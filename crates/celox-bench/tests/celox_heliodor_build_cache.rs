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

const MEMORY_SOURCE: &str = r#"
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

fn run(project: &Path, args: &[&str], success: bool) -> String {
    run_with_env(project, args, success, &[])
}

fn run_with_env(
    project: &Path,
    args: &[&str],
    success: bool,
    environment: &[(&str, &std::ffi::OsStr)],
) -> String {
    run_with_project(project, project, args, success, environment)
}

#[allow(clippy::disallowed_methods)] // Isolate child-process environment in integration tests.
fn run_with_project(
    working_dir: &Path,
    project: &Path,
    args: &[&str],
    success: bool,
    environment: &[(&str, &std::ffi::OsStr)],
) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_celox-heliodor"));
    // Isolate the test from user diagnostics and codegen environment switches.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CELOX_") {
            command.env_remove(name);
        }
    }
    let output = command
        .envs(environment.iter().copied())
        .current_dir(working_dir)
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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn invalidates_parallel_compilation_configuration() {
    let project = project(SOURCE);
    for (mode, lanes, status) in [
        ("off", "2", "miss"),
        ("auto", "2", "miss"),
        ("auto", "8", "miss"),
        ("auto", "2", "hit"),
        ("off", "2", "hit"),
    ] {
        let output = run_with_env(
            project.path(),
            &["--build-cache-dir", "cache", "--compile-only"],
            true,
            &[
                ("CELOX_PARALLEL", mode.as_ref()),
                ("CELOX_PARALLEL_PARTITIONS", lanes.as_ref()),
            ],
        );
        assert!(
            output.contains(&format!("CELOX_BUILD_CACHE test=t status={status}")),
            "{output}"
        );
    }
}

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    not(target_feature = "crt-static")
))]
#[test]
#[allow(clippy::disallowed_macros)] // Report a host-specific test exclusion.
fn invalidates_changed_os_native_features() {
    // This regression needs a host where normal codegen uses the GS state base.
    if celox::native_backend::features::detected_image_feature_bits() & (1 << 3) == 0 {
        eprintln!("skipping FSGSBASE regression: host uses the baseline state base");
        return;
    }
    let project = project(SOURCE);
    let path = project.path();
    let shim_source = path.join("mask_fsgsbase.c");
    let shim = path.join("mask_fsgsbase.so");
    fs::write(
        &shim_source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <sys/auxv.h>

unsigned long getauxval(unsigned long type) {
    unsigned long (*original)(unsigned long) = dlsym(RTLD_NEXT, "getauxval");
    unsigned long value = original(type);
    /* Mask Linux's OS-enabled FSGSBASE capability, leaving other facts intact. */
    return type == AT_HWCAP2 ? value & ~2UL : value;
}
"#,
    )
    .unwrap();
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&shim)
        .arg(&shim_source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    cached(path, &[], "miss", true);
    cached(path, &[], "hit", true);
    for status in ["miss", "hit"] {
        let output = run_with_env(
            path,
            &["--build-cache-dir", "cache"],
            true,
            &[("LD_PRELOAD", shim.as_os_str())],
        );
        assert!(
            output.contains(&format!("CELOX_BUILD_CACHE test=t status={status}")),
            "{output}"
        );
        assert!(output.contains("CELOX_TEST_RESULT test=t status=pass"));
    }
    // Restoring the original capabilities reuses the original compilation.
    cached(path, &[], "hit", true);
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
    let project = project(MEMORY_SOURCE);
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
fn absolute_readmem_path_keeps_its_original_location_after_project_move() {
    let fixture = project(MEMORY_SOURCE);
    let parent = tempfile::tempdir().unwrap();
    let old = parent.path().join("original-project");
    let moved = parent.path().join("relocated-project");
    fs::rename(fixture.path(), &old).unwrap();
    fs::write(old.join("mem.hex"), "12\n12\n12\n12\n").unwrap();
    let source = MEMORY_SOURCE.replace("mem.hex", old.join("mem.hex").to_str().unwrap());
    fs::write(old.join("src/test.veryl"), source).unwrap();
    cached(&old, &[], "miss", true);
    cached(&old, &[], "hit", true);
    fs::rename(&old, &moved).unwrap();
    // The literal still names the original location, which no longer exists.
    cached(&moved, &[], "miss", false);
    // Restore that location with different contents: the cache must also miss
    // while a fresh compilation follows the unchanged literal.
    fs::create_dir(&old).unwrap();
    fs::write(old.join("mem.hex"), "34\n34\n34\n34\n").unwrap();
    cached(&moved, &[], "miss", false);
}

#[test]
fn reuses_relative_cache_after_project_move() {
    let nested = SOURCE.replace(
        "$assert(q == 4);",
        "if q == 4 { for _i in 0..2 { $assert(q == 4); } } else { $assert(q == 4); }",
    );
    for source in [&nested, MEMORY_SOURCE] {
        check_project_move(source);
    }
}

fn check_project_move(source: &str) {
    let fixture = project(source);
    fs::write(fixture.path().join("mem.hex"), "12\n12\n12\n12\n").unwrap();
    let parent = tempfile::tempdir().unwrap();
    let old = parent.path().join("private-original-project");
    let moved = parent.path().join("relocated-project");
    fs::rename(fixture.path(), &old).unwrap();
    // Absolute path settings describing the same project-relative inputs must
    // retain their identity when the checkout moves.
    let metadata = format!(
        "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[build]\nsources = [{}]\n",
        serde_json::to_string(&old.join("src")).unwrap()
    );
    fs::write(old.join("Veryl.toml"), &metadata).unwrap();
    cached(&old, &[], "miss", true);
    let entry = fs::read_dir(old.join("cache"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = fs::read(entry).unwrap();
    assert!(
        !bytes
            .windows(old.as_os_str().as_encoded_bytes().len())
            .any(|window| window == old.as_os_str().as_encoded_bytes())
    );
    fs::rename(&old, &moved).unwrap();
    fs::write(
        moved.join("Veryl.toml"),
        metadata.replace(old.to_str().unwrap(), moved.to_str().unwrap()),
    )
    .unwrap();
    let output = cached(&moved, &[], "hit", true);
    assert!(output.contains("CELOX_TEST_RESULT test=t status=pass"));
    // Diagnostic paths and the component file base must now refer to the new root.
    let image_path = moved.join("rebound.image");
    cached(
        &moved,
        &[
            "--compile-only",
            "--native-image-output",
            image_path.to_str().unwrap(),
        ],
        "hit",
        true,
    );
    let image = fs::read(&image_path).unwrap();
    assert!(
        !image
            .windows(old.as_os_str().as_encoded_bytes().len())
            .any(|window| window == old.as_os_str().as_encoded_bytes())
    );
    assert!(
        !image
            .windows(moved.as_os_str().as_encoded_bytes().len())
            .any(|window| window == moved.as_os_str().as_encoded_bytes())
    );
    run(
        &moved,
        &["--native-image-input", image_path.to_str().unwrap()],
        true,
    );
    if source == MEMORY_SOURCE {
        // Reload candidates against the relocated root, including a newly
        // created source-relative file that outranks the root-relative file.
        fs::write(moved.join("src/mem.hex"), "34\n34\n34\n34\n").unwrap();
        cached(&moved, &[], "miss", false);
    }
    fs::remove_dir_all(moved.join("src")).unwrap();
    fs::remove_file(moved.join("Veryl.toml")).unwrap();
    run(
        &moved,
        &["--native-image-input", image_path.to_str().unwrap()],
        true,
    );
}

#[cfg(unix)]
#[test]
fn tracks_retargeted_readmem_directory_symlink() {
    let fixture = project(&MEMORY_SOURCE.replace("mem.hex", "data/mem.hex"));
    let root = fixture.path();
    for (directory, contents) in [("good", "12\n12\n12\n12\n"), ("bad", "34\n34\n34\n34\n")] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("mem.hex"), contents).unwrap();
    }
    let alias = root.join("src/data");
    std::os::unix::fs::symlink("../good", &alias).unwrap();
    cached(root, &[], "miss", true);
    cached(root, &[], "hit", true);
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("../bad", &alias).unwrap();
    let output = cached(root, &[], "miss", false);
    assert!(output.contains("status=fail"));
}

#[cfg(unix)]
#[test]
fn invalidates_namespace_changes_when_project_alias_moves() {
    let parent = tempfile::tempdir().unwrap();
    let mut projects = Vec::new();
    for _ in 0..2 {
        let project = project(
            r#"
#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var q: logic<32>;
    inst dut: dep::Dep(q);
    initial { clk.next(1); $assert(q == 4); $finish(); }
}
"#,
        );
        fs::write(project.path().join("Veryl.toml"),
            "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { path = \"dep\" }\n").unwrap();
        fs::create_dir_all(project.path().join("dep/src")).unwrap();
        fs::write(
            project.path().join("dep/Veryl.toml"),
            "[project]\nname = \"dep\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            project.path().join("dep/src/dep.veryl"),
            "pub module Dep(q: output logic<32>) { assign q = 4; }",
        )
        .unwrap();
        projects.push(project);
    }
    let alias = parent.path().join("current");
    std::os::unix::fs::symlink(projects[0].path(), &alias).unwrap();
    let dependency = projects[0].path().join("dep/src/dep.veryl");
    let cache = parent.path().join("cache");
    // Keep the working directory and explicitly supplied source paths fixed.
    let args = [
        "--source-file",
        dependency.to_str().unwrap(),
        "--build-cache-dir",
        cache.to_str().unwrap(),
    ];
    let output = run_with_project(parent.path(), &alias, &args, true, &[]);
    assert!(output.contains("status=miss"));
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(projects[1].path(), &alias).unwrap();
    let output = run_with_project(parent.path(), &alias, &args, false, &[]);
    assert!(output.contains("status=miss"), "{output}");
    assert!(output.contains("unknown_member"), "{output}");
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

#[test]
fn invalidates_resolved_dependency_properties() {
    let project = project(
        r#"
#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var q: logic<32>;
    inst dut: dep::Dep (q);
    initial { clk.next(1); $assert(q == 4); $finish(); }
}
"#,
    );
    let path = project.path();
    fs::write(
        path.join("Veryl.toml"),
        "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { path = \"dep\" }\n",
    )
    .unwrap();
    fs::create_dir_all(path.join("dep/src")).unwrap();
    fs::write(
        path.join("dep/src/dep.veryl"),
        "pub module Dep (q: output logic<32>) { assign q = $prop::VALUE; }",
    )
    .unwrap();
    let metadata = "[project]\nname = \"dep\"\nversion = \"0.1.0\"\n[properties]\nVALUE = 4\n";
    fs::write(path.join("dep/Veryl.toml"), metadata).unwrap();
    let args = ["--source-file", "dep/src/dep.veryl"];
    cached(path, &args, "miss", true);
    cached(path, &args, "hit", true);
    fs::write(
        path.join("dep/Veryl.toml"),
        metadata.replace("VALUE = 4", "VALUE = 5"),
    )
    .unwrap();
    // The root metadata and all supplied sources stay identical, but the
    // refreshed dependency property must affect the generated design.
    let output = cached(path, &args, "miss", false);
    assert!(output.contains("status=fail"), "{output}");
    cached(path, &args, "hit", false);
    fs::write(path.join("dep/Veryl.toml"), metadata).unwrap();
    cached(path, &args, "hit", true);
}

