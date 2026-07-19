//! Rendering the generated `src/lib.rs`.
//!
//! The shape is fixed and small, so this is string assembly rather than a
//! syntax tree: one `pub struct` for the library, one `pub struct` per exported
//! class, one method per export, and a `const CONTRACT` naming the surface it
//! was all generated from.
//!
//! # Why the imports are computed rather than fixed
//!
//! A library that exports nothing, or exports only nullary functions, would get
//! an unused import if the header were a constant — and the consumer builds this
//! file under `-D warnings`. So the header is derived from what the body
//! actually uses. That is not a nicety: an unused import in generated code is a
//! build failure in somebody else's crate, reported against a file they did not
//! write.
//!
//! # Why every generated type carries a host parameter
//!
//! The VM is a portable core: it formats `print` into finished lines and hands
//! them to a `HostCapabilities` the embedder supplies. A wrapper that only ever
//! built a `StdoutHost` would make that choice for the consumer — a Rust program
//! embedding a UI library could not put the library's output in a log, a test
//! buffer, or a browser console. So the library type and every handle newtype
//! are generic over the host, with `StdoutHost` as the default type parameter:
//! `load()` reads exactly as it did and `load_with(host)` is the door. Generic
//! rather than boxed, matching `kira-main`, so an embedder reads its own host
//! back afterwards.
//!
//! The parameter is spelled `H`, which is therefore a name no exported class may
//! take: [`Model::build`] refuses one by name rather than emitting a file whose
//! `impl<H> Drop for H<H>` does not compile.

use kira_bytecode::{ExportTable, ExportType};

use crate::wrapper::naming::{library_ident, library_type_ident, type_ident, value_ident};
use crate::wrapper::{WrapperError, WrapperSpec, artifact_file_name};

/// The name the generated code gives its host type parameter.
///
/// Public to this module only: it appears in the rendered signatures and in the
/// one refusal that keeps a class from claiming it.
pub(crate) const HOST_PARAM: &str = "H";

/// The host parameter as it is applied to a generated type: `Button<H>`.
const HOST_ARG: &str = "<H>";

/// The generic header the generated types and impls are written under.
const HOST_BOUND: &str = "H: HostCapabilities";

/// One exported class, as Rust sees it.
#[derive(Debug, Clone)]
pub(crate) struct ClassModel {
    /// The Rust newtype's name.
    pub rust: String,
}

/// One parameter of one export, as Rust sees it.
#[derive(Debug, Clone)]
pub(crate) struct ParamModel {
    /// The Rust binding's name.
    pub name: String,
    /// What it carries.
    pub ty: ExportType,
}

/// One export, as Rust sees it.
#[derive(Debug, Clone)]
pub(crate) struct FnModel {
    /// The Rust method's name, raw-escaped if it had to be.
    pub method: String,
    /// The name the export is called by at the seam.
    pub export: String,
    /// The name the Kira author wrote, for the doc comment.
    pub kira_name: String,
    /// The parameters, in order.
    pub params: Vec<ParamModel>,
    /// What it returns.
    pub result: ExportType,
}

/// Everything the renderer needs, with every name already proven spellable.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    /// The crate and module name.
    pub library: String,
    /// The Rust type name for the library handle.
    pub library_type: String,
    /// The embedded artifact's file name.
    pub artifact: String,
    /// Every exported class, in the order handle types index them.
    pub classes: Vec<ClassModel>,
    /// Every export a method is generated for.
    pub functions: Vec<FnModel>,
    /// The hash of the artifact this wrapper was generated from.
    pub content_hash: u64,
}

impl Model {
    /// Validates every name in `spec` and resolves it to its Rust spelling.
    ///
    /// All the refusing happens here, before a single character is rendered, so
    /// a failure names the offending Kira declaration rather than pointing at a
    /// generated file that would not compile.
    pub(crate) fn build(spec: &WrapperSpec<'_>) -> Result<Model, WrapperError> {
        let library = library_ident(spec.library)?;
        let library_type = library_type_ident(spec.library)?;

        let mut taken: Vec<(String, String)> =
            vec![(library_type.clone(), spec.library.to_owned())];
        let mut classes = Vec::with_capacity(spec.exports.classes.len());
        for class in &spec.exports.classes {
            let rust = type_ident(class)?;
            if rust == HOST_PARAM {
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
            classes.push(ClassModel { rust });
        }

        let functions = spec
            .exports
            .functions
            .iter()
            .map(|export| build_function(export, spec.exports))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Model {
            library,
            library_type,
            artifact: artifact_file_name(spec.library),
            classes,
            functions,
            content_hash: spec.content_hash,
        })
    }
}

