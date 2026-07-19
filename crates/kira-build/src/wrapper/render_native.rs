//! Rendering the generated crate for the **native engine**.
//!
//! Same public API as the VM engine's crate, different internals: instead of
//! embedding bytecode and calling `kira-main`, this one declares the library's
//! `kira_lib_*` trampolines in an `extern` block, links the static archive
//! through a `build.rs`, and marshals arguments into `BridgeValue`s.
//!
//! # The one place `unsafe` is honest
//!
//! The VM engine's crate forbids `unsafe` and means it. This one cannot: calling
//! a C symbol is `unsafe` by definition. So the generated code confines it — one
//! `unsafe` block per call, each with the invariant that makes it sound written
//! above it — and the crate denies `unsafe_op_in_unsafe_fn` so nothing hides
//! inside an `unsafe fn`'s body.
//!
//! # Who frees what, verbatim from `marshal.rs`
//!
//! - A **string argument** is allocated fresh from the *library's* allocator
//!   (`kira_rt_str_new`) and handed over. The callee frees its string arguments
//!   at return, so the wrapper never frees one it passed in. Freeing it here
//!   would be the double free that discipline exists to prevent.
//! - A **string result** is owned by the library. The wrapper reads the bytes out
//!   (`kira_rt_str_data`/`kira_rt_str_len`), copies them into a Rust `String`,
//!   and frees the handle with `kira_rt_str_free` from the same library. The
//!   library's heap is balanced before the call returns, and what the consumer
//!   holds is a plain Rust allocation.
//! - A **handle** is allocated by Kira and freed only by the generated
//!   destructor, which the Rust newtype's `Drop` calls exactly once.
//!
//! # Why there is no content hash here
//!
//! The VM engine checks data because it has no link step to fail. This one has
//! one, so the guard is a symbol: `load()` calls `kira_lib_<lib>_abi_1`, and an
//! archive built under a different export contract does not define it. The
//! failure is the consumer's link, naming the marker — which is the same lesson
//! `RUNTIME_ABI_VERSION` encodes, applied one level up.

use kira_bytecode::{ExportTable, ExportType};
use kira_llvm_backend::NativeExportSurface;

use crate::wrapper::naming::{library_ident, library_type_ident, type_ident, value_ident};
use crate::wrapper::render::{ClassModel, FnModel, ParamModel};
use crate::wrapper::{WrapperError, class_name_of};

/// Everything the native renderer needs, with every name already proven
/// spellable and every symbol already derived.
#[derive(Debug, Clone)]
pub(crate) struct NativeModel {
    /// The crate and module name.
    pub library: String,
    /// The Rust type name for the library handle.
    pub library_type: String,
    /// The marker symbol `load()` calls.
    pub marker: String,
    /// Every exported class, paired with its destructor's symbol.
    pub classes: Vec<(ClassModel, String)>,
    /// Every export, paired with its trampoline's symbol.
    pub functions: Vec<(FnModel, String)>,
}

impl NativeModel {
    /// Validates every name and pairs it with the symbol it was emitted under.
    pub(crate) fn build(
        library: &str,
        exports: &ExportTable,
        symbols: &NativeExportSurface,
    ) -> Result<NativeModel, WrapperError> {
        let library_ident = library_ident(library)?;
        let library_type = library_type_ident(library)?;
        let marker = symbols
            .abi_marker
            .clone()
            .ok_or(WrapperError::MissingSymbol {
                what: "the library's ABI marker",
            })?;

        let mut taken: Vec<(String, String)> = vec![(library_type.clone(), library.to_owned())];
        let mut classes = Vec::with_capacity(exports.classes.len());
        for (index, class) in exports.classes.iter().enumerate() {
            let rust = type_ident(class)?;
            if rust == crate::wrapper::render::HOST_PARAM {
                return Err(WrapperError::Reserved { name: rust });
            }
            if let Some((_, first)) = taken.iter().find(|(name, _)| *name == rust) {
                return Err(WrapperError::Collision {
                    rust,
                    first: first.clone(),
                    second: class.clone(),
                });
            }
            taken.push((rust.clone(), class.clone()));
            let symbol = symbols
                .classes
                .get(index)
                .map(|native| native.symbol.clone())
                .ok_or(WrapperError::MissingSymbol {
                    what: "an exported class's destructor",
                })?;
            classes.push((ClassModel { rust }, symbol));
        }

        let mut functions = Vec::with_capacity(exports.functions.len());
        for (index, export) in exports.functions.iter().enumerate() {
            let method = value_ident("an export", &export.name)?;
            let mut params = Vec::with_capacity(export.params.len());
            for (position, ty) in export.params.iter().enumerate() {
                check_class(*ty, exports)?;
                params.push(ParamModel {
                    name: format!("arg{position}"),
                    ty: *ty,
                });
            }
            check_class(export.result, exports)?;
            let symbol = symbols
                .functions
                .get(index)
                .map(|native| native.symbol.clone())
                .ok_or(WrapperError::MissingSymbol {
                    what: "an export's trampoline",
                })?;
            functions.push((
                FnModel {
                    method,
                    export: export.name.clone(),
                    kira_name: export.kira_name.clone(),
                    params,
                    result: export.result,
                },
                symbol,
            ));
        }

        Ok(NativeModel {
            library: library_ident,
            library_type,
            marker,
            classes,
            functions,
        })
    }

