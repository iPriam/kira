//! The `Ksl` compile-time namespace: how a macro body reaches the KSL compiler.
//!
//! `ksl!("Shaders/X.ksl")` is not meant to stay a compiler builtin. The end
//! state is an ordinary userland `comptime macro ksl` that the engine owns and
//! can evolve without a compiler release, and this namespace is what such a
//! macro calls:
//!
//! ```kira
//! comptime macro ksl {
//!     kind { function }
//!     expand(input: Syntax) -> Syntax {
//!         let msl = Ksl.compile(input, "msl")
//!         let wgsl = Ksl.compile(input, "wgsl")
//!         return quote {
//!             KslArtifact(
//!                 combinedMsl: #{msl.combinedSource},
//!                 vertexWgsl: #{wgsl.vertexSource},
//!                 fragmentWgsl: #{wgsl.fragmentSource},
//!                 vertexEntry: #{wgsl.vertexEntry},
//!                 fragmentEntry: #{wgsl.fragmentEntry},
//!                 resourceReflection: #{msl.resourceReflection},
//!             )
//!         }
//!     }
//! }
//! ```
//!
//! Note what the compiler does *not* know there: `KslArtifact`, its field
//! names, and how many backends the engine wants inlined are all Kira source.
//! The compiler's half of the contract is only [`CompiledShader`] — one
//! backend's output for one shader — which is why that is what
//! `Ksl.compile` returns.
//!
//! # Why this is a trait
//!
//! This crate is layer 1. The KSL pipeline it needs is spread from layer 1
//! (`kira-ksl-parser`) to layer 7 (`kira-build`), all of it *above* here, and
//! the layer DAG admits no upward dependency. So expansion states what it
//! needs as [`ShaderCompiler`] and takes an implementation from its caller —
//! the `kira-backend-api` pattern. With no implementation supplied, every
//! `Ksl.compile` refuses with [`KMAC022`], which is what a compiler with no
//! KSL pipeline yet honestly reports.
//!
//! [`KMAC022`]: crate::diagnostics::SHADER_COMPILE

use crate::diagnostics;
use crate::eval::EvalError;
use crate::value::{RecordValue, Value};

/// The name a macro body writes to reach this namespace.
pub(crate) const NAMESPACE: &str = "Ksl";

/// The name the record [`CompiledShader`] becomes reports as its type.
pub(crate) const RECORD_NAME: &str = "KslCompiled";

/// One backend's compiled output for one shader.
///
/// Every field is source text or a name the engine splices into whatever
/// artifact type it declares. A stage a shader does not have, and a source
/// form a target does not use, are both the empty string rather than an
/// absent member: a macro body reads a member that is always there and asks
/// whether it is empty, instead of branching on a shape that varies per
/// target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CompiledShader {
    /// The shader's declared name.
    pub shader_name: String,
    /// The whole module, for a target that emits one (Metal does).
    pub combined_source: String,
    /// The vertex stage on its own, for a target that splits stages.
    pub vertex_source: String,
    /// The fragment stage on its own, for a target that splits stages.
    pub fragment_source: String,
    /// The compute stage on its own, for a target that splits stages.
    pub compute_source: String,
    /// The vertex entry point's name in the emitted source.
    pub vertex_entry: String,
    /// The fragment entry point's name in the emitted source.
    pub fragment_entry: String,
    /// The compute entry point's name in the emitted source.
    pub compute_entry: String,
    /// Every resource the host binds against, in the pipeline's encoding.
    pub resource_reflection: String,
}

/// Why a shader could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShaderCompileError {
    /// The named file could not be read.
    #[error("`{path}` could not be read: {reason}")]
    Unreadable {
        /// The path as the macro wrote it.
        path: String,
        /// What the read failed with.
        reason: String,
    },
    /// The target is not one this compiler emits.
    #[error("`{target}` is not a shader target this compiler emits")]
    UnknownTarget {
        /// The target as the macro wrote it.
        target: String,
    },
    /// KSL rejected the shader.
    #[error("`{path}` did not compile: {summary}")]
    Rejected {
        /// The path as the macro wrote it.
        path: String,
        /// What the pipeline reported, already rendered.
        summary: String,
    },
}

/// What macro expansion needs from the KSL pipeline.
///
/// Implemented above this crate by whoever owns the pipeline, and handed to
/// [`expand_with`](crate::expand_with).
pub trait ShaderCompiler {
    /// Compiles the shader at `path` for `target`.
    ///
    /// `path` is relative to the project the shader is written in; resolving it
    /// is the implementation's job, because this crate has no filesystem.
    fn compile(&self, path: &str, target: &str) -> Result<CompiledShader, ShaderCompileError>;
}

/// A [`ShaderCompiler`] backed by shaders compiled before expansion ran.
///
/// The KSL pipeline reads files, and macro expansion sits inside salsa queries
/// that must stay pure — so the build layer scans for shader call sites,
/// compiles each one up front, and hands the results in here. The trait is
/// still the seam; this is just the implementation that does no work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PrecompiledShaders {
    /// One entry per (path, target) pair that was compiled.
    entries: Vec<(String, String, CompiledShader)>,
}

