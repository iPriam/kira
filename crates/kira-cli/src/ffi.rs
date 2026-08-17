//! `kira ffi`: inspect the foreign boundary selected by a compiled program.
//!
//! This command deliberately stops before linking. It answers which externs
//! the frontend produced and which archives/frameworks/system libraries the
//! selected target would give the backend, which makes a missing native row
//! diagnosable without starting a compiler or linker subprocess.

use crate::options::CompileOptions;
use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Runs `kira ffi [file|dir] [--device host|wasm32|wasm64] [--target <arch-os-abi>]`.
///
/// `--target` answers the question this command exists for, before a cross build
/// is attempted: which rows a package's `nativeLibraries` would select for that
/// machine, and which of them it declares nothing for.
pub fn ffi(args: &[String]) -> i32 {
    let options = match CompileOptions::parse(args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira ffi: {error}");
            return EXIT_USAGE;
        }
    };
    let target = crate::pipeline::compile_target_for_options(&options);
    let source = match crate::pipeline::resolve_source_path(&options.path) {
        Ok(source) => source,
        Err(code) => return code,
    };
    let compiled = match crate::pipeline::compile_verified_path(&source, &target) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let imports = &compiled.ir.foreign_imports;
    if imports.is_empty() {
        out!("{}: no foreign imports", source);
        return EXIT_OK;
    }
    let resolved = match crate::foreign_libs::resolve(
        std::path::Path::new(&source),
        &compiled.ir,
        target.clone(),
    ) {
        Ok(Some(inputs)) => inputs,
        Ok(None) => unreachable!("foreign imports were present"),
        Err(error) => {
            err!("kira ffi: {error}");
            return EXIT_FAILURE;
        }
    };

    out!("target: {target}");
    out!("imports: {}", imports.len());
    for (index, entry) in imports.iter().enumerate() {
        out!(
            "  [{index}] {} -> {}::{}",
            entry.name,
            entry.import.library(),
            entry.import.symbol()
        );
    }
    if !resolved.archives().is_empty() {
        out!("archives:");
        for archive in resolved.archives() {
            out!("  {}", archive.display());
        }
    }
    if !resolved.frameworks().is_empty() {
        out!("frameworks: {}", resolved.frameworks().join(", "));
    }
    if !resolved.system_libs().is_empty() {
        out!("system libraries: {}", resolved.system_libs().join(", "));
    }
    if !resolved.runtime_files().is_empty() {
        out!("runtime files:");
        for file in resolved.runtime_files() {
            out!("  {}", file.display());
        }
    }
    if !resolved.unavailable_imports().is_empty() {
        out!(
            "unavailable imports: {}",
            resolved
                .unavailable_imports()
                .iter()
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    EXIT_OK
}
