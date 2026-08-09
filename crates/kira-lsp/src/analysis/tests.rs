use super::*;
use kira_diagnostics::Severity;

/// Analyzes one document through a session of its own.
///
/// A fresh session per case, so no test can be reading another's analysis;
/// [`a_session_analyzes_each_document_on_its_own_terms`] is where reuse
/// across documents is asserted on deliberately.
fn analyze(path: &str, text: &str) -> Analysis {
    super::analyze(&mut AnalysisSession::new(), path, text)
}

/// A scratch directory that removes itself, so a failing test leaves no litter
/// and no test depends on another's leftovers.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "kira-lsp-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("a scratch directory");
        TempDir(base)
    }

    fn write(&self, name: &str, text: &str) -> std::path::PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture directories");
        }
        std::fs::write(&path, text).expect("write a fixture");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The document being edited is rarely a lone file, and this is the shape that
/// used to break: an app package whose entry sits in `app/`, with its
/// `package.kira` a directory above and its libraries reached through that
/// manifest. The server walked up to nothing, so every dependency import was
/// reported as an unresolved library while `kira check` on the same tree was
/// clean.
#[test]
fn a_package_document_resolves_the_libraries_its_manifest_declares() {
    let tree = TempDir::new("package-deps");
    tree.write(
        "editor/package.kira",
        "Package Editor {\n    let kind = .App\n    \
         let dependencies = [Dependency { name: \"Core\", path: \"../core\" }]\n}\n",
    );
    tree.write(
        "core/package.kira",
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    tree.write(
        "core/app/Core.kira",
        "function coreValue() -> Int { return 19 }",
    );
    let entry = tree.write("editor/app/main.kira", "");
    let path = entry.to_string_lossy().into_owned();

    let analysis = analyze(
        &path,
        "import Core\n@Main function main() { print(coreValue()) return }",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "a declared dependency resolves in the editor: {:?}",
        analysis.diagnostics
    );
}

/// A package's own files are one program, whether or not the entry imports
/// them. The editor must see a sibling under `app/` the same way the compiler
/// does — an undefined-name squiggle on a name the build resolves is the
/// disagreement this server exists to prevent.
#[test]
fn a_package_document_sees_its_sibling_sources() {
    let tree = TempDir::new("package-siblings");
    tree.write(
        "editor/package.kira",
        "Package Editor {\n    let kind = .App\n}\n",
    );
    tree.write(
        "editor/app/area/Thing.kira",
        "function siblingValue() -> Int { return 7 }",
    );
    let entry = tree.write("editor/app/main.kira", "");
    let path = entry.to_string_lossy().into_owned();

    let analysis = analyze(
        &path,
        "@Main function main() { print(siblingValue()) return }",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "a sibling package source is part of the program: {:?}",
        analysis.diagnostics
    );
}

/// A library package says so in its own manifest, so the server stops
/// demanding an entrypoint from a file that was never meant to have one.
#[test]
fn a_library_package_document_is_not_asked_for_an_entrypoint() {
    let tree = TempDir::new("library-kind");
    tree.write(
        "core/package.kira",
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    let entry = tree.write("core/app/Core.kira", "");
    let path = entry.to_string_lossy().into_owned();

    let analysis = analyze(&path, "function coreValue() -> Int { return 19 }");
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM011")),
        "a library is not missing `@Main`: {:?}",
        analysis.diagnostics
    );

    // And a file with no manifest above it is still an application, so the
    // missing-`@Main` error stays where it belongs for a bare document.
    let bare = tree.write("loose.kira", "");
    let analysis = analyze(
        &bare.to_string_lossy(),
        "function coreValue() -> Int { return 19 }",
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM011")),
        "a bare document is an application: {:?}",
        analysis.diagnostics
    );
}

/// A retained session must not let one document's declarations reach the
/// next. Analyzing a program that declares `f`, then one that calls `f`
/// without declaring it, has to report the call as unresolved — a stale
/// scope would silently accept it.
#[test]
fn a_session_analyzes_each_document_on_its_own_terms() {
    let mut session = AnalysisSession::new();
    let first = super::analyze(
        &mut session,
        "t.kira",
        "function f() -> Int { return 1 }\n@Main function main() { print(f()) return }",
    );
    assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);

    let second = super::analyze(
        &mut session,
        "t.kira",
        "@Main function main() { print(f()) return }",
    );
    assert!(
        second
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM061")),
        "the previous document's `f` must not still be in scope: {:?}",
        second.diagnostics
    );

    // And analyzing the first program again through the same session still
    // answers cleanly, so nothing was poisoned by the second.
    let again = super::analyze(
        &mut session,
        "t.kira",
        "function f() -> Int { return 1 }\n@Main function main() { print(f()) return }",
    );
    assert!(again.diagnostics.is_empty(), "{:?}", again.diagnostics);
}

