//! Parity for macro expansion.
//!
//! Expansion is a frontend source-to-source pass, so by the time any backend
//! sees a macro-using program there is no macro left in it — parity here is
//! structural rather than earned. These cases exist to prove that claim rather
//! than assert it: the same program, the same stdout, on the VM, the LLVM
//! backend, and the hybrid split.
//!
//! The expected values are the ones the reference's own corpus asserts, so a
//! divergence is visible against the oracle and not just against ourselves.

use crate::{assert_module_parity, assert_parity};

/// The declarative surface: fragment substitution, single evaluation, hygiene,
/// and a `place` fragment written through.
#[test]
fn declarative_macros_agree() {
    let output = assert_parity(
        r#"
macro mxSquare(value: expr) {
    expand {
        value * value
    }
}

macro mxAdd3(a: expr, b: expr, c: expr) {
    expand {
        a + b + c
    }
}

macro mxSwap(a: place, b: place) {
    expand {
        let temporary = a
        a = b
        b = temporary
    }
}

@Main
function main() {
    print(mxSquare!(6))
    print(mxAdd3!(1, 2, 3))
    var x = 10
    var y = 20
    mxSwap!(x, y)
    // Hygiene: the macro's own `temporary` never collides with this one.
    let temporary = 100
    print(x + y + temporary)
    print(mxSquare!(3) + mxSquare!(4))
    return
}
"#,
    );
    assert_eq!(output, "36\n6\n130\n25\n");
}

/// An `expr` fragment is evaluated exactly once even though the template names
/// it twice — the C-style double-evaluation footgun does not exist here.
#[test]
fn an_expr_fragment_is_evaluated_once_on_every_backend() {
    let output = assert_parity(
        r#"
macro mxTwice(value: expr) {
    expand {
        value + value
    }
}

function mxNext() -> Int {
    print("called")
    return 3
}

@Main
function main() {
    print(mxTwice!(mxNext()))
    return
}
"#,
    );
    // One "called" for two occurrences of the fragment: the argument was bound
    // to a hygienic temporary ahead of the statement and read twice.
    assert_eq!(output, "called\n6\n");
}

/// Derive macros: field reflection, `quote`/`#{}` splicing, and `Syntax.join`.
#[test]
fn derive_macros_agree() {
    let output = assert_parity(
        r#"
comptime macro MxFieldCount {
    kind { derive }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax {
        var count: Int = 0
        for field in target.fields {
            count = count + 1
        }
        return quote {
            function mxVecFieldCount() -> Int {
                return #{count}
            }
        }
    }
}

comptime macro MxSum {
    kind { derive }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax {
        var parts: [Syntax] = []
        for field in target.fields {
            parts.append(quote { p.#{field.name} })
        }
        let joined: Syntax = Syntax.join(parts, separator: " + ")
        return quote {
            function mxSumVec(p: borrow MxVec3) -> Int {
                return #{joined}
            }
        }
    }
}

@Derive(MxFieldCount, MxSum)
struct MxVec3 {
    var x: Int
    var y: Int
    var z: Int
}

@Main
function main() {
    print(mxVecFieldCount())
    let v = MxVec3 { x: 4, y: 5, z: 6 }
    print(mxSumVec(v))
    return
}
"#,
    );
    assert_eq!(output, "3\n15\n");
}

/// An enum's variants surface through the same `target.fields` a struct's do.
#[test]
fn an_enum_derive_agrees() {
    let output = assert_parity(
        r#"
comptime macro mxVariantCount {
    kind { derive }
    appliesTo { enum }
    expand(target: Declaration) -> Syntax {
        var count: Int = 0
        for field in target.fields {
            count = count + 1
        }
        return quote {
            function mxEnumVariantCount() -> Int {
                return #{count}
            }
        }
    }
}

@Derive(mxVariantCount)
enum MxColor {
    Red
    Green
    Blue
}

@Main
function main() {
    print(mxEnumVariantCount())
    return
}
"#,
    );
    assert_eq!(output, "3\n");
}

/// An attribute macro, and the property-wrapper conformance shape it is used
/// for: mid-identifier splice gluing names the generated functions after the
/// annotated type.
#[test]
fn attribute_macros_agree() {
    let output = assert_parity(
        r#"
comptime macro MxPropertyWrapper {
    kind { attribute }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax {
        var hasWrapped: Bool = false
        var hasProjected: Bool = false
        for field in target.fields {
            if field.name.asString() == "wrappedValue" {
                hasWrapped = true
            }
            if field.name.asString() == "projectedValue" {
                hasProjected = true
            }
        }
        if hasWrapped == false {
            Diagnostics.error("PropertyWrapper requires a wrappedValue field", at: target.syntax)
            return quote { }
        }
        return quote {
            function is_#{target.name}_propertyWrapper() -> Bool {
                return true
            }
            function has_#{target.name}_projectedValue() -> Bool {
                return #{hasProjected}
            }
        }
    }
}

@MxPropertyWrapper
struct MxStateWrapper {
    var wrappedValue: Int
    var projectedValue: Bool
}

@MxPropertyWrapper
struct MxPlainWrapper {
    var wrappedValue: Int
}

@Main
function main() {
    print(is_MxStateWrapper_propertyWrapper())
    print(has_MxStateWrapper_projectedValue())
    print(has_MxPlainWrapper_projectedValue())
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\nfalse\n");
}

/// A `function`-kind procedural macro in all three positions: declaration,
/// statement, and expression.
#[test]
fn function_macros_agree_in_every_position() {
    let output = assert_parity(
        r#"
comptime macro mxBits {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var fns: [Syntax] = []
        var value: Int = 1
        for name in names {
            fns.append(quote {
                function #{name}() -> Int {
                    return #{value}
                }
            })
            value = value * 2
        }
        return quote {
            #{fns}
        }
    }
}

comptime macro mxConst {
    kind { function }
    expand(input: Syntax) -> Syntax {
        return quote { 42 }
    }
}

comptime macro mxAssignTen {
    kind { function }
    expand(input: Syntax) -> Syntax {
        return quote {
            r = 10
        }
    }
}

comptime macro mxPrefixed {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var fns: [Syntax] = []
        for name in names {
            fns.append(quote {
                function mxp_#{name}() -> Int {
                    return 1
                }
            })
        }
        return quote {
            #{fns}
        }
    }
}

