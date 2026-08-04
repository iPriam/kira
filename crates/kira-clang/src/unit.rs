//! Loading libclang and parsing one translation unit with it.
//!
//! A [`Clang`] owns the loaded library and its index; a [`TranslationUnit`]
//! borrows it and owns the parsed unit. Both release their handle on drop, so
//! no caller can leak one, and the borrow is what keeps a cursor from
//! outliving the unit it points into.

use std::ffi::{CString, NulError, c_char};
use std::path::{Path, PathBuf};

use crate::api::{Api, CxIndex, CxTranslationUnit, DiagnosticSeverity, LoadError};
use crate::cursor::Cursor;

/// The libclang parse options Kira asks for.
///
/// `CXTranslationUnit_SkipFunctionBodies` (0x40) — a binding generator reads
/// declarations, and a header that inlines half its implementation should not
/// pay to have those bodies built. `CXTranslationUnit_DetailedPreprocessingRecord`
/// is deliberately *not* asked for: macro constants are not part of the
/// generated dialect, and the record is the expensive half of a parse.
const PARSE_OPTIONS: u32 = 0x40;

/// Why a header could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A path or compiler argument contained an interior NUL byte.
    #[error("`{text}` cannot be passed to the C parser: it contains a NUL byte")]
    NulInArgument {
        /// The offending text.
        text: String,
        /// The underlying conversion failure.
        #[source]
        source: NulError,
    },
    /// libclang refused to produce a translation unit at all.
    #[error("the C parser could not read `{path}` (libclang error {code})")]
    Unparsed {
        /// The header that could not be parsed.
        path: String,
        /// libclang's `CXErrorCode`.
        code: i32,
    },
    /// The header parsed with errors, so its declarations are not trustworthy.
    #[error("`{path}` does not compile as C:\n{diagnostics}")]
    Rejected {
        /// The header that did not compile.
        path: String,
        /// What clang said, one diagnostic per line.
        diagnostics: String,
    },
}

/// A loaded libclang, and the index every parse runs under.
pub struct Clang {
    api: Api,
    index: CxIndex,
    resource_dir: Option<PathBuf>,
}

impl Clang {
    /// Loads libclang from the LLVM installation rooted at `home`.
    pub fn load(home: &Path) -> Result<Self, LoadError> {
        let api = Api::load(home)?;
        let index = api.create_index();
        Ok(Self {
            api,
            index,
            resource_dir: resource_dir(home),
        })
    }

    /// Parses `header` with `arguments` as the compiler command line.
    ///
    /// `arguments` are clang driver arguments (`-I`, `-D`, `-target`, …); the
    /// header path itself is passed separately and must not be repeated.
    pub fn parse(
        &self,
        header: &Path,
        arguments: &[String],
    ) -> Result<TranslationUnit<'_>, ParseError> {
        let display = header.display().to_string();
        let path = c_string(&display)?;
        // The resource directory holds clang's own `stddef.h`, `stdarg.h`, and
        // the target macros every system header is written against. A driver
        // finds it beside its executable; libclang loaded into `kira` would
        // look beside `kira`, so it is named outright — without it, a header
        // that includes `<stddef.h>` does not compile and binds nothing.
        let mut owned: Vec<CString> = Vec::with_capacity(arguments.len() + 2);
        if let Some(resource_dir) = self.resource_dir.as_ref() {
            owned.push(c_string("-resource-dir")?);
            owned.push(c_string(&resource_dir.display().to_string())?);
        }
        for argument in arguments {
            owned.push(c_string(argument)?);
        }
        let pointers: Vec<*const c_char> = owned.iter().map(|argument| argument.as_ptr()).collect();

        let mut unit: CxTranslationUnit = std::ptr::null_mut();
        let count = i32::try_from(pointers.len()).unwrap_or(i32::MAX);
        // SAFETY: the index is live for as long as `self`, the path and every
        // argument are NUL-terminated and outlive the call, and `unit` is a
        // stack local of the right type.
        let code = unsafe {
            self.api.parse(
                self.index,
                path.as_ptr(),
                pointers.as_ptr(),
                count,
                PARSE_OPTIONS,
                &raw mut unit,
            )
        };
        if code != 0 || unit.is_null() {
            return Err(ParseError::Unparsed {
                path: display,
                code,
            });
        }
        let parsed = TranslationUnit {
            api: &self.api,
            unit,
        };

        // SAFETY: `unit` is live and owned by `parsed`.
        let diagnostics = unsafe { self.api.diagnostics(unit) };
        let errors: Vec<String> = diagnostics
            .into_iter()
            .filter(|(severity, _)| *severity >= DiagnosticSeverity::ERROR.0)
            .map(|(_, text)| text)
            .collect();
        if errors.is_empty() {
            return Ok(parsed);
        }
        Err(ParseError::Rejected {
            path: display,
            diagnostics: errors.join("\n"),
        })
    }
}

