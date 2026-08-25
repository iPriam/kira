//! The invocation pipeline end to end: a program in, expanded text out.
//!
//! What runs, what composes, what crosses files, and what is left of the
//! `Ksl` namespace — the shapes any macro expands through.

use super::*;
#[test]
fn a_program_with_no_macros_is_returned_unchanged() {
    let program = "@Main function main() {\n    print(1)\n    return\n}\n";
    let expansion = expand_one(program);
    assert_eq!(expansion.texts[0], program);
    assert!(expansion.diagnostics.is_empty());
}

#[test]
fn a_macro_declaration_is_blanked_and_keeps_every_other_offset() {
    let program = "macro square(value: expr) { expand { value * value } }\n\
                   @Main function main() {\n    print(square!(6))\n    return\n}\n";
    let expansion = expand_one(program);
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(!expanded.contains("macro square"), "{expanded}");
    assert!(expanded.contains("print(((6) * (6)))"), "{expanded}");
    // The line the macro occupied is still a line.
    assert_eq!(expanded.lines().count(), program.lines().count());
}

#[test]
fn an_unknown_macro_is_reported() {
    let expansion = expand_one(
        "macro known(a: expr) { expand { a } }\n\
         function f() -> Int {\n    return missing!(1)\n}\n",
    );
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC001")),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_macro_may_call_another_macro() {
    let expansion = expand_one(
        "macro double(v: expr) { expand { v + v } }\n\
         macro quad(v: expr) { expand { double!(v) + double!(v) } }\n\
         function f() -> Int {\n    return quad!(3)\n}\n",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(!expanded.contains('!'), "{expanded}");
    assert!(expanded.contains("(3)"), "{expanded}");
}

#[test]
fn a_recursive_macro_hits_the_depth_limit_rather_than_hanging() {
    let expansion = expand_one(
        "macro loopy(v: expr) { expand { loopy!(v) } }\n\
         function f() -> Int {\n    return loopy!(1)\n}\n",
    );
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC010")),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_function_macro_splices_declarations_at_file_scope() {
    let expansion = expand_one(
        r#"
comptime macro bits {
kind { function }
expand(input: Syntax) -> Syntax {
    let names: [Identifier] = input.identifiers()
    var fns: [Syntax] = []
    var value: Int = 1
    for name in names {
        fns.append(quote {
            function #{name}() -> Int { return #{value} }
        })
        value = value * 2
    }
    return quote { #{fns} }
}
}

bits!(Read, Write, Exec)
"#,
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let text = &expansion.texts[0];
    assert!(
        text.contains("function Read() -> Int { return 1 }"),
        "{text}"
    );
    assert!(
        text.contains("function Write() -> Int { return 2 }"),
        "{text}"
    );
    assert!(
        text.contains("function Exec() -> Int { return 4 }"),
        "{text}"
    );
}

#[test]
fn a_splice_glues_to_the_text_beside_it() {
    let expansion = expand_one(
        r#"
comptime macro prefixed {
kind { function }
expand(input: Syntax) -> Syntax {
    let names: [Identifier] = input.identifiers()
    var fns: [Syntax] = []
    for name in names {
        fns.append(quote { function mxp_#{name}() -> Int { return 1 } })
    }
    return quote { #{fns} }
}
}

prefixed!(Foo, Bar)
"#,
    );
    let text = &expansion.texts[0];
    assert!(text.contains("function mxp_Foo()"), "{text}");
    assert!(text.contains("function mxp_Bar()"), "{text}");
}

#[test]
fn a_macro_declared_in_one_file_expands_in_another() {
    let expansion = expand(&[
        (
            SourceId::new(0),
            "macro square(v: expr) { expand { v * v } }\n",
        ),
        (
            SourceId::new(1),
            "function f() -> Int {\n    return square!(5)\n}\n",
        ),
    ]);
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[1].contains("((5) * (5))"),
        "{:?}",
        expansion.texts
    );
}

/// A pipeline stand-in, so the seam can be proven without one.
struct OneShader;

impl ShaderCompiler for OneShader {
    fn compile(&self, path: &str, target: &str) -> Result<CompiledShader, ShaderCompileError> {
        Ok(CompiledShader {
            combined_source: format!("// {target} of {path}\nvertex void v() {{}}"),
            vertex_entry: "v".to_owned(),
            fragment_entry: "f".to_owned(),
            ..CompiledShader::default()
        })
    }
}

/// The userland `ksl` the KSL migration is aiming at.
///
/// Note what is *not* in the compiler here: `KslArtifact`, its field names,
/// and how many backends get inlined are all Kira source.
const USERLAND_KSL: &str = r#"
enum ShaderBackend { Msl Wgsl Glsl Hlsl Spirv }

comptime macro ksl {
kind { function }
expand(input: Syntax) -> Syntax {
    let msl = Ksl.compile(input, ShaderBackend.Msl)
    return quote {
        KslArtifact(combinedMsl: #{msl.combinedSource}, vertexEntry: #{msl.vertexEntry})
    }
}
}

function load() -> KslArtifact {
return ksl!("Shaders/Tri.ksl")
}
"#;

#[test]
fn there_is_no_builtin_ksl_left_to_fall_back_on() {
    // `ksl!` was a compiler builtin and is not one any more: the engine
    // declares it. An undeclared call is an unknown macro like any other,
    // and if a builtin ever returned this would report something else.
    let expansion = expand_one(
        "macro other(v: expr) { expand { v } }\n\
         function f() {\n    let s = ksl!(\"Shaders/Tri.ksl\")\n}\n",
    );
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC001")),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn with_no_pipeline_the_userland_macro_refuses_under_the_shader_code() {
    let expansion = expand_one(USERLAND_KSL);
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC022")),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_userland_ksl_macro_inlines_what_the_pipeline_compiled() {
    let shaders = OneShader;
    let expansion = expand_with(&[(SourceId::new(0), USERLAND_KSL)], Some(&shaders), "macos");
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let text = &expansion.texts[0];
    // The shader source crossed into generated Kira as a string literal,
    // newlines escaped — which is what makes inlining a whole backend's
    // output into an artifact work at all.
    assert!(
        text.contains(r#"combinedMsl: "// msl of Shaders/Tri.ksl\nvertex void v() {}""#),
        "{text}"
    );
    assert!(text.contains(r#"vertexEntry: "v""#), "{text}");
    assert!(!text.contains("ksl!"), "{text}");
}
