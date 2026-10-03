//! Celox-specific partition/codegen/worker regression. Each runtime configuration
//! runs in a subprocess so environment selection cannot race other Rust tests.
#![cfg(all(
    target_arch = "x86_64",
    target_os = "linux",
    not(feature = "arm64-codegen")
))]
// This binary is the subprocess application boundary for environment-isolated tests.
#![allow(clippy::disallowed_methods)]

use celox::{NativeProgramImage, Simulator};
use std::process::{Command, Stdio};

fn design(style: &str) -> String {
    if style == "identity" {
        // A consumer needs the old producer value, even if a partition updates
        // the producer before the consumer reads the intermediate publication.
        let mut code = String::from(
            "module Producer (clk: input clock, q: output logic<64>) { always_ff (clk) { q = q + 1; } } module Consumer (clk: input clock, value: input logic<32>, q: output logic<32>) { always_ff (clk) { q = value; } } #[test(Top)] module Top { inst clk: $tb::clock_gen;",
        );
        for i in 0..24 {
            code.push_str(&format!("var q{i}: logic<64>; var saved{i}: logic<32>; let copy{i}: logic<32> = q{i}[31:0]; inst producer_{i}: Producer (clk, q: q{i}); inst consumer_{i}: Consumer (clk, value: copy{i}, q: saved{i});"));
        }
        code.push_str("initial { for i in 0..768 { clk.next(1);");
        for i in 0..24 {
            code.push_str(&format!("$assert(q{i} == i+1); $assert(saved{i} == i);"));
        }
        code.push_str("} $finish(); } }");
        return code;
    }
    if style == "serial" {
        return "#[test(Top)] module Top { inst clk: $tb::clock_gen; var q: logic<32>; always_ff (clk) { q = q + 1; } initial { clk.next(768); $assert(q == 768); $finish(); } }".to_owned();
    }
    let hierarchical = style != "flat";
    let lanes = 24;
    // Neither form contains the old instance-name partition hint.
    let mut code = if hierarchical {
        String::from(
            "module Lane #(param ID: u32 = 0) (clk: input clock, clear: input logic, prev: input logic<32>, q: output logic<32>, d: output logic<32>) { var next: logic<32>; assign next = prev + 1; always_ff (clk) { if clear { q = 0; d = 0; } else { q = next; d = q; } } }",
        )
    } else {
        String::new()
    };
    if style == "effects" {
        code = code.replace(
            "q = next; d = q;",
            "q = next; d = q; if q == 3 { $display(\"OBS %d\", ID); }",
        );
    }
    if style == "memory" {
        code = code
            .replace("d: output logic<32>)", "d: output logic<32>, m: output logic<32>)")
            .replace("var next: logic<32>;", "var mem: logic<32>[8]; var index: logic<3>; assign m = mem[index]; var next: logic<32>;")
            .replace("q = 0; d = 0;", "q = 0; d = 0; index = 0; for j in 0..8 { mem[j] = 0; }")
            .replace("q = next; d = q;", "q = next; d = q; mem[index] = next; index = index + 1;");
    }
    code.push_str("#[test(Top)] module Top { inst clk: $tb::clock_gen; var clear: logic;");
    for i in 0..lanes {
        code.push_str(&format!("var q{i}: logic<32>; var d{i}: logic<32>;"));
        if style == "memory" {
            code.push_str(&format!("var m{i}: logic<32>;"));
        }
    }
    for i in 0..lanes {
        let prev = (i + lanes - 1) % lanes;
        if hierarchical {
            let memory_port = if style == "memory" {
                format!(", m: m{i}")
            } else {
                String::new()
            };
            code.push_str(&format!(
                "inst arbitrary_{i}: Lane #(ID: {i}) (clk, clear, prev: q{prev}, q: q{i}, d: d{i}{memory_port});"
            ));
        } else {
            code.push_str(&format!("var next{i}: logic<32>; assign next{i} = q{prev} + 1; always_ff (clk) {{ if clear {{ q{i} = 0; d{i} = 0; }} else {{ q{i} = next{i}; d{i} = q{i}; }} }}"));
        }
    }
    code.push_str("initial { for round in 0..3 { clear = 1; clk.next(2); clear = 0; for i in 0..128 { clk.next(2);");
    for i in 0..lanes {
        code.push_str(&format!(
            "$assert(q{i} == 2*(i+1)); $assert(d{i} == 2*(i+1)-1);"
        ));
        if style == "memory" {
            code.push_str(&format!(
                "if i >= 3 {{ $assert(m{i} == 2*(i+1)-7); }} else {{ $assert(m{i} == 0); }}"
            ));
        }
    }
    code.push_str("} } $finish(); } }");
    code
}