    /// Whether any export takes a string, so a string is lent *into* the
    /// library.
    fn lends_strings(&self) -> bool {
        self.functions.iter().any(|(function, _)| {
            function
                .params
                .iter()
                .any(|param| param.ty == ExportType::String)
        })
    }

    /// Whether any export returns a string, so a string is taken *out* of it.
    fn takes_strings(&self) -> bool {
        self.functions
            .iter()
            .any(|(function, _)| function.result == ExportType::String)
    }

    /// Whether any export takes no arguments, so the empty argument array is
    /// used.
    fn calls_without_arguments(&self) -> bool {
        self.functions
            .iter()
            .any(|(function, _)| function.params.is_empty())
    }

    /// The Rust type name of class `index`.
    fn class(&self, index: u32) -> String {
        class_name_of(
            self.classes.get(index as usize).map(|(class, _)| class),
            index,
        )
    }
}

/// Refuses a handle whose class index names nothing in the table.
fn check_class(ty: ExportType, table: &ExportTable) -> Result<(), WrapperError> {
    if let ExportType::Handle { class } = ty
        && table.classes.get(class as usize).is_none()
    {
        return Err(WrapperError::UnknownClass { class });
    }
    Ok(())
}

/// The Rust type a value of `ty` has going *into* the library.
fn param_type(model: &NativeModel, ty: ExportType) -> String {
    match ty {
        ExportType::Void => "()".to_owned(),
        ExportType::Int => "i64".to_owned(),
        ExportType::Float => "f64".to_owned(),
        ExportType::Bool => "bool".to_owned(),
        ExportType::String => "&str".to_owned(),
        ExportType::Handle { class } => format!("&{}", model.class(class)),
    }
}

/// The Rust type a value of `ty` has coming *out* of the library.
fn result_type(model: &NativeModel, ty: ExportType) -> String {
    match ty {
        ExportType::Void => "()".to_owned(),
        ExportType::Int => "i64".to_owned(),
        ExportType::Float => "f64".to_owned(),
        ExportType::Bool => "bool".to_owned(),
        ExportType::String => "String".to_owned(),
        ExportType::Handle { class } => model.class(class),
    }
}

/// A one-phrase name for what a result carries, for an error message.
fn describe_result(ty: ExportType) -> &'static str {
    match ty {
        ExportType::Void => "nothing",
        ExportType::Int => "an integer",
        ExportType::Float => "a float",
        ExportType::Bool => "a boolean",
        ExportType::String => "a string",
        ExportType::Handle { .. } => "a handle",
    }
}

/// Renders the whole of the generated `src/lib.rs` for the native engine.
pub(crate) fn lib_rs(model: &NativeModel) -> String {
    let mut out = String::new();
    out.push_str(&header(model));
    out.push_str(&externs(model));
    out.push_str(&marshalling(model));
    out.push_str(&library_struct(model));
    for (class, symbol) in &model.classes {
        out.push_str(&class_struct(model, class, symbol));
    }
    out
}

