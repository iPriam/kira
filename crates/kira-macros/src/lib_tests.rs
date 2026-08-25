//! Expansion tests for the whole macro pipeline.
//!
//! Split from `lib.rs` rather than trimmed: the module is the pipeline's only
//! end-to-end coverage — a program in, expanded text out — and every case is one
//! shape a macro can take. Keeping them beside the code put that file past the
//! size the repository allows.

use super::*;

fn expand_one(text: &str) -> Expansion {
    expand(&[(SourceId::new(0), text)])
}

#[path = "lib_tests/comptime.rs"]
mod comptime;
#[path = "lib_tests/pipeline.rs"]
mod pipeline;
#[path = "lib_tests/reflection.rs"]
mod reflection;
#[path = "lib_tests/reports.rs"]
mod reports;
