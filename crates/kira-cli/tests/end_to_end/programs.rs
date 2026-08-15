//! Single-file programs through the real binary: what runs, what is rejected,
//! and what the rejection says.

use crate::{check_source, run_source};

#[test]
fn runs_a_program_and_prints_its_output() {
    let output = run_source(
        "@Main function main() { print(runtimeCount()) print(doubleValue(21)) return }\n\
         function runtimeCount() -> Int { return 3 }\n\
         function doubleValue(value: Int) -> Int { return value + value }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "3\n42\n");
}

#[test]
fn recursion_and_control_flow_execute() {
    let output = run_source(
        "@Main function main() { var i = 0 var s = 0 while i < 5 { s = s + i i = i + 1 } print(s) print(fib(10)) return }\n\
         function fib(n: Int) -> Int { if n < 2 { return n } return fib(n - 1) + fib(n - 2) }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "10\n55\n");
}

#[test]
fn string_operations_execute() {
    let output = run_source(
        "@Main function main() { let a = \"foo\" let b = \"bar\" print(a + b) print(a == b) return }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "foobar\nfalse\n");
}

#[test]
fn check_accepts_a_clean_program() {
    let output = check_source("@Main function main() { print(1) return }");
    assert!(output.status.success());
}

#[test]
fn undefined_name_fails_with_a_rendered_diagnostic() {
    let output = run_source("@Main function main() { print(missing) return }");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM060"), "stderr was: {stderr}");
    assert!(
        stderr.contains('^'),
        "diagnostic should show a caret: {stderr}"
    );
    // A rejected program produces no stdout.
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_main_is_rejected() {
    let output = run_source("function f() -> Int { return 1 }");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM011"));
}

#[test]
fn a_construct_declaration_compiles_and_runs() {
    // A construct-backed declaration is a typed factory: constructing it and
    // reading its computed bridge member runs the member.
    let output = run_source(
        "construct Shape { let area: Int { 0 } }\n\
         Shape Square(side: Int) { let area: Int { side * side } }\n\
         @Main function main() { print(Square(side: 6).area) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "36\n");
}

#[test]
fn inferred_construct_members_and_updates_run_through_the_binary() {
    let output = run_source(
        "enum Material { Low XHigh }\n\
         struct Glass { var material: Material = .Low }\n\
         construct Style { let additionalEffect: Int = 0\n\
             let liquidGlass: Glass = Glass {}\n\
             let score: Int { additionalEffect + (liquidGlass.material == .XHigh ? 10 : 0) } }\n\
         Style Base { let additionalEffect = 3\n\
             let liquidGlass = Glass {} }\n\
         @Main function main() {\n\
             let StyleImplementation = Base {}\n\
             let button = StyleImplementation { let additionalEffect = 8 }\n\
             let sidebar = StyleImplementation { let liquidGlass.material = .XHigh }\n\
             print(button.score) print(sidebar.score) return\n\
         }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "8\n13\n");
}

#[test]
fn a_class_declaration_compiles_and_runs() {
    // An inherited method reads a field default the subclass overrode, and a
    // parent-qualified call runs the parent body against this instance.
    let output = run_source(
        "class Account { var balance: Int = 100\n let rate: Int = 2\n \
           function gross() -> Int { return self.balance * self.rate } }\n\
         class Savings extends Account { override let rate = 5\n \
           function bonus() -> Int { return Account.gross() + self.balance } }\n\
         @Main function main() { print(Savings().gross()) print(Savings().bonus()) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "500\n600\n");
}

#[test]
fn an_enum_declaration_compiles_and_runs() {
    // A leading-dot member resolves against the expected type, and `==`
    // compares discriminants.
    let output = run_source(
        "enum Color { Red Green Blue }\n\
         function rank(c: Color) -> Int { if c == .Green { return 2 } return 1 }\n\
         @Main function main() { print(rank(.Green)) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "2\n");
}

#[test]
fn a_struct_declaration_compiles_and_runs() {
    let output = run_source(
        "struct Point { var x: Int  var y: Int = 4 }\n\
         @Main function main() { let p = Point { x = 3 } print(p.x + p.y) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn a_struct_method_compiles_and_runs() {
    let output = run_source(
        "struct P { var x: Int\n function doubled() -> Int { return x * 2 } }\n\
         @Main function main() { let p = P { x = 21 } print(p.doubled()) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn type_error_is_rejected() {
    let output = run_source("@Main function main() { print(1 + true) return }");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM071"));
}

#[test]
fn missing_return_path_is_rejected_before_execution() {
    // The C1 soundness hole: without the definite-return check this printed an
    // empty line and exited 0; it must now be a compile error.
    let output = run_source(
        "function f(n: Int) -> Int { if n > 100 { return 1 } }\n\
         @Main function main() { print(f(5)) return }",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM033"));
    assert!(output.stdout.is_empty());
}

#[test]
fn newline_continuation_matches_the_reference_at_runtime() {
    // Parity with the reference: `let a = 5` / `-2` folds into `5 - 2`
    // (the reference prints 3 for this program; verified by running it).
    let output =
        run_source("@Main function main() {\n    let a = 5\n    -2\n    print(a)\n    return\n}");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "3\n");
}

#[test]
fn a_generic_enum_declaration_compiles_and_runs() {
    // The standard library's `Result`, verbatim, through the real binary: the
    // declaration names no type, the two written instantiations each mint one,
    // and `match` reads their differently-typed payloads back out.
    let output = run_source(
        "enum AppError { NotFound }\n\
         enum Result<Value, Failure> { Ok(Value) Error(Failure) }\n\
         function number(flag: Bool) -> Result<Int, AppError> {\n\
             if flag { return .Ok(7) }\n\
             return .Error(.NotFound)\n\
         }\n\
         function text() -> Result<String, AppError> { return .Ok(\"seven\") }\n\
         @Main function main() {\n\
             match number(true) { Ok(n) -> { print(n) } Error -> { print(0) } }\n\
             match number(false) { Ok(n) -> { print(n) } Error -> { print(0) } }\n\
             match text() { Ok(s) -> { print(s) } Error -> { print(\"none\") } }\n\
             return\n\
         }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n0\nseven\n");
}

#[test]
fn a_generic_arity_mismatch_is_rejected_with_a_rendered_diagnostic() {
    let output = run_source(
        "enum Result<Value, Failure> { Ok(Value) Error(Failure) }\n\
         @Main function main() { let x: Result<Int> = .Ok(1) print(1) return }",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM174"));
    assert!(output.stdout.is_empty());
}

#[test]
fn a_generic_struct_is_refused_by_name() {
    // Only an `enum` takes type parameters. A generic struct, class, or
    // function has no call site in the corpus, so it is refused rather than
    // guessed at.
    let output =
        run_source("struct Box<Value> { let v: Int }\n@Main function main() { print(1) return }");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KPAR047"));
}
