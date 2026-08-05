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
        _ => {}
    }
}
