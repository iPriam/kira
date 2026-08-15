fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let process_symbols = [
        "kira_live_emit_first_frame",
        "kira_live_emit_log_line",
        "kira_live_mark_reload",
        "kira_live_take_reload",
        "kira_dynamic_alloc",
        "kira_dynamic_free",
        "kira_dynamic_null_ptr",
        "kira_dynamic_ptr_is_null",
        "kira_dynamic_read_ptr_at",
        "kira_dynamic_read_u32_at",
        "kira_dynamic_read_u64_at",
        "kira_dynamic_read_u8_at",
        "kira_dynamic_write_f32_at",
        "kira_dynamic_write_ptr_at",
        "kira_dynamic_write_u32_at",
        "kira_dynamic_write_u64_at",
        "kira_dynamic_write_u8_at",
    ];
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if target_os == "windows" {
        for symbol in process_symbols {
            println!("cargo:rustc-link-arg-bin=kira-desktop-runner=/include:{symbol}");
            println!("cargo:rustc-link-arg-bin=kira-desktop-runner=/export:{symbol}");
        }
    } else if target_os == "macos" {
        println!("cargo:rustc-link-arg-bin=kira-desktop-runner=-Wl,-export_dynamic");
        for symbol in process_symbols {
            println!(
                "cargo:rustc-link-arg-bin=kira-desktop-runner=-Wl,-u,_{}",
                symbol
            );
        }
    } else if target_family == "unix" {
        println!("cargo:rustc-link-arg-bin=kira-desktop-runner=-rdynamic");
        for symbol in process_symbols {
            println!("cargo:rustc-link-arg-bin=kira-desktop-runner=-Wl,--undefined={symbol}");
        }
    }
}