/// The leading comment, the imports, and the error type.
fn header(model: &NativeModel) -> String {
    format!(
        "// Generated by `kirac build --backend llvm` from the Kira library\n\
         // `{library}`.\n\
         //\n\
         // Do not edit. Every build of the library rewrites this file from the\n\
         // library's own `@Export` surface, so an edit here survives exactly until\n\
         // the next build.\n\
         //\n\
         // Engine: native. The library is compiled machine code in a static archive\n\
         // this crate links; `build.rs` points the linker at it. Calling a C symbol\n\
         // is unsafe by definition, so unlike the VM engine's crate this one\n\
         // contains `unsafe` — confined to one block per call, each with the\n\
         // invariant that makes it sound stated above it.\n\
         \n\
         use kira_runtime_abi::{{BridgeValue, BridgeValueTag}};\n\
         \n\
         /// What can go wrong loading or calling this library.\n\
         pub type Error = kira_main::Error;\n\
         \n\
         {no_args}",
        library = model.library,
        no_args = if model.calls_without_arguments() {
            "/// The number of `BridgeValue`s a call with no arguments passes.\n\
             ///\n\
             /// A trampoline never reads past its signature, so an empty argument\n\
             /// array is a valid pointer to zero elements — which is what a reference\n\
             /// to an empty array gives.\n\
             const NO_ARGS: [BridgeValue; 0] = [];\n\n"
        } else {
            ""
        },
    )
}

/// The `extern` block: every symbol this crate resolves out of the archive.
fn externs(model: &NativeModel) -> String {
    let mut out = String::from(
        "// The library's own surface, plus the string helpers that let a `&str`\n\
         // and a `String` cross. Every one of these resolves out of the static\n\
         // archive `build.rs` links; a missing one is a link failure naming it,\n\
         // which is the whole design of the guard.\n\
         unsafe extern \"C\" {\n",
    );
    out.push_str(&format!(
        "    /// The per-library ABI marker. Empty and free to call; its only job\n\
         \x20   /// is failing the link when this crate and the archive disagree.\n\
         \x20   fn {marker}();\n",
        marker = model.marker,
    ));
    for (function, symbol) in &model.functions {
        out.push_str(&format!(
            "    /// The trampoline for the Kira export `{kira}`.\n\
             \x20   fn {symbol}(args: *const BridgeValue, count: u32, out: *mut BridgeValue);\n",
            kira = function.kira_name,
            symbol = symbol,
        ));
    }
    for (class, symbol) in &model.classes {
        out.push_str(&format!(
            "    /// The synthesized destructor for `{rust}`.\n\
             \x20   fn {symbol}(args: *const BridgeValue, count: u32, out: *mut BridgeValue);\n",
            rust = class.rust,
            symbol = symbol,
        ));
    }
    // Declared only where used. An `extern` declaration nobody calls is a
    // `dead_code` warning in the consumer's build, against a file they did not
    // write — and a library whose surface never mentions a string has no reason
    // to name the string allocator at all.
    if model.lends_strings() {
        out.push_str(
            "    /// Allocates a string in the library's own heap, from borrowed bytes.\n\
             \x20   fn kira_rt_str_new(data: *const u8, len: usize) -> *mut core::ffi::c_void;\n",
        );
    }
    if model.takes_strings() {
        out.push_str(
            "    /// Releases a string the library allocated.\n\
             \x20   fn kira_rt_str_free(value: *mut core::ffi::c_void);\n\
             \x20   /// Borrows a string's bytes.\n\
             \x20   fn kira_rt_str_data(value: *mut core::ffi::c_void) -> *const u8;\n\
             \x20   /// How many bytes a string has.\n\
             \x20   fn kira_rt_str_len(value: *mut core::ffi::c_void) -> usize;\n",
        );
    }
    out.push_str("}\n\n");
    out
}

