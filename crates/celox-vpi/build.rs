#![allow(clippy::disallowed_methods)] // Cargo target configuration is build-script input.

fn main() {
    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_default();
    match target_os.as_str() {
        "linux" | "freebsd" => {
            println!("cargo:rustc-link-arg-bin=celox-vpi-runtime=-Wl,--export-dynamic");
        }
        "macos" => {
            println!("cargo:rustc-link-arg-bin=celox-vpi-runtime=-Wl,-export_dynamic");
        }
        "windows" => {
            let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
            if target_env == "msvc" {
                for symbol in VPI_HOST_EXPORTS {
                    println!("cargo:rustc-link-arg-bin=celox-vpi-runtime=/EXPORT:{symbol}");
                }
            } else {
                println!("cargo:rustc-link-arg-bin=celox-vpi-runtime=-Wl,--export-all-symbols");
            }
        }
        _ => {}
    }
}

const VPI_HOST_EXPORTS: &[&str] = &[
    "vpi_chk_error",
    "vpi_control",
    "vpi_free_object",
    "vpi_get",
    "vpi_get_str",
    "vpi_get_time",
    "vpi_get_value",
    "vpi_get_vlog_info",
    "vpi_handle",
    "vpi_handle_by_index",
    "vpi_handle_by_name",
    "vpi_iterate",
    "vpi_put_value",
    "vpi_register_cb",
    "vpi_remove_cb",
    "vpi_scan",
];