/// New syntax reaches the editor for free, and this is what proves it: the
/// server runs the compiler's own `analyzed` query, so a `for` loop it has
/// never heard of squiggles exactly when `kira check` says it should.
#[test]
fn loop_syntax_analyzes_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "@Main function main() { for i in 0..3 { if i > 1 { break } continue } return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let stray = analyze("t.kira", "@Main function main() { break return }");
    let diagnostic = stray
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM041"))
        .expect("a `break` outside a loop is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// Classes reach the editor for the same reason every other construct did:
/// the server serves the compiler's own `analyzed` query. Inheritance,
/// `override`, and parent qualification all resolve here without the LSP
/// learning that classes exist — and an ambiguous inherited member still
/// squiggles.
#[test]
fn class_syntax_analyzes_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "class Account { var balance: Int = 100\n let rate: Int = 2\n \
           function gross() -> Int { return self.balance * self.rate } }\n\
         class Savings extends Account { override let rate = 5\n \
           function bonus() -> Int { return Account.gross() + self.balance } }\n\
         @Main function main() { print(Savings().bonus()) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let ambiguous = analyze(
        "t.kira",
        "class Left { let v: Int = 1 }\nclass Right { let v: Int = 2 }\n\
         class Child extends Left, Right { function read() -> Int { return v } }\n\
         @Main function main() { return }",
    );
    let diagnostic = ambiguous
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM068"))
        .expect("an ambiguous inherited field is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// Ownership reaches the editor for the same reason `for` did: the server
/// serves the compiler's own `analyzed` query, so a use-after-move
/// squiggles in an editor without the LSP learning that `move` exists.
#[test]
fn ownership_diagnostics_reach_the_editor() {
    let clean = analyze(
        "t.kira",
        "struct Mesh { let id: Int }\n\
         function consume(mesh: Mesh) -> Int { return mesh.id }\n\
         @Main function main() { let mesh = Mesh { id: 1 } print(consume(move mesh)) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let after_move = analyze(
        "t.kira",
        "struct Mesh { let id: Int }\n\
         function consume(mesh: Mesh) -> Int { return mesh.id }\n\
         @Main function main() { let mesh = Mesh { id: 1 } \
         print(consume(move mesh)) print(mesh.id) return }",
    );
    let diagnostic = after_move
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM107"))
        .expect("using a moved value is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// Arrays reach the editor the same way `for` and ownership did: the server
/// serves the compiler's own `analyzed` query, so an array literal, an index,
/// `.append`, `.count`, and `for`-in all analyze without the LSP learning any
/// of them exist — and an unsupported array method still squiggles.
#[test]
fn array_syntax_analyzes_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "@Main function main() { var xs: [Int] = [] xs.append(1) xs[0] = 2 \
         for x in xs { print(x) } print(xs.count) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let bad = analyze(
        "t.kira",
        "@Main function main() { let xs = [1, 2] print(xs.reverse()) return }",
    );
    let diagnostic = bad
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM101"))
        .expect("an unsupported array method is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// The conditional expression and the bitwise operators reach the editor
/// the same way everything before them did — six new tokens and a new
/// expression node, and the LSP learns none of them.
///
/// The bad case is chosen for the rung of the precedence ladder that is
/// most often misremembered: `flags & 8 == 8` groups as `flags & (8 == 8)`,
/// as it does in C but not in Go or Swift, so an editor squiggles it rather
/// than silently accepting the tighter-`&` reading.
#[test]
fn conditional_and_bitwise_syntax_analyze_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "@Main function main() { let flags = 6 print((flags & 2) == 2 ? flags << 1 : ~flags) \
         print(flags | 1) print(flags ^ 3) print(flags >> 1) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let mis_grouped = analyze(
        "t.kira",
        "@Main function main() { let flags = 6 print(flags & 8 == 8) return }",
    );
    let diagnostic = mis_grouped
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM071"))
        .expect("`&` against a `Bool` is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");

    let bad_condition = analyze(
        "t.kira",
        "@Main function main() { print(1 ? 2 : 3) return }",
    );
    assert!(
        bad_condition
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM131")),
        "a non-boolean condition is reported"
    );
}

/// `attempt`/`try`/`handle` needs no LSP wiring, and this says so.
///
/// The construct reaches the editor by construction: the server serves
/// diagnostics from the same salsa `analyzed` query `kira check` uses, so
/// new syntax is understood the moment analysis understands it. A clean
/// `attempt` produces no diagnostics, and each of its own checks squiggles
/// with a span an editor can render.
#[test]
fn attempt_and_try_analyze_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "enum E { A }\n\
         enum O { Ok: Int Error: E }\n\
         function f() -> O { return .Ok(1) }\n\
         function g() -> Int { attempt { let v = try f() return v } \
         handle { A { return 0 } } }\n\
         @Main function main() { print(g()) return }",
    );
    assert!(
        clean.diagnostics.is_empty(),
        "a well-formed attempt is clean: {:?}",
        clean.diagnostics
    );

    let stray = analyze(
        "t.kira",
        "enum E { A }\n\
         enum O { Ok: Int Error: E }\n\
         function f() -> O { return .Ok(1) }\n\
         @Main function main() { let v = try f() print(v) return }",
    );
    let diagnostic = stray
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM137"))
        .expect("`try` outside an `attempt` is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// Fixed-width spellings need no LSP wiring either, and this says so.
///
/// A width is resolved by the same `analyzed` query the compiler uses, so
/// `U8` and `F32` type-check in the editor and a mismatch between two
/// written widths squiggles — with the diagnostic naming both widths rather
/// than calling them both "Int", which is the part an editor user actually
/// reads.
#[test]
fn fixed_width_types_analyze_the_way_the_compiler_does() {
    let clean = analyze(
        "t.kira",
        "@Main function main() { let a: U8 = 5 let b: U64 = 6 let c: F32 = 1.5 \
         print(a + 1) print(b + 1) print(c + 0.5) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let bad = analyze(
        "t.kira",
        "@Main function main() { let a: U8 = 1 let b: U32 = a return }",
    );
    let diagnostic = bad
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM020"))
        .expect("a width mismatch is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
    assert!(
        diagnostic.message.contains("U8") && diagnostic.message.contains("U32"),
        "the message names both widths: {}",
        diagnostic.message
    );
}

#[test]
fn enum_syntax_analyzes_the_way_the_compiler_does() {
    // New syntax reaches the editor by construction: the LSP serves the same
    // `analyzed` query the compiler does, so an enum program checks clean and
    // an unknown variant is squiggled — no LSP-specific wiring required.
    let clean = analyze(
        "t.kira",
        "enum Color { Red Green Blue }\n\
         function rank(c: Color) -> Int { if c == .Red { return 1 } return 2 }\n\
         @Main function main() { print(rank(.Green)) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let bad = analyze(
        "t.kira",
        "enum Color { Red Green }\n\
         @Main function main() { let c: Color = .Purple return }",
    );
    let diagnostic = bad
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM120"))
        .expect("an unknown variant is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

#[test]
fn match_syntax_analyzes_the_way_the_compiler_does() {
    // Same claim as the enum test, for the construct built on top of it: no
    // LSP-specific wiring, so a `match` checks clean here and a
    // non-exhaustive one squiggles, because both come off the one
    // `analyzed` query.
    let clean = analyze(
        "t.kira",
        "enum Shade { Light Mid Dark }\n\
         function rank(s: borrow Shade) -> Int { match s { Light -> return 1; \
         Mid -> return 2; Dark -> return 3; } }\n\
         @Main function main() { let d: Shade = .Dark print(rank(d)) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let bad = analyze(
        "t.kira",
        "enum Shade { Light Mid Dark }\n\
         @Main function main() { let s: Shade = .Mid\n\
         match s { Light -> { print(1) } Mid -> { print(2) } } return }",
    );
    let diagnostic = bad
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM129"))
        .expect("a non-exhaustive match is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

/// Imports are the first feature that makes the server multi-file, and
/// this is where that stops being a claim.
///
/// The server analyzes the document as the *entry* file of a program and
/// reads the modules it imports off disk, so a name that only a sibling
/// module declares resolves in the editor exactly as it does for
/// `kira check` — and an import that names no file squiggles.
#[test]
fn imports_analyze_the_way_the_compiler_does() {
    let directory = std::env::temp_dir().join(format!(
        "kira_lsp_imports_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("temp dir");
    std::fs::write(
        directory.join("support.kira"),
        "function supportValue() -> Int { return 42 }",
    )
    .expect("write module");
    let entry = directory.join("main.kira");
    let path = entry.to_string_lossy().into_owned();

    let clean = analyze(
        &path,
        "import support as Support\n\
         @Main function main() { print(Support.supportValue()) return }",
    );
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    let missing = analyze(
        &path,
        "import nowhere\n@Main function main() { print(1) return }",
    );
    let diagnostic = missing
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM032"))
        .expect("an unresolved import is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");

    // A file-scoped rule is what makes this the editor's problem too: the
    // sibling imports nothing, so its qualified reference does not resolve
    // — and the squiggle belongs to the sibling, not to this document.
    std::fs::write(
        directory.join("leak.kira"),
        "function leakValue() -> Int { return support.supportValue() }",
    )
    .expect("write module");
    let leaking = analyze(
        &path,
        "import support\nimport leak\n@Main function main() { print(leakValue()) return }",
    );
    assert!(
        leaking.diagnostics.is_empty(),
        "a sibling module's error is not pinned onto this document: {:?}",
        leaking.diagnostics
    );

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_clean_program_has_no_diagnostics() {
    let analysis = analyze("t.kira", "@Main function main() { print(1) return }");
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics,
    );
}

/// The server must surface what the compiler surfaces — this is the same
/// code the CLI's `check` reports, reached through the same query.
#[test]
fn an_unknown_name_is_reported_with_its_code() {
    let analysis = analyze("t.kira", "@Main function main() { print(missing) return }");
    let diagnostic = analysis
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM060"))
        .expect("an unknown name is reported");
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
}

#[test]
fn a_program_without_main_is_reported() {
    let analysis = analyze("t.kira", "function f() { return }");
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM011")),
        "{:?}",
        analysis.diagnostics,
    );
}

/// The parser is error-resilient, so a broken program still analyzes and
/// still reports — an editor must not go blank on a half-typed line.
#[test]
fn a_syntactically_broken_program_still_reports_rather_than_bailing() {
    let analysis = analyze("t.kira", "@Main function main( {");
    assert!(!analysis.diagnostics.is_empty());
}
