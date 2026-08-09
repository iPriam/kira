//! The name a resource takes once it reaches GLSL.
//!
//! Here rather than in the GLSL backend because a resource's emitted name is a
//! *binding decision*, and this crate is where those are made. The reflection
//! carries `glsl_name` for exactly one reason — a GL host looks a texture unit
//! or a uniform up by name — so the name the reflection reports and the name the
//! backend emits have to be the same string, and one function is what guarantees
//! it. When they drifted, a shader declaring `external` compiled to a syntax
//! error at the reserved word while the host bound against the unprefixed name.

/// A name GLSL will accept, given a name KSL allowed.
///
/// GLSL reserves far more words than KSL does — `input`, `output` and `external`
/// among them, reserved since 1.30 — so a KSL identifier can be perfectly good
/// and still be a syntax error once emitted. Reserved names are prefixed rather
/// than rejected: what the author called a thing is their business, and the
/// emitted name is nobody's but the backend's.
#[must_use]
pub fn glsl_safe_name(name: &str) -> String {
    if RESERVED.contains(&name) {
        return format!("ksl_{name}");
    }
    name.to_owned()
}

/// GLSL words a KSL identifier may collide with.
///
/// Two groups. The reserved-for-future-use list, which is most of this. And the
/// live keywords KSL does not refuse itself — the storage and memory qualifiers
/// above all, because `out` is an ordinary name in KSL and the natural one for a
/// stage's output value, and a shader writing `let out: VertexOut` emitted a
/// declaration GLSL reads as the start of an interface variable.
///
/// Not the type names: those are spelled by the backend's type mapping and never
/// come from an author's identifier.
const RESERVED: &[&str] = &[
    "in",
    "out",
    "inout",
    "uniform",
    "buffer",
    "layout",
    "centroid",
    "coherent",
    "discard",
    "readonly",
    "writeonly",
    "atomic_uint",
    "active",
    "asm",
    "attribute",
    "cast",
    "class",
    "common",
    "double",
    "enum",
    "extern",
    "external",
    "filter",
    "fixed",
    "flat",
    "goto",
    "half",
    "highp",
    "hvec2",
    "hvec3",
    "hvec4",
    "inline",
    "input",
    "interface",
    "invariant",
    "lowp",
    "mediump",
    "namespace",
    "noinline",
    "noperspective",
    "output",
    "packed",
    "partition",
    "patch",
    "precise",
    "precision",
    "public",
    "resource",
    "restrict",
    "row_major",
    "sample",
    "shared",
    "sizeof",
    "smooth",
    "static",
    "subroutine",
    "superp",
    "template",
    "this",
    "typedef",
    "union",
    "unsigned",
    "using",
    "varying",
    "volatile",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_word_is_prefixed_and_anything_else_is_left_alone() {
        assert_eq!(glsl_safe_name("external"), "ksl_external");
        assert_eq!(glsl_safe_name("input"), "ksl_input");
        // A live keyword, not a reserved word: `out` is what a stage's output
        // value is naturally called, and GLSL reads it as a storage qualifier.
        assert_eq!(glsl_safe_name("out"), "ksl_out");
        assert_eq!(glsl_safe_name("atlas"), "atlas");
    }
}