/// The marshalling helpers the generated methods share.
///
/// Each is emitted only if this library's surface reaches it: a helper nobody
/// calls is a `dead_code` warning in the consumer's build, against a file they
/// did not write. `result_slot` is unconditional — every call has a result slot,
/// including a `Void` one.
fn marshalling(model: &NativeModel) -> String {
    let mut out = String::new();
    if model.lends_strings() {
        out.push_str(
            "/// Lends `text` to the library for the duration of one call.\n\
         ///\n\
         /// The handle is allocated from the *library's* allocator and is not freed\n\
         /// here: the callee frees its string arguments at return. Freeing it on\n\
         /// this side would be the double free that discipline exists to prevent.\n\
         fn lend_str(text: &str) -> BridgeValue {\n\
         \x20   // SAFETY: `text`'s bytes are valid for the length passed, and\n\
         \x20   // `kira_rt_str_new` copies them rather than retaining the pointer.\n\
         \x20   let handle = unsafe { kira_rt_str_new(text.as_ptr(), text.len()) };\n\
         \x20   BridgeValue::new(BridgeValueTag::STRING, handle as u64)\n\
         }\n\
         \n",
        );
    }
    if model.takes_strings() {
        out.push_str(
            "/// Takes ownership of a string the library returned.\n\
         ///\n\
         /// Read the bytes, copy them into a Rust `String`, then free the library's\n\
         /// handle from the same library. The library's heap is balanced before this\n\
         /// returns, and what the caller holds is a plain Rust allocation.\n\
         fn take_str(payload: u64) -> String {\n\
         \x20   let handle = payload as *mut core::ffi::c_void;\n\
         \x20   if handle.is_null() {\n\
         \x20       return String::new();\n\
         \x20   }\n\
         \x20   // SAFETY: `handle` is a live string handle the library just returned\n\
         \x20   // and still owns; the bytes are read before it is freed, and it is\n\
         \x20   // freed exactly once.\n\
         \x20   let owned = unsafe {\n\
         \x20       let data = kira_rt_str_data(handle);\n\
         \x20       let len = kira_rt_str_len(handle);\n\
         \x20       let copied = if data.is_null() || len == 0 {\n\
         \x20           String::new()\n\
         \x20       } else {\n\
         \x20           String::from_utf8_lossy(core::slice::from_raw_parts(data, len)).into_owned()\n\
         \x20       };\n\
         \x20       kira_rt_str_free(handle);\n\
         \x20       copied\n\
         \x20   };\n\
         \x20   owned\n\
         }\n\
         \n",
        );
    }
    out.push_str(
        "/// The slot a trampoline writes its result into.\n\
         ///\n\
         /// Starts as `Void`, so a trampoline that took the allocation-failure path\n\
         /// and wrote nothing is read as a result the caller refuses by name rather\n\
         /// than as uninitialized memory.\n\
         fn result_slot() -> BridgeValue {\n\
         \x20   BridgeValue::new(BridgeValueTag::VOID, 0)\n\
         }\n\
         \n",
    );
    out
}

/// The library type, its `load`, and one method per export.
fn library_struct(model: &NativeModel) -> String {
    let mut out = format!(
        "/// The Kira library `{library}`, linked into this program.\n\
         ///\n\
         /// Zero-sized: the library is machine code in this binary, so there is\n\
         /// nothing to hold. It is still a value rather than a set of free\n\
         /// functions, so that a consumer's code reads identically against either\n\
         /// engine.\n\
         ///\n\
         /// Neither `Send` nor `Sync`: one library belongs to one thread, matching\n\
         /// the VM engine's contract and the session rule the seam already has.\n\
         #[derive(Debug, Clone)]\n\
         pub struct {ty} {{\n\
         \x20   /// Makes the type unconstructible from outside and `!Send`/`!Sync`\n\
         \x20   /// without a `PhantomData` incantation a reader has to decode.\n\
         \x20   _thread: core::marker::PhantomData<*const ()>,\n\
         }}\n\
         \n\
         impl {ty} {{\n\
         \x20   /// Loads the linked library.\n\
         \x20   ///\n\
         \x20   /// There is nothing to load — the code is already in this binary —\n\
         \x20   /// so what this really does is *reference the ABI marker*, which is\n\
         \x20   /// how a stale archive is caught. That check already happened, at\n\
         \x20   /// link time, by name: if the archive this crate was generated for\n\
         \x20   /// had been replaced by one built under a different export contract,\n\
         \x20   /// this program would not have linked.\n\
         \x20   ///\n\
         \x20   /// The `Result` is kept so a consumer's code is identical against\n\
         \x20   /// either engine, where the VM's `load()` genuinely can fail.\n\
         \x20   pub fn load() -> Result<{ty}, Error> {{\n\
         \x20       // SAFETY: the marker is an empty, argument-free function this\n\
         \x20       // library defines; calling it has no effect and cannot fail.\n\
         \x20       unsafe {{ {marker}() }};\n\
         \x20       Ok({ty} {{\n\
         \x20           _thread: core::marker::PhantomData,\n\
         \x20       }})\n\
         \x20   }}\n",
        library = model.library,
        ty = model.library_type,
        marker = model.marker,
    );
    for (function, symbol) in &model.functions {
        out.push('\n');
        out.push_str(&method(model, function, symbol));
    }
    out.push_str("}\n");
    out
}