#[test]
fn parallel_image_preserves_flat_and_hierarchical_dependencies_and_effects() {
    let dir = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let cpus = unsafe {
        let mut mask: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask),
            0
        );
        (libc::CPU_COUNT(&mask) as usize)
            .min(std::thread::available_parallelism().map_or(1, usize::from))
    };
    for style in [
        "flat",
        "hierarchical",
        "effects",
        "memory",
        "identity",
        "serial",
    ] {
        let run = |phase: &str, mode: &str| {
            let mut command = Command::new(&executable);
            command.args(["--exact", "parallel_subprocess", "--nocapture"]);
            for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("CELOX_PARALLEL")) {
                command.env_remove(key);
            }
            command
                .env("CELOX_PARALLEL_TEST_PHASE", phase)
                .env("CELOX_PARALLEL_TEST_DIR", dir.path())
                .env("CELOX_PARALLEL_TEST_STYLE", style)
                .env("CELOX_PARALLEL_RUNTIME", mode)
                .env("CELOX_PARALLEL_DIAGNOSTICS", "1")
                .env("CELOX_PARALLEL_PARTITIONS", "16")
                .env("CELOX_PARALLEL_MAX_WORKERS", cpus.to_string())
                .env("CELOX_PARALLEL_WINDOW", "4")
                .env("CELOX_PARALLEL_COOLDOWN", "32");
            if phase == "compile" {
                command.env("CELOX_PARALLEL_PARTITION", dir.path().join(style));
            }
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "{style}/{phase}/{mode} timed out: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{phase}/{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let events = String::from_utf8_lossy(&output.stderr)
                .lines()
                .filter_map(|line| line.split_once("OBS ").map(|(_, value)| value.to_owned()))
                .collect::<Vec<_>>();
            (String::from_utf8_lossy(&output.stderr).into_owned(), events)
        };
        run("compile", "off");
        let (_, expected_events) = run("execute", "off");
        if style == "effects" {
            assert_eq!(expected_events.len(), 24 * 3);
        }

        for width in [1, 2, 4, 8, 16].into_iter().filter(|&w| w <= cpus) {
            let (log, events) = run("execute", &format!("spin{width}"));
            assert_eq!(events, expected_events, "{style}/spin{width} event order");
            assert!(
                log.contains(&format!("CELOX_PARALLEL_POOL width={width} ")),
                "{log}"
            );
        }
        let (log, events) = run("execute", "auto");
        assert_eq!(events, expected_events, "{style}/auto event order");
        if style == "serial" {
            assert!(
                log.contains("CELOX_PARALLEL_AUTO max_useful_width=1\n"),
                "{log}"
            );
        } else if cpus > 1 {
            // Count groups with real memory work, independently of the runtime's
            // width metadata, so empty marker groups cannot make this pass.
            let mut max_work = 0;
            for entry in std::fs::read_dir(dir.path().join(style)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("tsv") {
                    continue;
                }
                let mut waves = std::collections::BTreeMap::<usize, usize>::new();
                for row in std::fs::read_to_string(path).unwrap().lines().skip(1) {
                    let fields = row.split('\t').collect::<Vec<_>>();
                    if fields[2].parse::<usize>().unwrap() + fields[3].parse::<usize>().unwrap() > 0
                    {
                        *waves.entry(fields[1].parse().unwrap()).or_default() += 1;
                    }
                }
                max_work = max_work.max(waves.into_values().max().unwrap_or(0));
            }
            assert!(max_work > 1, "no actual parallel memory work in {style}");
            assert!(log.contains("CELOX_PARALLEL_AUTO max_useful_width="));
            assert!(
                !log.contains("CELOX_PARALLEL_AUTO max_useful_width=1\n"),
                "no concurrent work in {style}: {log}"
            );
        }
    }
}

#[test]
fn parallel_subprocess() {
    let Ok(phase) = std::env::var("CELOX_PARALLEL_TEST_PHASE") else {
        return;
    };
    let dir = std::path::PathBuf::from(std::env::var_os("CELOX_PARALLEL_TEST_DIR").unwrap());
    let path = dir.join("flat.celox");
    if phase == "compile" {
        Simulator::builder(
            &design(&std::env::var("CELOX_PARALLEL_TEST_STYLE").unwrap()),
            "Top",
        )
        .four_state(false)
        .compile_native()
        .unwrap()
        .write_image(path)
        .unwrap();
        return;
    }
    let image = NativeProgramImage::from_container_bytes(&std::fs::read(path).unwrap()).unwrap();
    let mut sim = Simulator::from_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    let tb = celox::testbench::compile_initial_testbench(&sim).unwrap();
    sim.start_native_execution_timing();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut sim, &tb),
        celox::TestResult::Pass
    );
    let timing = sim.finish_native_execution_timing().unwrap();
    if std::env::var("CELOX_PARALLEL_RUNTIME")
        .unwrap()
        .starts_with("spin")
    {
        assert!(
            timing.calls() >= 768,
            "parallel native calls were not recorded: {timing:?}"
        );
    }
}
