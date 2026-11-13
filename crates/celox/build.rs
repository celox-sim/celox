#![allow(clippy::disallowed_methods)] // Cargo target configuration is build-script input.

//! Configuration that depends on both the target and enabled features.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(hotspot_jit)");
    // Incremental (hotspot) compilation runs generated code in this process,
    // so it needs a native backend for the host: x86-64 or AArch64 without
    // the other architecture's cross-codegen feature.
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let native = match arch.as_str() {
        "x86_64" => !feature("ARM64_CODEGEN"),
        "aarch64" => !feature("X86_64_CODEGEN"),
        _ => false,
    };
    if native {
        println!("cargo::rustc-cfg=hotspot_jit");
    }
    if feature("CLI") {
        export_vpi_host_symbols();
    }
}

/// Export the `celox` command's VPI functions to the cocotb library it loads.
fn export_vpi_host_symbols() {
    const BINARY: &str = "celox";
    match std::env::var("CARGO_CFG_TARGET_OS")
        .unwrap_or_default()
        .as_str()
    {
        "linux" | "freebsd" => {
            println!("cargo::rustc-link-arg-bin={BINARY}=-Wl,--export-dynamic");
        }
        "macos" => println!("cargo::rustc-link-arg-bin={BINARY}=-Wl,-export_dynamic"),
        "windows" => {
            if std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() == "msvc" {
                for symbol in VPI_HOST_EXPORTS {
                    println!("cargo::rustc-link-arg-bin={BINARY}=/EXPORT:{symbol}");
                }
            } else {
                println!("cargo::rustc-link-arg-bin={BINARY}=-Wl,--export-all-symbols");
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
