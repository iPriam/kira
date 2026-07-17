//! Every example in the repo must behave identically on every backend.

use std::path::PathBuf;

use crate::{BACKENDS, run_on};

/// Every example in the repo must behave identically on every backend.
#[test]
fn every_example_agrees_on_every_backend() {
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("the examples directory");

    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).expect("read examples") {
        let directory = entry.expect("example entry").path();
        if !directory.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&directory).expect("read example directory") {
            let source = file.expect("example file").path();
            if source.extension().is_none_or(|kind| kind != "kira") {
                continue;
            }
            let vm = run_on(&source, "vm");
            for backend in &BACKENDS[1..] {
                let run = run_on(&source, backend);
                assert_eq!(
                    String::from_utf8_lossy(&vm.stdout),
                    String::from_utf8_lossy(&run.stdout),
                    "example `{}` differs between the vm and {backend} backends.\n\
                     {backend} stderr: {}",
                    source.display(),
                    String::from_utf8_lossy(&run.stderr),
                );
                assert_eq!(
                    vm.status.code(),
                    run.status.code(),
                    "example `{}` exits differently on the vm and {backend} backends",
                    source.display(),
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "no examples were checked");
}
