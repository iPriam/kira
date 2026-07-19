//! The model a generated crate is rendered from, and every refusal that
//! happens before a character of it is written.
//!
//! Split from [`super::emit`] at the boundary the two halves already had: this
//! file decides *what* the crate says — which names are legal Rust, which
//! collide, which engine is being targeted — and `emit` decides how it reads. A
//! failure here names the offending Kira declaration; a failure there would be a
//! generated file that did not compile, which is why all the refusing is on this
//! side.
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
pub(crate) const HOST_ARG: &str = "<H>";

/// The generic header the generated types and impls are written under.
pub(crate) const HOST_BOUND: &str = "H: HostCapabilities";

/// Which engine the rendered crate drives, and what that engine needs named.
///
/// The VM engine and the hybrid engine are rendered by **this** module rather
/// than by two, and that is a parity decision rather than a tidiness one. The
/// feature's central claim is that a consumer's code does not change when the
/// engine does; two renderers would let the two APIs drift apart one convenience
/// at a time, and no test would notice until a consumer tried to switch. One
/// renderer with the difference named makes the delta reviewable — it is
/// everything in this enum and nothing else.
///
/// The native engine is not here: it emits an `extern` block, `unsafe`, and a
/// `build.rs`, which is a different file rather than a different binding.
/// [`super::render_native`] owns it, and the consumer test crate is what proves
/// all three agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EngineBinding {
    /// Bytecode embedded in the crate, run on the VM.
    Vm,
    /// Bytecode plus a manifest embedded in the crate, and a shared library
    /// found on disk at load.
    Hybrid {
        /// The embedded `.khm` manifest's file name.
        manifest: String,
        /// The absolute path the build wrote the shared library to.
        ///
        /// The *last* place the loader looks; see `kira_hybrid_main::locate`.
        native_half: String,
    },
}

impl EngineBinding {
    /// The crate the generated code takes its engine types from.
    pub(crate) fn crate_name(&self) -> &'static str {
        match self {
            EngineBinding::Vm => "kira_main",
            EngineBinding::Hybrid { .. } => "kira_hybrid_main",
        }
    }

    /// The type one running copy of the library has.
    pub(crate) fn instance_type(&self) -> &'static str {
        match self {
            EngineBinding::Vm => "Instance",
            EngineBinding::Hybrid { .. } => "HybridInstance",
        }
    }

    /// The type a decoded, not-yet-running library has.
    pub(crate) fn library_type(&self) -> &'static str {
        match self {
            EngineBinding::Vm => "Library",
            EngineBinding::Hybrid { .. } => "HybridLibrary",
        }
    }

    /// The error type every generated method returns.
    pub(crate) fn error_type(&self) -> &'static str {
        match self {
            EngineBinding::Vm => "kira_main::Error",
            EngineBinding::Hybrid { .. } => "kira_hybrid_main::HybridMainError",
        }
    }
}

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
    /// Which engine the rendered crate drives.
    pub engine: EngineBinding,
}

impl Model {
    /// Validates every name in `spec` and resolves it to its Rust spelling.
    ///
    /// All the refusing happens here, before a single character is rendered, so
    /// a failure names the offending Kira declaration rather than pointing at a
    /// generated file that would not compile.
    pub(crate) fn build(
        spec: &WrapperSpec<'_>,
        engine: EngineBinding,
    ) -> Result<Model, WrapperError> {
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
            engine,
        })
    }
}

/// Resolves one export's names and checks every handle names a real class.
pub(crate) fn build_function(
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
pub(crate) fn check_class(ty: ExportType, table: &ExportTable) -> Result<(), WrapperError> {
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
pub(crate) fn param_type(model: &Model, ty: ExportType, host: &str) -> String {
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
pub(crate) fn result_type(model: &Model, ty: ExportType, host: &str) -> String {
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
pub(crate) fn class_name(model: &Model, index: u32) -> String {
    crate::wrapper::class_name_of(model.classes.get(index as usize), index)
}

/// How `ty` is spelled inside the `const CONTRACT`.
pub(crate) fn contract_type(ty: ExportType) -> String {
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
pub(crate) fn describe_result(ty: ExportType) -> &'static str {
    match ty {
        ExportType::Void => "nothing",
        ExportType::Int => "an integer",
        ExportType::Float => "a float",
        ExportType::Bool => "a boolean",
        ExportType::String => "a string",
        ExportType::Handle { .. } => "a handle",
    }
}