impl PrecompiledShaders {
    /// Builds a table from `entries`, each a path, a target, and its output.
    #[must_use]
    pub fn new(entries: Vec<(String, String, CompiledShader)>) -> Self {
        Self { entries }
    }

    /// Whether the table holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ShaderCompiler for PrecompiledShaders {
    fn compile(&self, path: &str, target: &str) -> Result<CompiledShader, ShaderCompileError> {
        self.entries
            .iter()
            .find(|(entry_path, entry_target, _)| entry_path == path && entry_target == target)
            .map(|(_, _, compiled)| compiled.clone())
            .ok_or_else(|| ShaderCompileError::Rejected {
                path: path.to_owned(),
                summary: format!("no `{target}` output was compiled for it"),
            })
    }
}

/// Runs one `Ksl.compile(path, target)` call.
///
/// `shaders` is `None` when no pipeline was supplied, which is the state this
/// compiler is in until the KSL migration lands. It refuses rather than
/// returning an empty [`CompiledShader`]: a shader that silently compiled to
/// nothing would take a render path down at runtime instead of at build time.
pub(crate) fn compile(
    shaders: Option<&dyn ShaderCompiler>,
    values: &[Value],
) -> Result<Value, EvalError> {
    let [path, target] = values else {
        return Err(EvalError::coded(
            diagnostics::SHADER_ARGUMENT_COUNT,
            format!(
                "`{NAMESPACE}.compile` takes a path and a target, but {} argument(s) were passed",
                values.len()
            ),
        ));
    };
    let path = literal_path(path)?;
    let target = &target_label(target)?;
    let Some(shaders) = shaders else {
        return Err(EvalError::coded(
            diagnostics::SHADER_COMPILE,
            format!(
                "`{NAMESPACE}.compile(\"{path}\", \"{target}\")` cannot run: this compiler has no \
                 KSL pipeline yet, so there is nothing to compile the shader with. The KSL front \
                 end and the shader backends are a separate migration; until they land, a shader \
                 source has to be supplied to the renderer directly."
            ),
        ));
    };
    let compiled = shaders
        .compile(&path, target)
        .map_err(|error| EvalError::coded(diagnostics::SHADER_COMPILE, error.to_string()))?;
    Ok(record(&compiled))
}

/// The backend label a macro body named, from the case it wrote.
///
/// A **case** rather than a string, because a target is a closed set. The name
/// is checked by the shader compiler behind [`ShaderCompiler`], which owns the
/// backend enum; what this earns is that the target is *written* as a case, so a
/// backend that is renamed fails at the one place naming it rather than being
/// carried into every artifact as an empty field.
fn target_label(target: &Value) -> Result<String, EvalError> {
    let Value::EnumCase(case) = target else {
        return Err(EvalError::coded(
            diagnostics::SHADER_COMPILE,
            format!(
                "`{NAMESPACE}.compile` names its target as a case, not a `{}` — the cases are `.Msl`, `.Wgsl`, `.Glsl`, `.Hlsl`, `.Spirv`",
                target.type_name()
            ),
        ));
    };
    // The case is spelled in Kira's style (`Glsl`); the backend registry spells
    // its labels in the shader world's (`glsl`). Lowercasing is the whole of the
    // translation, and doing it here keeps the enum reading like Kira.
    Ok(case.variant.to_lowercase())
}

/// The path a macro body passed, whether it wrote a `String` or handed the
/// call site's own syntax straight through.
///
/// A `function` macro receives its arguments as one `Syntax`, so the natural
/// body writes `Ksl.compile(input, "msl")` and `input` is still the source text
/// `"Shaders/X.ksl"` — quotes included. Decoding that here keeps the userland
/// macro from having to unquote a literal by hand.
fn literal_path(value: &Value) -> Result<String, EvalError> {
    match value {
        Value::Str(text) => Ok(text.clone()),
        Value::Syntax(syntax) => {
            let trimmed = syntax.text.trim();
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                Ok(kira_lexer::decode_string_literal(trimmed))
            } else {
                Err(EvalError::coded(
                    diagnostics::SHADER_PATH_NOT_LITERAL,
                    format!(
                        "`{NAMESPACE}.compile` compiles its shader at compile time, so its path \
                         must be a string literal known then, not `{trimmed}`"
                    ),
                ))
            }
        }
        other => Err(EvalError::coded(
            diagnostics::SHADER_PATH_NOT_LITERAL,
            format!(
                "`{NAMESPACE}.compile` needs its path as a string literal, not a `{}`",
                other.type_name()
            ),
        )),
    }
}