mxBits!(MxRead, MxWrite, MxExec)
mxPrefixed!(Foo, Bar)

@Main
function main() {
    print(MxRead() + MxWrite() + MxExec())
    print(mxConst!())
    var r = 0
    mxAssignTen!()
    print(r)
    print(mxp_Foo() + mxp_Bar())
    return
}
"#,
    );
    assert_eq!(output, "7\n42\n10\n2\n");
}

/// A field-triggered, replace-mode attribute macro: the annotated fields are
/// dropped and every unshadowed read and write of them is rerouted through
/// generated accessors, while a local of the same name is left alone.
#[test]
fn a_field_triggered_rewrite_agrees() {
    let output = assert_parity(
        r#"
comptime macro MfxTracked {
    kind { attribute }
    appliesTo { form }
    trigger { field }
    replace { true }

    expand(target: Declaration) -> Syntax {
        var stateFields: [Syntax] = []
        var accessors: [Syntax] = []
        var rewritten: Syntax = target.syntax
        for field in target.fields {
            if field.hasAnnotation("MfxTracked") {
                stateFields.append(quote {
                    var #{field.name}: #{field.type.asSyntax()} = #{field.initializer}
                })
                accessors.append(quote {
                    function __state_#{target.name}_get_#{field.name}() -> #{field.type.asSyntax()} {
                        print("get:" + #{field.name.asString()})
                        let store = __State_#{target.name} {}
                        return store.#{field.name}
                    }
                    function __state_#{target.name}_set_#{field.name}(value: #{field.type.asSyntax()}) {
                        print("set:" + #{field.name.asString()})
                        return
                    }
                })
                rewritten = rewritten.dropField(field.name)
                rewritten = rewritten.rewriteProperty(
                    field.name,
                    quote { __state_#{target.name}_get_#{field.name}() },
                    quote { __state_#{target.name}_set_#{field.name} }
                )
            }
        }
        return quote {
            struct __State_#{target.name} {
                #{stateFields}
            }
            #{accessors}
            #{rewritten}
        }
    }
}

construct MfxPanel {
    @Required let body: Int
}

MfxPanel MfxDemo() {
    @MfxTracked var count: Int = 7
    @MfxTracked var label: String = "hello"
    let body: Int = 1

    function poke() -> Int {
        count = count + 1
        return count
    }

    function readLabel() -> String {
        return label
    }

    function shadowed() -> Int {
        let count = 99
        return count
    }
}

@Main
function main() {
    let d = MfxDemo()
    print(d.poke())
    print(d.readLabel())
    print(d.shadowed())
    return
}
"#,
    );
    assert_eq!(
        output,
        "get:count\nset:count\nget:count\n7\nget:label\nhello\n99\n"
    );
}

/// The full property-wrapper protocol: one `kind { wrapper }` macro defines it,
/// an annotated struct is the template, and a field annotated with the
/// template's name summons the macro over the enclosing form.
#[test]
fn the_wrapper_protocol_agrees() {
    let output = assert_parity(
        r#"
comptime macro MwsPropertyWrapper {
    kind { wrapper }
    appliesTo { form }

    expand(target: Declaration, wrapper: Declaration) -> Syntax {
        if target.name.asString() == wrapper.name.asString() {
            var hasWrapped: Bool = false
            for field in target.fields {
                if field.name.asString() == "wrappedValue" {
                    hasWrapped = true
                }
            }
            if hasWrapped == false {
                Diagnostics.error("MwsPropertyWrapper requires a wrappedValue field", at: target.syntax)
                return quote { }
            }
            return quote {
                function is_#{target.name}_propertyWrapper() -> Bool {
                    return true
                }
            }
        }
        var monos: [Syntax] = []
        var accessors: [Syntax] = []
        var rewritten: Syntax = target.syntax
        for field in target.fields {
            if field.hasAnnotation(wrapper.name.asString()) {
                let monoName = "__pw_" + target.name.asString() + "_" + field.name.asString()
                var mono: Syntax = wrapper.syntax.replaceIdentifier(wrapper.name.asString(), monoName)
                mono = mono.replaceIdentifier("Wrapped", field.type.asSyntax())
                monos.append(mono)
                accessors.append(quote {
                    function __pw_#{target.name}_get_#{field.name}() -> #{field.type.asSyntax()} {
                        let backing = __pw_#{target.name}_#{field.name} {
                            wrappedValue: #{field.initializer}
                            key: #{target.name.asString() + "." + field.name.asString()}
                        }
                        return backing.get()
                    }
                    function __pw_#{target.name}_set_#{field.name}(value: #{field.type.asSyntax()}) {
                        let backing = __pw_#{target.name}_#{field.name} {
                            wrappedValue: #{field.initializer}
                            key: #{target.name.asString() + "." + field.name.asString()}
                        }
                        backing.set(move value)
                        return
                    }
                })
                rewritten = rewritten.dropField(field.name)
                rewritten = rewritten.rewriteProperty(
                    field.name,
                    quote { __pw_#{target.name}_get_#{field.name}() },
                    quote { __pw_#{target.name}_set_#{field.name} }
                )
            }
        }
        return quote {
            #{monos}
            #{accessors}
            #{rewritten}
        }
    }
}