impl Drop for Clang {
    fn drop(&mut self) {
        // SAFETY: the index came from `Api::create_index` on this same `Api`
        // and nothing may use it after the owner is dropped.
        unsafe { self.api.dispose_index(self.index) }
        // Named so the field is visibly kept alive to here: every entry point
        // in `Api` points into this library.
        let _ = self.api.library();
    }
}

/// One parsed header, and the declarations reachable from it.
pub struct TranslationUnit<'a> {
    api: &'a Api,
    unit: CxTranslationUnit,
}

impl<'a> TranslationUnit<'a> {
    /// Every top-level declaration the unit holds, in declaration order.
    pub fn declarations(&self) -> Vec<Cursor<'a>> {
        // SAFETY: `unit` is live for as long as `self`.
        let root = unsafe { self.api.translation_unit_cursor(self.unit) };
        Cursor::new(root, self.api).children()
    }
}

impl Drop for TranslationUnit<'_> {
    fn drop(&mut self) {
        // SAFETY: the unit came from `Api::parse` on this same `Api` and
        // nothing may use it after the owner is dropped.
        unsafe { self.api.dispose_translation_unit(self.unit) }
    }
}

/// The clang resource directory inside an LLVM install, when it has one.
///
/// The layout is `lib/clang/<major>`, and a bundle ships exactly one major —
/// the one it was built from — so the single entry is the answer and two
/// entries mean an install this code has no way to choose between.
fn resource_dir(home: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(home.join("lib").join("clang"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("include").is_dir())
        .collect();
    found.sort();
    found.pop()
}

/// Converts one argument into the NUL-terminated form the C API takes.
fn c_string(text: &str) -> Result<CString, ParseError> {
    CString::new(text).map_err(|source| ParseError::NulInArgument {
        text: text.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CursorKind, TypeKind};

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "kira-clang-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            std::fs::create_dir_all(&base).expect("a scratch directory");
            TempDir(base)
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).expect("write a fixture");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The whole binding proved end to end against a real header: the library
    /// loads, a unit parses, and the structured answers libclang is here for —
    /// element counts and pointees — come back as values rather than spellings.
    #[test]
    fn a_header_parses_and_its_types_answer_structurally() {
        let llvm = kira_toolchain::discover(None).expect("the managed LLVM bundle");
        let clang = Clang::load(&llvm.home).expect("libclang loads out of the bundle");
        let dir = TempDir::new("parse");
        let header = dir.write(
            "sample.h",
            "struct Sample { int count; char pad[7]; };\n\
             int sample_take(struct Sample *s, const char *name);\n",
        );

        let unit = clang.parse(&header, &[]).expect("the header parses");
        let declarations = unit.declarations();

        let record = declarations
            .iter()
            .find(|cursor| cursor.kind() == CursorKind::STRUCT_DECL)
            .expect("the struct declaration");
        assert_eq!(record.name(), "Sample");
        let fields: Vec<String> = record.children().iter().map(Cursor::name).collect();
        assert_eq!(fields, vec!["count", "pad"]);
        let pad = record.children()[1].c_type();
        assert_eq!(pad.kind(), TypeKind::CONSTANT_ARRAY);
        assert_eq!(pad.array_size(), 7);

        let function = declarations
            .iter()
            .find(|cursor| cursor.kind() == CursorKind::FUNCTION_DECL)
            .expect("the function declaration");
        assert_eq!(function.name(), "sample_take");
        assert!(!function.is_variadic());
        assert_eq!(function.result_type().kind(), TypeKind::INT);
        let arguments = function.c_type().arguments();
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].kind(), TypeKind::POINTER);
        assert_eq!(arguments[1].pointee().canonical().kind(), TypeKind::CHAR_S);
        assert!(arguments[1].pointee().is_const());
    }

    /// A header that does not compile is a refusal carrying clang's own words,
    /// never a translation unit whose declarations are half-guesses.
    #[test]
    fn a_header_that_does_not_compile_is_refused_with_clangs_diagnostics() {
        let llvm = kira_toolchain::discover(None).expect("the managed LLVM bundle");
        let clang = Clang::load(&llvm.home).expect("libclang loads out of the bundle");
        let dir = TempDir::new("broken");
        let header = dir.write("broken.h", "#include \"nothing-is-here.h\"\n");

        let Err(error) = clang.parse(&header, &[]) else {
            panic!("expected a rejection, got a translation unit");
        };
        let ParseError::Rejected { diagnostics, .. } = &error else {
            panic!("expected a rejection, got {error}");
        };
        assert!(diagnostics.contains("nothing-is-here.h"), "{diagnostics}");
    }
}