/// The compile-time record a [`CompiledShader`] is read through.
///
/// Every member is a `String`, so each one splices into a `quote` as a Kira
/// string literal with its newlines and quotes already escaped — which is what
/// makes inlining a whole shader source into generated Kira work at all.
fn record(compiled: &CompiledShader) -> Value {
    let members = vec![
        ("shaderName", &compiled.shader_name),
        ("combinedSource", &compiled.combined_source),
        ("vertexSource", &compiled.vertex_source),
        ("fragmentSource", &compiled.fragment_source),
        ("computeSource", &compiled.compute_source),
        ("vertexEntry", &compiled.vertex_entry),
        ("fragmentEntry", &compiled.fragment_entry),
        ("computeEntry", &compiled.compute_entry),
        ("resourceReflection", &compiled.resource_reflection),
    ];
    Value::Record(Box::new(RecordValue {
        name: RECORD_NAME,
        members: members
            .into_iter()
            .map(|(name, text)| (name.to_owned(), Value::Str(text.clone())))
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipeline stand-in: it proves the seam, not the shader.
    struct Fixed(CompiledShader);

    impl ShaderCompiler for Fixed {
        fn compile(&self, path: &str, target: &str) -> Result<CompiledShader, ShaderCompileError> {
            if target != "msl" {
                return Err(ShaderCompileError::UnknownTarget {
                    target: target.to_owned(),
                });
            }
            let mut compiled = self.0.clone();
            compiled.combined_source = format!("// {path}\n{}", compiled.combined_source);
            Ok(compiled)
        }
    }

    fn fixture() -> Fixed {
        Fixed(CompiledShader {
            combined_source: "vertex float4 v() {}".to_owned(),
            vertex_entry: "v".to_owned(),
            fragment_entry: "f".to_owned(),
            resource_reflection: "{}".to_owned(),
            ..CompiledShader::default()
        })
    }

    fn member(value: &Value, name: &str) -> String {
        let Value::Record(record) = value else {
            panic!("not a record: {value:?}");
        };
        let (_, found) = record
            .members
            .iter()
            .find(|(member, _)| member == name)
            .expect("member");
        match found {
            Value::Str(text) => text.clone(),
            other => panic!("not a string: {other:?}"),
        }
    }

    /// The target as a macro body writes it.
    fn backend(case: &str) -> Value {
        Value::EnumCase(Box::new(crate::value::EnumCaseValue {
            enum_name: "ShaderBackend".to_owned(),
            variant: case.to_owned(),
            payload: None,
        }))
    }

    #[test]
    fn with_no_pipeline_it_refuses_rather_than_returning_an_empty_shader() {
        let error = compile(
            None,
            &[Value::Str("Shaders/X.ksl".to_owned()), backend("Msl")],
        )
        .expect_err("no pipeline");
        assert_eq!(error.code, diagnostics::SHADER_COMPILE);
        assert!(error.message.contains("Shaders/X.ksl"), "{}", error.message);
    }

    #[test]
    fn a_supplied_pipeline_is_read_back_member_by_member() {
        let shaders = fixture();
        let value = compile(
            Some(&shaders),
            &[Value::Str("Shaders/X.ksl".to_owned()), backend("Msl")],
        )
        .expect("compiled");
        assert!(member(&value, "combinedSource").contains("Shaders/X.ksl"));
        assert_eq!(member(&value, "vertexEntry"), "v");
        assert_eq!(member(&value, "fragmentEntry"), "f");
        // A stage the shader does not have reads as empty, never as absent.
        assert_eq!(member(&value, "computeSource"), "");
    }

    #[test]
    fn the_call_sites_own_syntax_is_accepted_as_the_path() {
        let shaders = fixture();
        let value = compile(
            Some(&shaders),
            &[Value::built("\"Shaders/X.ksl\""), backend("Msl")],
        )
        .expect("compiled");
        assert!(member(&value, "combinedSource").contains("Shaders/X.ksl"));
    }

    #[test]
    fn a_path_that_is_not_a_literal_is_refused_before_the_pipeline_is_asked() {
        let shaders = fixture();
        let error = compile(
            Some(&shaders),
            &[
                Value::built("name"),
                Value::EnumCase(Box::new(crate::value::EnumCaseValue {
                    enum_name: "ShaderBackend".to_owned(),
                    variant: "Msl".to_owned(),
                    payload: None,
                })),
            ],
        )
        .expect_err("not a literal");
        assert_eq!(error.code, diagnostics::SHADER_PATH_NOT_LITERAL);
    }

    #[test]
    fn a_pipeline_refusal_is_reported_under_the_shader_code() {
        let shaders = fixture();
        let error = compile(
            Some(&shaders),
            &[Value::Str("Shaders/X.ksl".to_owned()), backend("Spirv")],
        )
        .expect_err("unknown target");
        assert_eq!(error.code, diagnostics::SHADER_COMPILE);
        assert!(error.message.contains("spirv"), "{}", error.message);
    }

    #[test]
    fn the_wrong_argument_count_is_refused() {
        let error =
            compile(None, &[Value::Str("Shaders/X.ksl".to_owned())]).expect_err("one argument");
        assert_eq!(error.code, diagnostics::SHADER_ARGUMENT_COUNT);
    }
}
