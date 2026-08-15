use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=vendor");
    if env::var("CARGO_CFG_WINDOWS").is_err() || env::var("CARGO_CFG_TARGET_ARCH")? != "x86_64" {
        return Ok(());
    }

    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("windows-x86_64")
        .join("libffi-8.dll");
    if !source.is_file() {
        return Err(format!("bundled libffi is missing: {}", source.display()).into());
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let profile_dir = out_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("OUT_DIR has no cargo profile directory")?;
    fs::copy(&source, profile_dir.join("libffi-8.dll"))?;
    Ok(())
}
