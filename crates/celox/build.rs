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
}