/// One export's method.
fn method(model: &NativeModel, function: &FnModel, symbol: &str) -> String {
    let signature: Vec<String> = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, param_type(model, param.ty)))
        .collect();
    let kira_signature: Vec<String> = function
        .params
        .iter()
        .map(|param| result_type(model, param.ty))
        .collect();

    let mut call = String::new();
    call.push_str("        let mut out = result_slot();\n");
    if function.params.is_empty() {
        call.push_str("        let args = NO_ARGS;\n");
    } else {
        call.push_str("        let args = [\n");
        for param in &function.params {
            call.push_str(&format!("            {},\n", pack(param)));
        }
        call.push_str("        ];\n");
    }
    call.push_str(
        "        // SAFETY: `args` is a live array of exactly `count` values packed\n\
         \x20       // from this export's own signature, and `out` is a live writable\n\
         \x20       // slot. The trampoline reads no further than the signature it was\n\
         \x20       // generated from — the same one this crate was generated from.\n",
    );
    call.push_str(&format!(
        "        unsafe {{ {symbol}(args.as_ptr(), args.len() as u32, &mut out) }};\n",
    ));

    let lift = match function.result {
        ExportType::Void => "            BridgeValueTag::VOID => Ok(()),\n".to_owned(),
        ExportType::Int => {
            "            BridgeValueTag::INT => Ok(out.payload as i64),\n".to_owned()
        }
        ExportType::Float => {
            "            BridgeValueTag::FLOAT => Ok(f64::from_bits(out.payload)),\n".to_owned()
        }
        ExportType::Bool => {
            "            BridgeValueTag::BOOL => Ok(out.payload != 0),\n".to_owned()
        }
        ExportType::String => {
            "            BridgeValueTag::STRING => Ok(take_str(out.payload)),\n".to_owned()
        }
        ExportType::Handle { class } => format!(
            "            BridgeValueTag::HANDLE => Ok({} {{\n\
             \x20               handle: out.payload,\n\
             \x20           }}),\n",
            model.class(class),
        ),
    };

    format!(
        "    /// Calls the Kira export `{kira_name}({kira_params}) -> {kira_result}`.\n\
         \x20   ///\n\
         \x20   /// Arguments are lent to the library for the duration of the call; the\n\
         \x20   /// result is owned by the caller.\n\
         \x20   pub fn {method}(&self{comma}{signature}) -> Result<{returns}, Error> {{\n\
         {call}\
         \x20       match out.tag {{\n\
         {lift}\
         \x20           _ => Err(Error::unexpected_tag(\"{export}\", \"{expected}\", out.tag)),\n\
         \x20       }}\n\
         \x20   }}\n",
        kira_name = function.kira_name,
        kira_params = kira_signature.join(", "),
        kira_result = result_type(model, function.result),
        method = function.method,
        comma = if signature.is_empty() { "" } else { ", " },
        signature = signature.join(", "),
        returns = result_type(model, function.result),
        call = call,
        lift = lift,
        export = function.export,
        expected = describe_result(function.result),
    )
}

/// How one argument is packed into a `BridgeValue`.
fn pack(param: &ParamModel) -> String {
    match param.ty {
        ExportType::Void => "BridgeValue::new(BridgeValueTag::VOID, 0)".to_owned(),
        ExportType::Int => format!(
            "BridgeValue::new(BridgeValueTag::INT, {} as u64)",
            param.name
        ),
        ExportType::Float => format!(
            "BridgeValue::new(BridgeValueTag::FLOAT, {}.to_bits())",
            param.name
        ),
        ExportType::Bool => format!(
            "BridgeValue::new(BridgeValueTag::BOOL, u64::from({}))",
            param.name
        ),
        ExportType::String => format!("lend_str({})", param.name),
        ExportType::Handle { .. } => format!(
            "BridgeValue::new(BridgeValueTag::HANDLE, {}.handle)",
            param.name
        ),
    }
}