/// Resolves one export's names and checks every handle names a real class.
fn build_function(
    export: &kira_bytecode::ModuleExport,
    table: &ExportTable,
) -> Result<FnModel, WrapperError> {
    let method = value_ident("an export", &export.name)?;
    let mut params = Vec::with_capacity(export.params.len());
    for (position, ty) in export.params.iter().enumerate() {
        check_class(*ty, table)?;
        params.push(ParamModel {
            // Positional, because the export table carries no parameter names:
            // it is a wire format, and names in it would be a layout change
            // rather than an append. The doc comment carries the Kira signature
            // instead, which is where a reader looks anyway.
            name: format!("arg{position}"),
            ty: *ty,
        });
    }
    check_class(export.result, table)?;
    Ok(FnModel {
        method,
        export: export.name.clone(),
        kira_name: export.kira_name.clone(),
        params,
        result: export.result,
    })
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

/// The Rust type a value of `ty` has when it is passed *into* the library.
///
/// Strings and handles are borrowed: the boundary contract lends an argument
/// and moves a result, and this is that sentence in Rust's type system.
///
/// `host` is what a handle type carries after its name — [`HOST_ARG`] where the
/// spelling has to compile, and empty in a doc comment, where the parameter is
/// noise in front of the Kira signature a reader came for.
fn param_type(model: &Model, ty: ExportType, host: &str) -> String {
    match ty {
        ExportType::Void => "()".to_owned(),
        ExportType::Int => "i64".to_owned(),
        ExportType::Float => "f64".to_owned(),
        ExportType::Bool => "bool".to_owned(),
        ExportType::String => "&str".to_owned(),
        ExportType::Handle { class } => format!("&{}{host}", class_name(model, class)),
    }
}

/// The Rust type a value of `ty` has when it comes *out* of the library.
///
/// `host` reads as it does for [`param_type`].
fn result_type(model: &Model, ty: ExportType, host: &str) -> String {
    match ty {
        ExportType::Void => "()".to_owned(),
        ExportType::Int => "i64".to_owned(),
        ExportType::Float => "f64".to_owned(),
        ExportType::Bool => "bool".to_owned(),
        ExportType::String => "String".to_owned(),
        ExportType::Handle { class } => format!("{}{host}", class_name(model, class)),
    }
}

/// The Rust name of class `index`, which [`Model::build`] proved exists.
fn class_name(model: &Model, index: u32) -> String {
    crate::wrapper::class_name_of(model.classes.get(index as usize), index)
}

/// How `ty` is spelled inside the `const CONTRACT`.
fn contract_type(ty: ExportType) -> String {
    match ty {
        ExportType::Void => "ExportType::Void".to_owned(),
        ExportType::Int => "ExportType::Int".to_owned(),
        ExportType::Float => "ExportType::Float".to_owned(),
        ExportType::Bool => "ExportType::Bool".to_owned(),
        ExportType::String => "ExportType::String".to_owned(),
        ExportType::Handle { class } => format!("ExportType::Handle {{ class: {class} }}"),
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

/// Renders the whole of the generated `src/lib.rs`.
pub(crate) fn lib_rs(model: &Model) -> String {
    let mut out = String::new();
    out.push_str(&header(model));
    out.push_str(&contract(model));
    out.push_str(&library_struct(model));
    for class in &model.classes {
        out.push_str(&class_struct(model, class));
    }
    out
}

/// The leading comment, the imports, and the embedded artifact.
///
/// Plain `//` rather than `//!`: see the module docs on this file being
/// `include!`able.
fn header(model: &Model) -> String {
    let mut uses = vec![
        "use std::cell::RefCell;".to_owned(),
        "use std::rc::Rc;".to_owned(),
        String::new(),
    ];
    if !model.functions.is_empty() {
        uses.push("use kira_bytecode::ExportType;".to_owned());
    }
    let mut main_items = vec!["ExportContract"];
    if !model.functions.is_empty() {
        main_items.push("ExpectedExport");
    }
    if !model.classes.is_empty() {
        main_items.push("Handle");
    }
    main_items.push("Instance");
    main_items.push("Library");
    main_items.push("StdoutHost");
    main_items.sort_unstable();
    uses.push(format!("use kira_main::{{{}}};", main_items.join(", ")));
    // `HostCapabilities` is unconditional: it is the bound on every generated
    // type, so even a library that exports nothing names it.
    let mut abi_items = vec!["HostCapabilities"];
    if !model.functions.is_empty() {
        abi_items.push("NativeResult");
    }
    if model
        .functions
        .iter()
        .any(|function| !function.params.is_empty())
    {
        abi_items.push("NativeArg");
    }
    abi_items.sort_unstable();
    uses.push(match abi_items.as_slice() {
        [one] => format!("use kira_runtime_abi::{one};"),
        many => format!("use kira_runtime_abi::{{{}}};", many.join(", ")),
    });

    format!(
        "// Generated by `kirac build` from the Kira library `{library}`.\n\
         //\n\
         // Do not edit. Every `kirac build` of the library rewrites this file from\n\
         // the library's own `@Export` surface, so an edit here survives exactly\n\
         // until the next build.\n\
         //\n\
         // Engine: VM. The bytecode below is embedded and runs on the Kira VM in\n\
         // this process. There is no linker step, no LLVM, and no `unsafe` in this\n\
         // file — which is what lets a consumer build it anywhere Rust builds.\n\
         \n\
         {uses}\n\
         \n\
         /// What can go wrong loading or calling this library.\n\
         pub type Error = kira_main::Error;\n\
         \n\
         /// The compiled Kira library, embedded so the consumer links nothing.\n\
         const BYTECODE: &[u8] = include_bytes!(\"../{artifact}\");\n\
         \n",
        library = model.library,
        artifact = model.artifact,
        uses = uses.join("\n"),
    )
}

/// The `const CONTRACT` this wrapper checks its library against.
fn contract(model: &Model) -> String {
    let mut out = String::new();
    out.push_str(
        "/// The export surface this file was generated from.\n\
         ///\n\
         /// `load` checks the embedded bytecode against it, so a wrapper generated\n\
         /// from one build of the library and shipped beside another says which\n\
         /// export moved, instead of calling the wrong one.\n\
         const CONTRACT: ExportContract<'static> = ExportContract {\n",
    );
    if model.classes.is_empty() {
        out.push_str("    classes: &[],\n");
    } else {
        out.push_str("    classes: &[\n");
        for class in &model.classes {
            out.push_str(&format!("        \"{}\",\n", class.rust));
        }
        out.push_str("    ],\n");
    }
    if model.functions.is_empty() {
        out.push_str("    functions: &[],\n");
    } else {
        out.push_str("    functions: &[\n");
        for function in &model.functions {
            out.push_str("        ExpectedExport {\n");
            out.push_str(&format!("            name: \"{}\",\n", function.export));
            if function.params.is_empty() {
                out.push_str("            params: &[],\n");
            } else {
                out.push_str("            params: &[\n");
                for param in &function.params {
                    out.push_str(&format!("                {},\n", contract_type(param.ty)));
                }
                out.push_str("            ],\n");
            }
            out.push_str(&format!(
                "            result: {},\n",
                contract_type(function.result)
            ));
            out.push_str("        },\n");
        }
        out.push_str("    ],\n");
    }
    out.push_str(&format!(
        "    content_hash: {:#018x},\n}};\n\n",
        model.content_hash
    ));
    out
}

/// The library type, its `load`, and one method per export.
fn library_struct(model: &Model) -> String {
    let mut out = format!(
        "/// The Kira library `{library}`, loaded and running.\n\
         ///\n\
         /// Holds one VM instance whose heap outlives a call, which is what lets a\n\
         /// handle still mean something between two calls. Neither `Send` nor\n\
         /// `Sync`: one instance belongs to one thread, and the `Rc` inside says so\n\
         /// to the compiler rather than to a comment.\n\
         ///\n\
         /// `{param}` is the host the library's effects go to — `print` and nothing\n\
         /// else in v1. It defaults to `StdoutHost`, so `load()` needs no type\n\
         /// annotation; supply your own with `load_with`.\n\
         pub struct {ty}<{bound} = StdoutHost> {{\n\
         \x20   /// The running instance, shared with every handle into it so that\n\
         \x20   /// dropping a handle can release the object it names.\n\
         \x20   instance: Rc<RefCell<Instance<{param}>>>,\n\
         }}\n\
         \n\
         /// Written out rather than derived: a derived impl would demand\n\
         /// `{param}: Debug`, and a host has no reason to have one.\n\
         impl<{bound}> core::fmt::Debug for {ty}<{param}> {{\n\
         \x20   fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n\
         \x20       f.debug_struct(\"{ty}\").field(\"instance\", &self.instance).finish()\n\
         \x20   }}\n\
         }}\n\
         \n\
         /// Cloning shares the one running instance, so every clone sees one heap.\n\
         ///\n\
         /// Written out for the same reason `Debug` is: `Rc` clones whatever it\n\
         /// points at, and a derived impl would ask the host to be `Clone` anyway.\n\
         impl<{bound}> Clone for {ty}<{param}> {{\n\
         \x20   fn clone(&self) -> {ty}<{param}> {{\n\
         \x20       {ty} {{\n\
         \x20           instance: Rc::clone(&self.instance),\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n\
         \n\
         impl {ty}<StdoutHost> {{\n\
         \x20   /// Loads the embedded library and starts an instance of it, with the\n\
         \x20   /// library's output going to this process's stdout.\n\
         \x20   ///\n\
         \x20   /// Fails when the embedded artifact is not the one this file was\n\
         \x20   /// generated from, naming the first export that disagrees.\n\
         \x20   pub fn load() -> Result<{ty}<StdoutHost>, Error> {{\n\
         \x20       {ty}::load_with(StdoutHost)\n\
         \x20   }}\n\
         }}\n\
         \n\
         impl<{bound}> {ty}<{param}> {{\n\
         \x20   /// Loads the embedded library and starts an instance against `host`.\n\
         \x20   ///\n\
         \x20   /// The instance owns the host for its whole life, so a host that\n\
         \x20   /// accumulates — a capture buffer, a log, a browser console — is\n\
         \x20   /// readable afterwards through `with_host`.\n\
         \x20   pub fn load_with(host: {param}) -> Result<{ty}<{param}>, Error> {{\n\
         \x20       let library = Library::from_bytes(BYTECODE)?;\n\
         \x20       library.verify(&CONTRACT)?;\n\
         \x20       Ok({ty} {{\n\
         \x20           instance: Rc::new(RefCell::new(library.instantiate_with(host)?)),\n\
         \x20       }})\n\
         \x20   }}\n\
         \n\
         \x20   /// Reads the host this library runs against.\n\
         \x20   ///\n\
         \x20   /// A closure rather than a reference, because the instance lives behind\n\
         \x20   /// a `RefCell` shared with every handle: handing out a borrow would\n\
         \x20   /// hand out the `RefCell` guard's lifetime with it.\n\
         \x20   pub fn with_host<R>(&self, read: impl FnOnce(&{param}) -> R) -> R {{\n\
         \x20       read(self.instance.borrow().host())\n\
         \x20   }}\n\
         \n\
         \x20   /// Reaches the host mutably, for an embedder that drains it between\n\
         \x20   /// calls.\n\
         \x20   pub fn with_host_mut<R>(&self, take: impl FnOnce(&mut {param}) -> R) -> R {{\n\
         \x20       take(self.instance.borrow_mut().host_mut())\n\
         \x20   }}\n\
         \n\
         \x20   /// How many handles into this library are still live.\n\
         \x20   pub fn live_handles(&self) -> usize {{\n\
         \x20       self.instance.borrow().live_handles()\n\
         \x20   }}\n",
        library = model.library,
        ty = model.library_type,
        param = HOST_PARAM,
        bound = HOST_BOUND,
    );
    for function in &model.functions {
        out.push('\n');
        out.push_str(&method(model, function));
    }
    out.push_str("}\n");
    out
}

/// One export's method.
fn method(model: &Model, function: &FnModel) -> String {
    let signature: Vec<String> = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, param_type(model, param.ty, HOST_ARG)))
        .collect();
    let arguments: Vec<String> = function
        .params
        .iter()
        .map(|param| match param.ty {
            ExportType::Void => "NativeArg::Void".to_owned(),
            ExportType::Int => format!("NativeArg::Int({})", param.name),
            ExportType::Float => format!("NativeArg::Float({})", param.name),
            ExportType::Bool => format!("NativeArg::Bool({})", param.name),
            ExportType::String => format!("NativeArg::Str({})", param.name),
            ExportType::Handle { .. } => {
                format!("NativeArg::Handle({}.handle.as_word())", param.name)
            }
        })
        .collect();
    // The doc comment restates the Kira signature, so the host parameter is left
    // off there: it is Rust's bookkeeping and not something the author wrote.
    let kira_signature: Vec<String> = function
        .params
        .iter()
        .map(|param| result_type(model, param.ty, ""))
        .collect();

    let call = if arguments.is_empty() {
        format!(
            "        let result = self.instance.borrow_mut().call(\"{}\", &[])?;\n",
            function.export
        )
    } else {
        let mut call = String::from("        let result = self.instance.borrow_mut().call(\n");
        call.push_str(&format!("            \"{}\",\n", function.export));
        call.push_str("            &[\n");
        for argument in &arguments {
            call.push_str(&format!("                {argument},\n"));
        }
        call.push_str("            ],\n        )?;\n");
        call
    };

    let lift = match function.result {
        ExportType::Void => "            NativeResult::Void => Ok(()),\n".to_owned(),
        ExportType::Int => "            NativeResult::Int(value) => Ok(value),\n".to_owned(),
        ExportType::Float => "            NativeResult::Float(value) => Ok(value),\n".to_owned(),
        ExportType::Bool => "            NativeResult::Bool(value) => Ok(value),\n".to_owned(),
        ExportType::String => "            NativeResult::Str(value) => Ok(value),\n".to_owned(),
        ExportType::Handle { class } => format!(
            "            NativeResult::Handle(word) => Ok({} {{\n\
             \x20               handle: Handle::from_word(word),\n\
             \x20               instance: Rc::clone(&self.instance),\n\
             \x20           }}),\n",
            class_name(model, class),
        ),
    };

    format!(
        "    /// Calls the Kira export `{kira_name}({kira_params}) -> {kira_result}`.\n\
         \x20   ///\n\
         \x20   /// Arguments are lent to the library for the duration of the call; the\n\
         \x20   /// result is owned by the caller.\n\
         \x20   pub fn {method}(&self{comma}{signature}) -> Result<{returns}, Error> {{\n\
         {call}\
         \x20       match result {{\n\
         {lift}\
         \x20           found => Err(Error::unexpected_result(\"{export}\", \"{expected}\", &found)),\n\
         \x20       }}\n\
         \x20   }}\n",
        kira_name = function.kira_name,
        kira_params = kira_signature.join(", "),
        kira_result = result_type(model, function.result, ""),
        method = function.method,
        comma = if signature.is_empty() { "" } else { ", " },
        signature = signature.join(", "),
        returns = result_type(model, function.result, HOST_ARG),
        call = call,
        lift = lift,
        export = function.export,
        expected = describe_result(function.result),
    )
}

/// One exported class's Rust newtype and its destructor.
fn class_struct(model: &Model, class: &ClassModel) -> String {
    format!(
        "\n\
         /// A live `{rust}` inside the library.\n\
         ///\n\
         /// Owned: dropping it releases the Kira object it names, and nothing else\n\
         /// ever does. Use-after-free is not expressible — every method borrows the\n\
         /// handle and `Drop` consumes it — and a handle is a ticket rather than an\n\
         /// address, so there is nothing in it to compute with.\n\
         ///\n\
         /// Carries the host parameter of the library it came from, so a handle and\n\
         /// the instance that made it can never be mismatched at a call site.\n\
         pub struct {rust}<{bound} = StdoutHost> {{\n\
         \x20   /// The word the seam carries; opaque to this side.\n\
         \x20   handle: Handle,\n\
         \x20   /// The instance that owns the object, kept alive by this handle.\n\
         \x20   instance: Rc<RefCell<Instance<{param}>>>,\n\
         }}\n\
         \n\
         /// Written out rather than derived, so the host need not be `Debug`.\n\
         impl<{bound}> core::fmt::Debug for {rust}<{param}> {{\n\
         \x20   fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n\
         \x20       f.debug_struct(\"{rust}\").field(\"handle\", &self.handle).finish()\n\
         \x20   }}\n\
         }}\n\
         \n\
         impl<{bound}> {rust}<{param}> {{\n\
         \x20   /// The library instance this handle belongs to.\n\
         \x20   pub fn library(&self) -> {library_ty}<{param}> {{\n\
         \x20       {library_ty} {{\n\
         \x20           instance: Rc::clone(&self.instance),\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n\
         \n\
         impl<{bound}> Drop for {rust}<{param}> {{\n\
         \x20   /// Releases the Kira object this handle names.\n\
         \x20   ///\n\
         \x20   /// A release that fails means the object is already gone, which is the\n\
         \x20   /// state `Drop` was trying to reach: there is nothing to report, and no\n\
         \x20   /// caller left to report it to.\n\
         \x20   fn drop(&mut self) {{\n\
         \x20       let _ = self.instance.borrow_mut().release(self.handle);\n\
         \x20   }}\n\
         }}\n",
        rust = class.rust,
        library_ty = model.library_type,
        param = HOST_PARAM,
        bound = HOST_BOUND,
    )
}
