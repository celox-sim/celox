#![cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]

use std::{fs, path::Path, process::Command};

const SOURCE: &str = r#"
module Counter (clk: input clock, rst: input reset, q: output logic<32>) {
    always_ff {
        if_reset { q = 0; }
        else { q += 1; }
    }
}
#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen (clk);
    var q: logic<32>;
    inst dut: Counter (clk, rst, q);
    initial {
        rst.assert(2);
        clk.next(4);
        $assert(q == 4);
        $finish();
    }
}
"#;

fn project(source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/test.veryl"), source).unwrap();
    project
}

#[allow(clippy::disallowed_methods)] // Isolate child-process environment in integration tests.
fn run(project: &Path, args: &[&str], success: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_celox-heliodor"));
    // Isolate the test from user diagnostics and codegen environment switches.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CELOX_") {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(project)
        .arg("--project")
        .arg(project)
        .args(["--test", "t", "--source-file", "src/test.veryl"])
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.success(), success, "{stdout}\n{stderr}");
    format!("{stdout}\n{stderr}")
}

fn cached(project: &Path, args: &[&str], status: &str, success: bool) -> String {
    let mut cached_args = vec!["--build-cache-dir", "cache"];
    cached_args.extend_from_slice(args);
    let output = run(project, &cached_args, success);
    assert!(
        output.contains(&format!("CELOX_BUILD_CACHE test=t status={status}")),
        "{output}"
    );
    output
}

#[test]
fn reuses_compilation_across_processes_and_invalidates_changed_inputs() {
    let project = project(SOURCE);
    let path = project.path();
    assert!(!run(path, &[], true).contains("CELOX_BUILD_CACHE"));
    let output_image = path.join("generated.image");
    cached(
        path,
        &[
            "--compile-only",
            "--native-image-output",
            output_image.to_str().unwrap(),
        ],
        "miss",
        true,
    );
    assert!(output_image.exists());
    let cached_image = path.join("cached.image");
    cached(
        path,
        &[
            "--compile-only",
            "--native-image-output",
            cached_image.to_str().unwrap(),
        ],
        "hit",
        true,
    );
    assert_eq!(
        fs::read(output_image).unwrap(),
        fs::read(cached_image).unwrap()
    );
    for _ in 0..2 {
        let output = cached(path, &[], "hit", true);
        assert!(
            output.contains("CELOX_TEST_RESULT test=t status=pass"),
            "{output}"
        );
    }
    // Execution-only limits do not change the generated program.
    cached(path, &["--tick-limit", "100"], "hit", true);
    for args in [
        vec!["--opt-level", "o0"],
        vec!["--four-state"],
        vec![
            "--x86-slp",
            if cfg!(target_arch = "x86_64") {
                "off"
            } else {
                "on"
            },
        ],
        vec![
            "--native-memory-width",
            if cfg!(target_arch = "x86_64") {
                "64"
            } else {
                "128"
            },
        ],
        vec!["--sir-pass=-gvn"],
    ] {
        cached(path, &args, "miss", true);
        cached(path, &args, "hit", true);
    }
    fs::write(
        path.join("src/test.veryl"),
        format!("{SOURCE}\n// edited\n"),
    )
    .unwrap();
    cached(path, &[], "miss", true);
    cached(path, &[], "hit", true);
    fs::write(
        path.join("Veryl.toml"),
        "[project]\nname = \"cache_test\"\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    cached(path, &[], "miss", true);
}

#[test]
fn tracks_readmem_contents_and_new_lookup_candidates() {
    let project = project(
        r#"
module Rom (q: output logic<8>) {
    #[allow(initial_assign)]
    var mem: logic<8>[4];
    initial { $readmemh("mem.hex", mem); }
    assign q = mem[0];
}
#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var q: logic<8>;
    inst dut: Rom (q);
    initial {
        clk.next(1);
        $assert(q == 8'h12);
        $finish();
    }
}
"#,
    );
    let path = project.path();
    fs::write(path.join("mem.hex"), "12\n12\n12\n12\n").unwrap();
    cached(path, &[], "miss", true);
    cached(path, &[], "hit", true);
    // A source-relative file now takes precedence over the project-root file.
    fs::write(path.join("src/mem.hex"), "12\n12\n12\n12\n").unwrap();
    cached(path, &[], "miss", true);
    fs::write(path.join("src/mem.hex"), "34\n34\n34\n34\n").unwrap();
    let output = cached(path, &[], "miss", false);
    assert!(output.contains("status=fail"), "{output}");
    fs::remove_file(path.join("src/mem.hex")).unwrap();
    cached(path, &[], "miss", true);
}

#[test]
fn rebuilds_corrupt_entries_and_tolerates_unwritable_cache() {
    let project = project(SOURCE);
    let path = project.path();
    cached(path, &[], "miss", true);
    let entry = fs::read_dir(path.join("cache"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(entry, "truncated").unwrap();
    let output = cached(path, &[], "miss", true);
    assert!(output.contains("build cache ignored"), "{output}");
    cached(path, &[], "hit", true);
    fs::write(path.join("blocked"), "a file, not a directory").unwrap();
    let output = run(path, &["--build-cache-dir", "blocked"], true);
    assert!(output.contains("build cache write skipped"), "{output}");
}

#[test]
fn rejects_incompatible_modes() {
    let project = project(SOURCE);
    for args in [
        vec!["--backend", "cranelift"],
        vec!["--dump-ir-dir", "trace"],
        vec!["--native-image-input", "image"],
    ] {
        let mut args_with_cache = vec!["--build-cache-dir", "cache"];
        args_with_cache.extend(args);
        let output = run(project.path(), &args_with_cache, false);
        assert!(output.contains("--build-cache-dir requires"), "{output}");
    }
}