/// One exported class's Rust newtype and its destructor.
fn class_struct(model: &NativeModel, class: &ClassModel, symbol: &str) -> String {
    format!(
        "\n\
         /// A live `{rust}` inside the library.\n\
         ///\n\
         /// Owned: dropping it calls the library's synthesized destructor, which\n\
         /// releases the Kira object and frees the box holding it. Nothing else ever\n\
         /// does. Use-after-free is not expressible — every method borrows the handle\n\
         /// and `Drop` consumes it — and the word inside is opaque, so there is\n\
         /// nothing in it to compute with.\n\
         #[derive(Debug)]\n\
         pub struct {rust} {{\n\
         \x20   /// The word the library handed out; opaque to this side.\n\
         \x20   handle: u64,\n\
         }}\n\
         \n\
         impl {rust} {{\n\
         \x20   /// The library instance this handle belongs to.\n\
         \x20   pub fn library(&self) -> {library_ty} {{\n\
         \x20       {library_ty} {{\n\
         \x20           _thread: core::marker::PhantomData,\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n\
         \n\
         impl Drop for {rust} {{\n\
         \x20   /// Releases the Kira object this handle names.\n\
         \x20   fn drop(&mut self) {{\n\
         \x20       let args = [BridgeValue::new(BridgeValueTag::HANDLE, self.handle)];\n\
         \x20       let mut out = result_slot();\n\
         \x20       // SAFETY: `self.handle` came from this library and has not been\n\
         \x20       // released — `Drop` runs once per value, and the field is private,\n\
         \x20       // so no other code can have released it. The destructor treats a\n\
         \x20       // null handle as nothing to do.\n\
         \x20       unsafe {{ {symbol}(args.as_ptr(), args.len() as u32, &mut out) }};\n\
         \x20   }}\n\
         }}\n",
        rust = class.rust,
        library_ty = model.library_type,
        symbol = symbol,
    )
}

/// Renders the generated crate's `build.rs`.
///
/// Two lines of cargo directives and the platform libraries the Rust
/// `staticlib` inside the archive needs. The list is not written here: it is
/// rendered from [`kira_llvm_backend::PLATFORM_LINK_LISTS`], the same data this
/// compiler's own linker path uses for a native executable. The same archive
/// needs the same libraries, and a second copy of the list would drift from the
/// first the day one of them gains a library.
pub(crate) fn build_rs(model: &NativeModel, archive_directory: &str) -> String {
    format!(
        "//! Points the linker at the Kira library `{library}`.\n\
         //!\n\
         //! Generated by `kirac build --backend llvm`. Do not edit.\n\
         //!\n\
         //! The archive is self-contained: it carries this library's compiled Kira\n\
         //! code *and* the Kira native runtime's members, so exactly one\n\
         //! `-l` is needed. What it does not carry is the platform libraries the\n\
         //! Rust standard library inside it calls into, which is what the rest of\n\
         //! this file supplies.\n\
         \n\
         fn main() {{\n\
         \x20   println!(\"cargo:rustc-link-search=native={directory}\");\n\
         \x20   println!(\"cargo:rustc-link-lib=static={library}\");\n\
         \x20   println!(\"cargo:rerun-if-changed={directory}/lib{library}.a\");\n\
         \x20   for library in platform_libraries() {{\n\
         \x20       println!(\"cargo:rustc-link-lib={{library}}\");\n\
         \x20   }}\n\
         \x20   for framework in platform_frameworks() {{\n\
         \x20       println!(\"cargo:rustc-link-lib=framework={{framework}}\");\n\
         \x20   }}\n\
         }}\n\
         \n\
         /// The system libraries the Rust `staticlib` inside the archive needs.\n\
         fn platform_libraries() -> &'static [&'static str] {{\n\
         {libraries}\
         }}\n\
         \n\
         /// The frameworks it needs, which only Apple platforms have.\n\
         fn platform_frameworks() -> &'static [&'static str] {{\n\
         {frameworks}\
         }}\n",
        library = model.library,
        directory = archive_directory,
        libraries = platform_branches(|list| list.libraries),
        frameworks = platform_branches(|list| list.frameworks),
    )
}

/// Renders the `cfg!(target_os = ...)` chain one of the generated `build.rs`
/// list functions is made of.
///
/// A chain rather than the host's own list, because the generated crate decides
/// at *its* compile time what it is building for, exactly as the hand-written
/// version it replaces did.
fn platform_branches(
    select: impl Fn(&kira_llvm_backend::PlatformLinkList) -> &'static [&'static str],
) -> String {
    let mut out = String::new();
    for list in kira_llvm_backend::PLATFORM_LINK_LISTS {
        let names = select(list);
        if names.is_empty() {
            continue;
        }
        let keyword = if out.is_empty() { "if" } else { "} else if" };
        let entries: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
        out.push_str(&format!(
            "    {keyword} cfg!(target_os = \"{os}\") {{\n        &[{entries}]\n",
            os = list.target_os,
            entries = entries.join(", "),
        ));
    }
    if out.is_empty() {
        out.push_str("    &[]\n");
    } else {
        out.push_str("    } else {\n        &[]\n    }\n");
    }
    out
}
