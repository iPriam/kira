//! Parity for the executable slice of the struct-attached `@FFI.*` family:
//! zero-filled construction of a `@FFI.Struct { layout: c }`.
//!
//! Zero-fill is a frontend construction rule — it lowers to an ordinary
//! `StructNew` of zero literals — so the VM, LLVM/native, and hybrid backends
//! must produce byte-identical output with no new opcode. These programs read
//! the zeroed fields back and print them, which is the parity statement for the
//! rule.

use crate::assert_parity;

#[test]
fn a_c_layout_empty_literal_zero_fills_every_field() {
    let output = assert_parity(
        r#"
@FFI.Struct { layout: c; }
struct V {
    var a: I32
    var b: I64
    var flag: Bool
    var ratio: F64
}

@Main
function main() {
    let v = V {}
    print(v.a)
    print(v.b)
    print(v.flag)
    print(v.ratio)
    return
}
"#,
    );
    assert_eq!(output, "0\n0\nfalse\n0\n");
}

#[test]
fn a_c_layout_paren_call_zero_fills_like_an_empty_literal() {
    let output = assert_parity(
        r#"
@FFI.Struct { layout: c; }
struct V {
    var a: I32
    var b: I32
}

@Main
function main() {
    let made = V()
    let braced = V {}
    print(made.a + made.b)
    print(braced.a + braced.b)
    return
}
"#,
    );
    assert_eq!(output, "0\n0\n");
}

#[test]
fn a_c_layout_initializer_overrides_only_its_field() {
    let output = assert_parity(
        r#"
@FFI.Struct { layout: c; }
struct V {
    var a: I32
    var b: I32
    var c: I32
}

@Main
function main() {
    let v = V { b: 7 }
    print(v.a)
    print(v.b)
    print(v.c)
    return
}
"#,
    );
    assert_eq!(output, "0\n7\n0\n");
}

#[test]
fn a_nested_c_layout_struct_zero_fills_recursively() {
    let output = assert_parity(
        r#"
@FFI.Struct { layout: c; }
struct Inner {
    var x: I32
    var y: I32
}

@FFI.Struct { layout: c; }
struct Outer {
    var inner: Inner
    var tag: I32
}

@Main
function main() {
    let o = Outer {}
    print(o.inner.x)
    print(o.inner.y)
    print(o.tag)
    let p = Outer { tag: 9 }
    print(p.inner.x + p.tag)
    return
}
"#,
    );
    assert_eq!(output, "0\n0\n0\n9\n");
}
