//! A data struct's implicit memberwise constructor: `Point(x, y)` fills the
//! fields a `Point { x: .., y: .. }` literal names, and the refusals a
//! mis-shaped argument list earns.

use super::codes;
use super::diagnostics;

#[test]
fn a_full_memberwise_construction_type_checks() {
    assert!(
        diagnostics(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(1, 2)  print(p.x + p.y)  return }"
        )
        .is_empty()
    );
}

#[test]
fn a_partial_construction_defaults_the_trailing_fields() {
    // Only `x` is given; `y` takes its declared default, exactly as an omitted
    // field does in a struct literal.
    assert!(
        diagnostics(
            "struct Point { var x: Int = 7  var y: Int = 9 }\n\
             @Main function main() { let p = Point(1)  print(p.x + p.y)  return }"
        )
        .is_empty()
    );
}

#[test]
fn an_empty_construction_is_the_all_defaulted_value() {
    assert!(
        diagnostics(
            "struct Point { var x: Int = 7  var y: Int = 9 }\n\
             @Main function main() { let p = Point()  print(p.x + p.y)  return }"
        )
        .is_empty()
    );
}

#[test]
fn a_named_construction_fills_every_omitted_defaulted_field() {
    let text = r#"
struct StrictExample {
    var name: String = "default name"
    var title: String = "default title"
    var content: String = "default content"
}

@Main
function main() {
    let value = StrictExample(name: "given name")
    print(value.title)
    print(value.content)
    return
}
"#;
    assert!(diagnostics(text).is_empty());
}

#[test]
fn a_braced_struct_literal_fills_every_omitted_defaulted_field() {
    let text = r#"
struct StrictExample {
    var name: String
    var title: String = "default title"
    var content: String = "default content"
}

@Main
function main() {
    let value = StrictExample {
        name = "given name"
        content = "given content"
    }
    print(value.title)
    print(value.content)
    return
}
"#;
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
}

#[test]
fn an_empty_braced_struct_literal_uses_all_field_defaults() {
    let text = r#"
struct StrictExample {
    let title: String = "default title"
    let content: String = "default content"
}

@Main
function main() {
    let value = StrictExample { }
    print(value.title)
    print(value.content)
    return
}
"#;
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
}

#[test]
fn too_many_arguments_are_refused() {
    assert_eq!(
        codes(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(1, 2, 3)  print(p.x)  return }"
        ),
        vec!["KSEM223"],
    );
}

#[test]
fn an_argument_of_the_wrong_type_is_refused() {
    assert_eq!(
        codes(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(true, 2)  print(p.y)  return }"
        ),
        vec!["KSEM224"],
    );
}

#[test]
fn a_field_with_no_argument_and_no_default_is_missing() {
    assert_eq!(
        codes(
            "struct Cell { var value: Int }\n\
             @Main function main() { let c = Cell()  print(c.value)  return }"
        ),
        vec!["KSEM225"],
    );
}

#[test]
fn a_labeled_memberwise_construction_binds_each_field_by_name() {
    // The label names the field, so the written order need not match the
    // declared one — the argument still fills the field its name says.
    assert!(
        diagnostics(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(y: 2, x: 1)  print(p.x + p.y)  return }"
        )
        .is_empty()
    );
}

#[test]
fn a_label_that_names_no_field_is_refused() {
    assert_eq!(
        codes(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(z: 1)  print(p.x)  return }"
        ),
        vec!["KSEM226"],
    );
}

#[test]
fn a_field_set_twice_is_refused() {
    assert_eq!(
        codes(
            "struct Point { var x: Int = 0  var y: Int = 0 }\n\
             @Main function main() { let p = Point(1, x: 2)  print(p.x)  return }"
        ),
        vec!["KSEM227"],
    );
}

#[test]
fn a_local_of_the_same_name_still_wins_over_the_constructor() {
    // A binding the reader can see beats a type they would have to look up: the
    // shadowing local guards the construction path, so `Point(2)` is not read as
    // a memberwise call and the Int local is simply not callable.
    assert_eq!(
        codes(
            "struct Point { var x: Int = 0 }\n\
             @Main function main() { let Point = 1  print(Point(2))  return }"
        ),
        vec!["KSEM061"],
    );
}
