//! Backend parity for inferred construct fields, bare braces, and copy/update
//! rebuilding.

use crate::assert_parity;

#[test]
fn inferred_fields_and_nested_updates_execute_identically() {
    let output = assert_parity(
        r#"
enum Material { Low XHigh }

struct Glass {
    var material: Material = .Low
}

construct Style {
    let additionalEffect: Int = 0
    let liquidGlass: Glass = Glass {}
    let score: Int { additionalEffect + (liquidGlass.material == .XHigh ? 10 : 0) }
}

Style Base {
    let additionalEffect = 3
    let liquidGlass = Glass {}
}

@Main
function main() {
    let StyleImplementation = Base {}
    let button = StyleImplementation { let additionalEffect = 8 }
    let sidebar = StyleImplementation {
        let liquidGlass.material = .XHigh
    }
    print(button.score)
    print(sidebar.score)
    return
}
"#,
    );
    assert_eq!(output, "8\n13\n");
}

#[test]
fn later_construct_members_read_earlier_members_on_every_backend() {
    let output = assert_parity(
        r#"
construct Theme {
    let value: Int = 0
}

Theme Concrete {
    let value = 3
    let doubled = value + 1
}

@Main
function main() {
    print(Concrete {}.doubled)
    return
}
"#,
    );
    assert_eq!(output, "4\n");
}

#[test]
fn bare_braced_constructs_keep_child_content_on_every_backend() {
    let output = assert_parity(
        r#"
construct Child {
    @Required let value: Int
}

Child Leaf(value: Int) {
    let result: Int { value }
}

construct Stack {
    let child: some Child
    let result: Int { child.value }
}

Stack Wrap {
    let child: some Child
}

@Main
function main() {
    let stack = Wrap { Leaf(value: 7) }
    print(stack.result)
    return
}
"#,
    );
    assert_eq!(output, "7\n");
}
