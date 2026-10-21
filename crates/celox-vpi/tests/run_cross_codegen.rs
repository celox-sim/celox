//! Cross-codegen uses the normal CLI and must never initialize foreign code.
#![cfg(any(
    all(target_arch = "x86_64", feature = "arm64-codegen"),
    all(target_arch = "aarch64", feature = "x86_64-codegen")
))]

use std::{fs, process::Command};

#[test]
fn exports_foreign_native_image_and_rejects_execution() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("top.veryl");
    let image = root.path().join("top.image");
    fs::write(
        &source,
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args([
            "run",
            "--top",
            "Top",
            "--compile-only",
            "--native-image-output",
        ])
        .arg(&image)
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    celox::NativeProgramImage::from_container_bytes(&fs::read(image).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args(["run", "--top", "Top"])
        .arg(source)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cross-codegen requires --compile-only"),
        "{output:?}"
    );
}