#[test]
fn invalidates_component_manifest_contents_candidates_and_precedence() {
    check_component_manifest_invalidation(false);
}

#[test]
fn invalidates_dependency_component_manifests() {
    check_component_manifest_invalidation(true);
}

#[test]
fn invalidates_native_component_library_presence() {
    check_native_component_library_presence(false);
}

#[test]
fn invalidates_dependency_native_component_library_presence() {
    check_native_component_library_presence(true);
}

fn check_native_component_library_presence(dependency: bool) {
    for override_name in [false, true] {
        let export = if dependency { "dep::demo" } else { "demo" };
        let project = project(&format!(
            "#[test(t)]\nmodule t {{ var component: $comp::{export}; initial {{ component.ping(); $finish(); }} }}"
        ));
        let path = project.path();
        let root = if dependency {
            fs::write(
                path.join("Veryl.toml"),
                "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { path = \"dep\" }\n",
            )
            .unwrap();
            path.join("dep")
        } else {
            path.to_path_buf()
        };
        fs::create_dir_all(root.join("comp")).unwrap();
        fs::write(
            root.join("Veryl.toml"),
            "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[[components]]\npath = \"comp\"\n",
        )
        .unwrap();
        let cargo = "[package]\nname = \"demo-comp\"\nversion = \"0.1.0\"\n";
        fs::write(
            root.join("comp/Cargo.toml"),
            if override_name {
                format!("{cargo}[lib]\nname = \"custom_component\"\n")
            } else {
                cargo.to_owned()
            },
        )
        .unwrap();
        fs::write(
            root.join("comp/veryl.manifest.json"),
            r#"{"types":{"demo":{"kind":"dynamic"}}}"#,
        )
        .unwrap();
        let args = ["--compile-only", "--native-image-output", "cached.image"];
        cached(path, &args, "miss", true);
        let without_library = fs::read(path.join("cached.image")).unwrap();
        cached(path, &args, "hit", true);

        let name = if override_name {
            "custom_component"
        } else {
            "demo_comp"
        };
        let native = root.join("target/veryl-components/release").join(format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            name,
            std::env::consts::DLL_SUFFIX
        ));
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        // Compile-only records the path without loading the library's ABI.
        fs::write(&native, "native library placeholder").unwrap();
        cached(path, &args, "miss", true);
        cached(path, &args, "hit", true);
        run(
            path,
            &["--compile-only", "--native-image-output", "fresh.image"],
            true,
        );
        let with_library = fs::read(path.join("cached.image")).unwrap();
        assert_ne!(with_library, without_library);
        assert_eq!(with_library, fs::read(path.join("fresh.image")).unwrap());

        // Changes to library code do not change the embedded runtime path.
        fs::write(&native, "updated native library placeholder").unwrap();
        cached(path, &args, "hit", true);
        fs::remove_file(&native).unwrap();
        cached(path, &args, "hit", true);
        assert_eq!(
            fs::read(path.join("cached.image")).unwrap(),
            without_library
        );
        // The compiler uses is_file(), so a directory is also an absent library.
        fs::create_dir(&native).unwrap();
        cached(path, &args, "hit", true);
        fs::remove_dir(&native).unwrap();
        fs::write(&native, "native library placeholder").unwrap();
        cached(path, &args, "hit", true);
        for entry in fs::read_dir(path.join("cache")).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            assert!(
                !bytes
                    .windows(path.as_os_str().as_encoded_bytes().len())
                    .any(|window| window == path.as_os_str().as_encoded_bytes())
            );
        }
        // Library paths, component source locations, and the runtime file base
        // are rebound together for both root and dependency components.
        let destination = tempfile::tempdir().unwrap();
        let moved = destination.path().join("project");
        fs::rename(path, &moved).unwrap();
        cached(&moved, &args, "hit", true);
        run(
            &moved,
            &["--compile-only", "--native-image-output", "fresh.image"],
            true,
        );
        assert_eq!(
            fs::read(moved.join("cached.image")).unwrap(),
            fs::read(moved.join("fresh.image")).unwrap()
        );
    }
}

