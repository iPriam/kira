//! `kira instruments`: instruction-level profiling for the VM backend.

use crate::options::CompileOptions;
use crate::pipeline::{EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Runs the VM instruction profiler.
pub fn run(args: &[String]) -> i32 {
    let mut compiler_args = Vec::new();
    let mut max_functions = 20usize;
    let mut max_sites = 8usize;
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match argument {
            "--limit" | "--sites" => {
                let Some(value) = args.get(index + 1) else {
                    err!("kira instruments: {argument} expects a positive integer");
                    return EXIT_USAGE;
                };
                let parsed = value.parse::<usize>().ok().filter(|value| *value > 0);
                let Some(parsed) = parsed else {
                    err!("kira instruments: {argument} expects a positive integer");
                    return EXIT_USAGE;
                };
                if argument == "--limit" {
                    max_functions = parsed;
                } else {
                    max_sites = parsed;
                }
                index += 1;
            }
            other => compiler_args.push(other.to_owned()),
        }
        index += 1;
    }
    let options = match CompileOptions::parse(&compiler_args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira instruments: {error}");
            return EXIT_USAGE;
        }
    };
    let code = crate::pipeline::profile_vm(options, max_functions, max_sites);
    if code == EXIT_OK {
        out!("profile complete");
    }
    code
}