@MwsPropertyWrapper
struct MwsState {
    var wrappedValue: Wrapped
    var key: String = ""

    function get() -> Wrapped {
        print("get:" + self.key)
        return self.wrappedValue
    }

    function set(value: Wrapped) {
        print("set:" + self.key)
        return
    }
}

construct MwsPanel {
    @Required let body: Int
}

MwsPanel MwsDemo() {
    @MwsState var count: Int = 7
    @MwsState var label: String = "hello"
    let body: Int = 1

    function poke() -> Int {
        count = count + 1
        return count
    }

    function readLabel() -> String {
        return label
    }

    function shadowed() -> Int {
        let count = 99
        return count
    }
}

@Main
function main() {
    print(is_MwsState_propertyWrapper())
    let d = MwsDemo()
    print(d.poke())
    print(d.readLabel())
    print(d.shadowed())
    return
}
"#,
    );
    assert_eq!(
        output,
        "true\nget:MwsDemo.count\nset:MwsDemo.count\nget:MwsDemo.count\n7\nget:MwsDemo.label\nhello\n99\n"
    );
}

/// A macro declared in one module expands at a call site in another: the
/// registry spans the whole program, so an imported macro works like a local
/// one.
#[test]
fn a_macro_crosses_a_module_boundary() {
    let output = assert_module_parity(
        "import helpers\n\
         @Main function main() {\n\
             print(hxDouble!(21))\n\
             return\n\
         }",
        &[(
            "helpers",
            "macro hxDouble(value: expr) {\n    expand {\n        value + value\n    }\n}\n",
        )],
    );
    assert_eq!(output, "42\n");
}