fn check_component_manifest_invalidation(dependency: bool) {
    let source = r#"
#[test(t)]
module t {
    var component: $comp::demo;
    initial { component.ping(); $finish(); }
}
"#;
    let source = if dependency {
        source.replace("$comp::demo", "$comp::dep::demo")
    } else {
        source.to_owned()
    };
    let project = project(&source);
    let path = project.path();
    let root = if dependency {
        fs::write(
            path.join("Veryl.toml"),
            "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { path = \"dep\" }\n",
        )
        .unwrap();
        path.join("dep")
    } else {
        path.to_path_buf()
    };
    fs::create_dir_all(root.join("comp")).unwrap();
    fs::write(
        root.join("Veryl.toml"),
        "[project]\nname = \"cache_test\"\nversion = \"0.1.0\"\n[[components]]\npath = \"comp\"\n",
    )
    .unwrap();
    fs::write(
        root.join("comp/Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let committed = root.join("comp/veryl.manifest.json");
    let valid = r#"{"types":{"demo":{"kind":"dynamic"}}}"#;
    let removed = r#"{"types":{"other":{"kind":"dynamic"}}}"#;
    fs::write(&committed, valid).unwrap();
    let args = ["--compile-only"];
    cached(path, &args, "miss", true);
    cached(path, &args, "hit", true);
    fs::write(&committed, removed).unwrap();
    let output = cached(path, &args, "miss", false);
    assert!(output.contains("unknown_member"), "{output}");

    let sidecar = root.join("target/veryl-components/release/demo.manifest.json");
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(&sidecar, valid).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    fs::File::open(&sidecar)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(future))
        .unwrap();
    cached(path, &args, "miss", true);
    cached(path, &args, "hit", true);
    // Only mtime changes: the committed manifest now wins over the sidecar.
    fs::File::open(&committed)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(future + std::time::Duration::from_secs(1)))
        .unwrap();
    cached(path, &args, "miss", false);
    fs::remove_file(committed).unwrap();
    cached(path, &args, "miss", true);
    cached(path, &args, "hit", true);
}
