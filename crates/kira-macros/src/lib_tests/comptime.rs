//! `comptime function` evaluation: the calls a body folds before expansion.

use super::*;
#[test]
fn a_comptime_function_call_becomes_its_value() {
    let program = "comptime function twice(n: Int) -> Int { return n * 2 }
                   @Main function main() {
print(twice(21))
return
}
";
    let expansion = expand_one(program);
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(!expanded.contains("comptime function"), "{expanded}");
    assert!(expanded.contains("print(42)"), "{expanded}");
    // The declaration was blanked rather than removed, so every later
    // offset is where it was.
    assert_eq!(expanded.lines().count(), program.lines().count());
}

#[test]
fn a_comptime_function_runs_statements_not_just_one_expression() {
    // The whole point of building this on the macro evaluator: locals and a
    // loop, which a single-expression folder cannot do.
    let expansion = expand_one(
        "comptime function sumTo(limit: Int) -> Int {
             var total = 0
             var i = 1
             while i <= limit {
                 total = total + i
                 i = i + 1
             }
             return total
         }
         @Main function main() {
print(sumTo(100))
return
}
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("print(5050)"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn one_comptime_function_calls_another() {
    let expansion = expand_one(
        "comptime function double(n: Int) -> Int { return n * 2 }
         comptime function quad(n: Int) -> Int { return double(double(n)) }
         @Main function main() {
print(quad(5))
return
}
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("print(20)"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn a_method_sharing_a_comptime_functions_name_is_left_alone() {
    // A call site is found by name, so a method or a declaration wearing the
    // same name has to be told apart by what precedes it.
    let expansion = expand_one(
        "comptime function double(n: Int) -> Int { return n * 2 }
         struct Counter {
             var value: Int = 0
             function double(n: Int) -> Int { return n + self.value }
         }
         @Main function main() {
             var c = Counter { value: 100 }
             print(c.double(1))
             return
         }
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(
        expanded.contains("function double(n: Int) -> Int { return n + self.value }"),
        "{expanded}"
    );
    assert!(expanded.contains("c.double(1)"), "{expanded}");
}

#[test]
fn a_comptime_function_that_cannot_fold_is_refused_rather_than_emitted() {
    // A call left standing would reach a backend as a call to a function
    // that is not there, so an argument the evaluator cannot read is an
    // error rather than a passthrough.
    let expansion = expand_one(
        "comptime function twice(n: Int) -> Int { return n * 2 }
         @Main function main() {
var runtime = 5
print(twice(runtime))
return
}
",
    );
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code(diagnostics::UNSUPPORTED_IN_EXPAND)),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_comptime_function_that_calls_itself_is_refused_rather_than_hanging() {
    let expansion = expand_one(
        "comptime function loops(n: Int) -> Int { return loops(n) }
         @Main function main() {
print(loops(1))
return
}
",
    );
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code(diagnostics::DEPTH_LIMIT)),
        "{:?}",
        expansion.diagnostics
    );
}
