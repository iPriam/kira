//! Collector activation tests.

use super::*;

const TEST_RUNNER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests-kik/harness/app/TestRunner.kira"
));
const TEST_VOCABULARY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests-kik/harness/app/Test.kira"
));

fn source(db: &salsa::DatabaseImpl, kind: BuildKind) -> SourceProgram {
    SourceProgram::new(
        db,
        "Test Sample {}".to_owned(),
        "main.kira".to_owned(),
        vec![
            ModuleSource {
                module: "KikHarness.Test".to_owned(),
                path: "tests-kik/harness/app/Test.kira".to_owned(),
                text: TEST_VOCABULARY.to_owned(),
            },
            ModuleSource {
                module: "KikHarness.TestRunner".to_owned(),
                path: "tests-kik/harness/app/TestRunner.kira".to_owned(),
                text: TEST_RUNNER.to_owned(),
            },
        ],
        kind,
        PrecompiledShaders::default(),
        host_platform(),
        false,
    )
}

#[test]
fn a_test_runner_only_generates_in_test_mode() {
    let db = salsa::DatabaseImpl::new();

    let ordinary_source = source(&db, BuildKind::Application);
    assert_eq!(*ordinary_source.build_kind(&db), BuildKind::Application);
    let ordinary = expanded(&db, ordinary_source);
    assert!(
        !ordinary.entry.contains("processArgument(0)"),
        "{}",
        ordinary.entry
    );
    assert!(
        !ordinary.entry.contains("__kira_test_expect_Sample"),
        "{}",
        ordinary.entry
    );

    let test = expanded(&db, source(&db, BuildKind::Test));
    assert!(test.entry.contains("processArgument(0)"));
    assert!(test.entry.contains("__kira_test_expect_Sample"));
    assert!(test.entry.contains("__kira_test_check_Sample"));
    assert!(test.entry.contains("__kira_test_body_Sample"));
}
