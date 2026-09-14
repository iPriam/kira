use super::*;

fn run_body(text: &str, arguments: Vec<(String, Value)>) -> Result<Outcome, EvalError> {
    let body = compile(text).expect("a parseable expand body");
    let functions = ComptimeFunctions::new();
    let enums = HashMap::new();
    let comptime = Comptime {
        functions: &functions,
        shaders: None,
        platform: "unknown",
        enums: &enums,
        testing: false,
    };
    run(&body, arguments, comptime, false)
}

#[test]
fn arithmetic_and_conditionals_run() {
    let outcome = run_body(
            "var total: Int = 0\nvar i: Int = 0\nwhile i < 4 {\n    total = total + i\n    i = i + 1\n}\nif total > 5 {\n    return quote { big }\n}\nreturn quote { small }\n",
            Vec::new(),
        )
        .expect("a result");
    assert_eq!(outcome.syntax.trim(), "big");
}

#[test]
fn a_for_loop_over_an_array_binds_each_element() {
    let outcome = run_body(
            "var out: [Syntax] = []\nfor name in names {\n    out.append(quote { #{name} })\n}\nreturn quote { #{out} }\n",
            vec![(
                "names".to_owned(),
                Value::Array(vec![
                    Value::Identifier("a".to_owned()),
                    Value::Identifier("b".to_owned()),
                ]),
            )],
        )
        .expect("a result");
    assert!(outcome.syntax.contains('a'), "{}", outcome.syntax);
    assert!(outcome.syntax.contains('b'), "{}", outcome.syntax);
}

/// A loop that never exits stops the build instead of hanging the
/// compiler. The fuel starts one step from empty so the test trips the
/// budget rather than running it out for real.
#[test]
fn a_loop_that_never_exits_is_stopped() {
    let body = compile("while true {\nvar x: Int = 1\n}\n").expect("a parseable expand body");
    let mut evaluator = evaluator_with_fuel(&body, STEP_LIMIT - 1, 0);
    let error = match evaluator.block(&body.block) {
        Err(error) => error,
        Ok(_) => panic!("a stop rather than a hang"),
    };
    assert_eq!(error.code, "KMAC010");
}

/// A loop cloning an ever-larger value stops the build instead of hanging
/// the compiler. The fuel starts one cell from empty so the test trips the
/// budget rather than building it out for real.
#[test]
fn a_loop_cloning_ever_larger_values_is_stopped() {
    let body = compile("var words: [String] = [\"ab\"]\nwhile true {\nwords.append(\"cd\")\n}\n")
        .expect("a parseable expand body");
    let mut evaluator = evaluator_with_fuel(&body, 0, CELL_LIMIT - 1);
    let error = match evaluator.block(&body.block) {
        Err(error) => error,
        Ok(_) => panic!("a stop rather than a hang"),
    };
    assert_eq!(error.code, "KMAC010");
}

/// An evaluator with preset fuel, so a budget test trips the limit it is
/// proving rather than running the budget out for real.
fn evaluator_with_fuel<'a>(body: &'a Body, steps: u64, cells: u64) -> Evaluator<'a> {
    // Leaked rather than scoped: the borrows outlive the call, and the
    // test ends with them.
    let functions: &'a ComptimeFunctions = Box::leak(Box::new(ComptimeFunctions::new()));
    let enums: &'a HashMap<String, Vec<String>> = Box::leak(Box::new(HashMap::new()));
    Evaluator {
        body,
        functions,
        depth: 0,
        scopes: vec![HashMap::new()],
        reported: Vec::new(),
        shaders: None,
        platform: "unknown".to_owned(),
        enums: enums.clone(),
        testing: false,
        lint: false,
        fuel: Fuel(Rc::new(FuelCounts {
            steps: Cell::new(steps),
            cells: Cell::new(cells),
        })),
    }
}

#[test]
fn an_unsupported_statement_is_refused_rather_than_guessed() {
    let error =
        run_body("match x {\n    Red -> return quote { }\n}\n", Vec::new()).expect_err("a refusal");
    assert_eq!(error.code, "KMAC020");
}

/// `Identifier(text)` builds the use-site identifier the text spells, so a
/// macro can name what only exists as a string — one branch per spelling
/// is the alternative, and it stops scaling at two.
#[test]
fn an_identifier_built_from_text_splices_as_a_name() {
    let outcome = run_body(
        "var name: Identifier = Identifier(\"answer\")\nreturn quote { #{name} }",
        Vec::new(),
    )
    .expect("a result");
    assert_eq!(outcome.syntax.trim(), "answer");
}

/// Text no identifier can spell is refused where the text is still
/// visible, not where the name would have landed.
#[test]
fn an_identifier_built_from_a_keyword_or_a_non_name_is_refused() {
    for text in ["return", "9lives", "", "has space", "has-dash"] {
        let error = run_body(
            &format!("var name: Identifier = Identifier(\"{text}\")\nreturn quote {{ x }}"),
            Vec::new(),
        )
        .expect_err("a refusal");
        assert_eq!(error.code, "KMAC013", "{text}");
    }
}
