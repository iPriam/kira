//! Semantic coverage for the host main-thread capability.

use kira_semantics_model::Type;
use kira_semantics_model::hir::HirExpr;

use super::{analyze_text, codes, diagnostics};
use crate::BuildMachine;

#[test]
fn a_lifecycle_loop_runs_alongside_the_entrypoint() {
    let text = "function build() -> Int { return 42 }\n\
                @MainThreadLifecycle function ui() { print(build()) return }\n\
                @Main function main() { return }";
    let program = analyze_text(text);
    assert!(diagnostics(text).is_empty());
    // The entrypoint keeps the application thread; the lifecycle is a
    // separate function that owns the process main thread.
    assert_eq!(program.main, Some(kira_semantics_model::FuncId(2)));
    assert_eq!(
        program.main_thread_lifecycles,
        vec![kira_semantics_model::FuncId(1)]
    );
}

#[test]
fn one_function_cannot_be_both_the_entrypoint_and_the_lifecycle() {
    assert_eq!(
        codes("@Main @MainThreadLifecycle function main() { return }"),
        vec!["KSEM339"]
    );
}

#[test]
fn several_lifecycles_share_the_main_thread_with_dispatched_tasks() {
    let text = "@MainThreadLifecycle function graphics() { return }\n\
                @MainThreadLifecycle function ui() { return }\n\
                @MainThread function task() { return }\n\
                @Main function main() { MainThread.post { task() } return }";
    let program = analyze_text(text);
    assert!(diagnostics(text).is_empty());
    assert_eq!(
        program.main_thread_lifecycles,
        vec![
            kira_semantics_model::FuncId(0),
            kira_semantics_model::FuncId(1)
        ]
    );
}

#[test]
fn calling_a_lifecycle_starts_it_through_the_main_thread_host() {
    let text = "@MainThreadLifecycle function ui() { return }\n\
                @Main function main() { ui() return }";
    let program = analyze_text(text);
    assert!(diagnostics(text).is_empty());
    assert!(program.exprs.iter().any(|(_, expr)| matches!(
        expr,
        HirExpr::MainThreadCall {
            operation: kira_runtime_abi::MainThreadOp::LifecycleStart,
            function: kira_semantics_model::FuncId(0),
            ..
        }
    )));
}

#[test]
fn a_lifecycle_function_takes_no_parameters() {
    assert_eq!(
        codes(
            "@MainThreadLifecycle function ui(value: Int) { return }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM341"]
    );
}

#[test]
fn a_lifecycle_function_returns_void() {
    assert_eq!(
        codes(
            "@MainThreadLifecycle function ui() -> Int { return 1 }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM344"]
    );
}

#[test]
fn main_thread_operations_resolve_without_an_executor_name() {
    let program = analyze_text(
        "@MainThread function uiInt(value: Int) -> Int { return value + 1 }\n\
         @MainThread function uiPost(value: Int) { return }\n\
         @Main function main() {\n\
             let task = MainThread.spawn { uiInt(1) }\n\
             MainThread.post { uiPost(2) }\n\
             let value = MainThread.invoke { uiInt(task.await) }\n\
             return\n\
         }",
    );
    assert!(
        diagnostics(
            "@MainThread function uiInt(value: Int) -> Int { return value + 1 }\n\
         @MainThread function uiPost(value: Int) { return }\n\
         @Main function main() {\n\
             let task = MainThread.spawn { uiInt(1) }\n\
             MainThread.post { uiPost(2) }\n\
             let value = MainThread.invoke { uiInt(task.await) }\n\
             return\n\
         }"
        )
        .is_empty()
    );
    assert!(
        program
            .exprs
            .iter()
            .any(|(_, expr)| matches!(expr, HirExpr::MainThreadCall { .. }))
    );
    assert!(program.exprs.iter().any(|(_, expr)| matches!(
        expr,
        HirExpr::MainThreadJoin {
            ty: Type::Int(_),
            ..
        }
    )));
}

#[test]
fn main_thread_spawn_preserves_a_non_scalar_result_type() {
    let text = "@MainThread function ui() -> String { return \"ok\" }\n\
                @Main function main() { let task = MainThread.spawn { ui() }\n\
                    let value = task.await print(value) return }";
    let program = analyze_text(text);
    assert!(diagnostics(text).is_empty());
    assert!(program.exprs.iter().any(|(_, expr)| matches!(
        expr,
        HirExpr::MainThreadJoin {
            ty: Type::String,
            ..
        }
    )));
}

#[test]
fn a_direct_helper_call_to_a_main_thread_function_is_rejected() {
    assert_eq!(
        codes(
            "@MainThread function ui() -> Int { return 1 }\n\
             @Main function main() { ui() return }"
        ),
        vec!["KSEM330"]
    );
}

#[test]
fn a_main_thread_operation_requires_a_main_thread_target() {
    assert_eq!(
        codes(
            "function ordinary() -> Int { return 1 }\n\
             @Main function main() { let value = MainThread.invoke { ordinary() } return }"
        ),
        vec!["KSEM332"]
    );
}

#[test]
fn a_class_method_can_be_a_main_thread_target() {
    assert!(
        diagnostics(
            "class Window { let value: Int = 3\n\
         @MainThread function read() -> Int { return self.value } }\n\
         @Main function main() { let window = Window()\n\
             let value = MainThread.invoke { window.read() } return }"
        )
        .is_empty()
    );
}

#[test]
fn main_cannot_be_both_the_helper_entry_and_a_main_thread_target() {
    assert_eq!(
        codes("@Main @MainThread function main() { return }"),
        vec!["KSEM337"]
    );
}

#[test]
fn main_thread_targets_are_rejected_for_web_builds() {
    let text = "@MainThread function ui() -> Int { return 1 }\n\
                @Main function main() { return }";
    let db = salsa::DatabaseImpl::new();
    let source = crate::SourceProgram::new(
        &db,
        text.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        crate::BuildKind::Application,
        crate::PrecompiledShaders::default(),
        BuildMachine::new("emscripten", "wasm32"),
        false,
    );
    let diagnostics = crate::analyzed::accumulated::<crate::DiagnosticAccumulator>(&db, source);
    assert_eq!(
        diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.0.code_text())
            .collect::<Vec<_>>(),
        vec!["KSEM338"]
    );
}

#[test]
fn a_main_thread_operation_needs_one_direct_call() {
    let errors = diagnostics("@Main function main() { MainThread.post { 1 2 } return }");
    assert!(errors.iter().any(|error| error.has_code("KSEM331")));
    assert!(errors.iter().any(|error| error.has_code("KSEM332")));
}
