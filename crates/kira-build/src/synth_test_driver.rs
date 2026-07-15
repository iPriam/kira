//! Pure-Kira test driver synthesis: the compiler synthesizes a Kira entry
//! function that runs every `Test` declaration, compares results in Kira,
//! and prints PASS/FAIL/SKIP — so the same Test runs on vm, llvm, and
//! hybrid without backend-specific comparison overrides.
//!
//! Port target: kira-zig `kira_build/src/synth_test_driver.zig`.
