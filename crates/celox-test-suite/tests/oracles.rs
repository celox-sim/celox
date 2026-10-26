//! Live adapter checks. Run explicitly with --ignored; these require external tools.
#![cfg(any(feature = "icarus", feature = "verilator", feature = "xezim"))]
// xezim runs testbenches only; the signal-access checks are not built for it.
#![cfg_attr(
    not(any(feature = "icarus", feature = "verilator")),
    allow(dead_code, unused_imports)
)]
use celox_test_suite::{Backend, BigUint, Design, Result, SignalPath, Simulator};
use std::path::Path;

const DESIGN: &str = r#"
module Child (
    clk: input clock_negedge,
    rst: input reset_async_high,
    d: input logic<130>,
    q: output logic<130>,
    previous: output logic<130>,
) {
    always_ff (clk, rst) {
        if_reset { q = 0; previous = 0; }
        else { q = d; previous = q; }
    }
}
module Top (
    clk: input clock_negedge,
    rst: input reset_async_high,
    d: input logic<130>,
    q: output logic<130>,
    previous: output logic<130>,
) {
    inst child: Child (clk, rst, d, q, previous);
}
"#;

fn signal(name: &str) -> SignalPath {
    SignalPath {
        instances: Vec::new(),
        name: name.into(),
    }
}

fn check_edges_and_wide_values(build: fn(&Design, &Path) -> Result<Box<dyn Backend>>, tool: &str) {
    let dir =
        std::env::temp_dir().join(format!("veryl-suite-adapter-{tool}-{}", std::process::id()));
    let mut backend = build(&Design::new(DESIGN, "Top"), &dir).unwrap();
    let zero = BigUint::default();
    let one = BigUint::from(1u8);
    let wide: BigUint = (&one << 129) | BigUint::from(0xabcdu16);
    backend
        .write(&signal("clk"), one.clone(), zero.clone())
        .unwrap();
    backend
        .write(&signal("rst"), one.clone(), zero.clone())
        .unwrap();
    backend
        .write(&signal("d"), wide.clone(), zero.clone())
        .unwrap();
    // Reads settle pending writes, including the asynchronous reset/NBA queue.
    assert_eq!(
        backend.read(&signal("q")).unwrap(),
        (zero.clone(), zero.clone())
    );
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("rst")).unwrap().0, one);
    assert_eq!(backend.read(&signal("q")).unwrap().0, zero);
    backend
        .write(&signal("rst"), zero.clone(), zero.clone())
        .unwrap();
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("q")).unwrap().0, wide);
    assert_eq!(backend.read(&signal("previous")).unwrap().0, zero);
    backend
        .write(&signal("d"), BigUint::from(7u8), zero.clone())
        .unwrap();
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("previous")).unwrap().0, wide);
    assert_eq!(backend.read(&signal("q")).unwrap().0, BigUint::from(7u8));
    assert!(backend.tick("unknown_clock").is_err());
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator, C++, make and timeout on PATH"]
fn verilator_edges_nba_and_wide_values() {
    check_edges_and_wide_values(
        |d, p| {
            Ok(Box::new(
                celox_test_suite::veryl::verilator::Verilator::build(d, p)?,
            ))
        },
        "verilator",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_edges_nba_and_wide_values() {
    check_edges_and_wide_values(
        |d, p| {
            Ok(Box::new(celox_test_suite::veryl::icarus::Icarus::build(
                d, p,
            )?))
        },
        "icarus",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_preserves_xz_and_array_element_order() {
    let dir = std::env::temp_dir().join(format!("veryl-suite-adapter-xz-{}", std::process::id()));
    let design = Design::new(
        r#"
module Top (a: input logic<70>[2], y: output logic<70>[2]) {
    assign y[0] = a[0];
    assign y[1] = a[1];
}

"#,
        "Top",
    )
    .four_state(true);
    let mut backend = celox_test_suite::veryl::icarus::Icarus::build(&design, &dir).unwrap();
    let payload: BigUint = (BigUint::from(1u8) << 139) | BigUint::from(0x5au8);
    let mask: BigUint =
        (BigUint::from(1u8) << 139) | (BigUint::from(1u8) << 71) | BigUint::from(0xffu8);
    backend
        .write(&signal("a"), payload.clone(), mask.clone())
        .unwrap();
    assert_eq!(backend.read(&signal("y")).unwrap(), (payload, mask));
}

fn check_generated_instance_paths(
    build: fn(&Design, &Path) -> Result<Box<dyn Backend>>,
    tool: &str,
) {
    let directory =
        std::env::temp_dir().join(format!("veryl-suite-paths-{tool}-{}", std::process::id()));
    let design = Design::new(
        r#"
module Child (d: input logic<8>, q: output logic<8>) {
    assign q = d;
}
module Top (
    d: input logic<8>[2], q: output logic<8>[2],
    plain_d: input logic<8>, plain_q: output logic<8>,
) {
    inst plain: Child (d: plain_d, q: plain_q);
    for i in 0..2: unit {
        inst leaf: Child (d: d[i], q: q[i]);
    }
}
"#,
        "Top",
    );
    let mut sim = Simulator::new(build(&design, &directory).unwrap());
    let d = sim.signal("d");
    let plain_d = sim.signal("plain_d");
    sim.modify(|io| {
        io.set(d, 0x3322u16);
        io.set(plain_d, 0x11u8);
    })
    .unwrap();
    let plain = sim.child_signal(&[("plain", None)], "q");
    let first = sim.child_signal(&[("unit", Some(0)), ("leaf", None)], "q");
    let second = sim.child_signal(&[("unit", Some(1)), ("leaf", None)], "q");
    assert_eq!(sim.get_as::<u8>(plain), 0x11);
    assert_eq!(sim.get_as::<u8>(first), 0x22);
    assert_eq!(sim.get_as::<u8>(second), 0x33);
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator, C++, make and timeout on PATH"]
fn verilator_generated_instance_paths() {
    check_generated_instance_paths(
        |d, p| {
            Ok(Box::new(
                celox_test_suite::veryl::verilator::Verilator::build(d, p)?,
            ))
        },
        "verilator",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_generated_instance_paths() {
    check_generated_instance_paths(
        |d, p| {
            Ok(Box::new(celox_test_suite::veryl::icarus::Icarus::build(
                d, p,
            )?))
        },
        "icarus",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus and timeout on PATH"]
fn icarus_rejects_invalid_output_connections() {
    let directory =
        std::env::temp_dir().join(format!("veryl-suite-rejections-{}", std::process::id()));
    let mut checked = 0;
    for case in celox_test_suite::veryl::cases()
        .filter(|case| case.expectation == celox_test_suite::Expectation::CompilationError)
    {
        let output = directory.join(case.name.replace("::", "/"));
        case.run(&mut |design| {
            Ok(Box::new(celox_test_suite::veryl::icarus::Icarus::build(
                design, &output,
            )?))
        });
        checked += 1;
    }
    assert_eq!(checked, 8);
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator and timeout on PATH"]
fn verilator_rejects_invalid_assignment() {
    let directory = std::env::temp_dir().join(format!(
        "veryl-suite-verilator-rejection-{}",
        std::process::id()
    ));
    // The other seven negative cases remain excluded for Verilator's acceptance
    // of dynamic output destinations. This one reports an assignment type error.
    let case = celox_test_suite::veryl::cases()
        .find(|case| {
            case.name == "hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb"
        })
        .unwrap();
    case.run(&mut |design| {
        Ok(Box::new(
            celox_test_suite::veryl::verilator::Verilator::build(design, &directory)?,
        ))
    });
}

fn check_native_testbench_outcomes(
    build: fn(&Design, &Path) -> Result<Box<dyn Backend>>,
    tool: &str,
) {
    for (name, body, expected) in [
        ("pass", "$assert(1'd1); $finish();", true),
        ("assert_fail", "$assert(1'd0); $finish();", false),
        ("no_finish", "$assert(1'd1);", false),
        (
            "forged_completion",
            "$display(\"@suite testbench pass\");",
            false,
        ),
        ("skipped_finish", "if enabled { $finish(); }", false),
        (
            "nested_finish",
            "if !enabled { $finish(); } $assert(1'd0);",
            true,
        ),
    ] {
        let directory = std::env::temp_dir().join(format!(
            "veryl-suite-testbench-{tool}-{name}-{}",
            std::process::id()
        ));
        let source = format!(
            "#[test(Top)] module Top {{ var enabled: logic; initial {{ enabled = 1'd0; {body} }} }}"
        );
        let mut backend = build(&Design::new(&source, "Top"), &directory).unwrap();
        let result = backend.run_testbench();
        assert_eq!(result.is_ok(), expected, "{tool} {name}: {result:?}");
        // A consumed process must never report a second successful execution.
        assert!(backend.run_testbench().is_err());
    }
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_native_testbench_outcomes() {
    check_native_testbench_outcomes(
        |d, p| {
            Ok(Box::new(celox_test_suite::veryl::icarus::Icarus::build(
                d, p,
            )?))
        },
        "icarus",
    );
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator, C++, make and timeout on PATH"]
fn verilator_native_testbench_outcomes() {
    check_native_testbench_outcomes(
        |d, p| {
            Ok(Box::new(
                celox_test_suite::veryl::verilator::Verilator::build(d, p)?,
            ))
        },
        "verilator",
    );
}

/// SystemVerilog sources that are their own native testbench.
#[cfg(feature = "xezim")]
struct NativeSv;

#[cfg(feature = "xezim")]
impl celox_test_suite::Frontend for NativeSv {
    fn source_extension(&self) -> &'static str {
        "sv"
    }

    fn prepare(&self, design: &Design) -> Result<celox_test_suite::PreparedDesign> {
        Ok(celox_test_suite::PreparedDesign {
            sources: design
                .sources
                .iter()
                .map(|source| source.text.clone())
                .collect(),
            top: design.top.clone(),
            native_testbench: true,
            edges: Default::default(),
            info: None,
        })
    }
}

// The Veryl native testbenches call `CELOX_SUITE_FINISH()`, which xezim
// 0.11.3 rejects; the completion rules are checked on SystemVerilog directly.
#[cfg(feature = "xezim")]
#[test]
#[ignore = "requires xezim and timeout on PATH"]
fn xezim_native_testbench_outcomes() {
    for (name, body, expected) in [
        ("pass", "$display(\"done\"); $finish;", true),
        ("fatal", "$fatal(1, \"failed\"); $finish;", false),
        ("error", "$error(\"failed\"); $finish;", false),
        ("no_finish", "$display(\"done\");", false),
        (
            "forged_completion",
            "$display(\"Simulation finished at time 0 ($finish called)\");",
            false,
        ),
        ("partial_line", "$write(\"done\"); $finish;", true),
    ] {
        let directory = std::env::temp_dir().join(format!(
            "sv-suite-testbench-xezim-{name}-{}",
            std::process::id()
        ));
        let mut design = Design::new(
            &format!("module Top; initial begin {body} end endmodule"),
            "Top",
        );
        design.sources[0].path = "test.sv".into();
        let mut backend =
            celox_test_suite::xezim::Xezim::build(&NativeSv, &design, &directory).unwrap();
        let result = backend.run_testbench();
        assert_eq!(result.is_ok(), expected, "xezim {name}: {result:?}");
        assert!(backend.run_testbench().is_err());
    }
}

/// Run a SystemVerilog script case with xezim: `Ok` only when the testbench
/// finished and its checks passed.
#[cfg(feature = "xezim")]
fn xezim_script(name: &str, source: &str, script: &str) -> std::result::Result<(), String> {
    // Four-state, so no storage starts at zero and every edge is the
    // stimulus's own.
    xezim_case(name, source, &format!("(four_state) {script}"))
}

#[cfg(feature = "xezim")]
fn xezim_case(name: &str, source: &str, script: &str) -> std::result::Result<(), String> {
    let text = format!(
        "(group xezim (category sequential)) (case {name} (source \"test.sv\" #\"{source}\"#) (top Top) {script})"
    );
    let case =
        celox_test_suite::load_group("xezim.vtest", &text, celox_test_suite::no_std_library)[0];
    let directory = std::env::temp_dir()
        .join(format!("sv-suite-xezim-{}", std::process::id()))
        .join(name);
    let frontend = celox_test_suite::sv::verification::SvFrontend;
    let mut backend = celox_test_suite::xezim::Xezim::build_script(&frontend, case, &directory)
        .map_err(|error| error.to_string())?;
    let run = backend.run_testbench();
    let checks = celox_test_suite::script::sv::check_run(&directory);
    // The failed assertions, as the runner reports them.
    let asserts: Vec<String> = std::fs::read_to_string(directory.join("protocol.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("@suite assert "))
        .map(str::to_owned)
        .collect();
    match (run, checks) {
        (Ok(()), checks) => checks,
        (Err(error), checks) => Err(format!("{error}: {checks:?}: {asserts:?}")),
    }
}

/// The response to a stimulus takes an NBA update, combinational deltas,
/// a derived-clock edge with a second NBA update and `#0` resumptions, all in
/// one time slot. The program-based settle must observe all of it, at time
/// zero and later, after `run_until` and after writes at the current time.
#[cfg(feature = "xezim")]
#[test]
#[ignore = "requires xezim and timeout on PATH"]
fn xezim_settles_whole_time_slots() {
    const DESIGN: &str = "
        module Top(input logic clk, input logic [7:0] a, output logic [7:0] t2, output logic [7:0] o,
                   output logic [7:0] z, output logic [7:0] z2);
            logic [7:0] r, s, t1;
            logic c2;
            always_ff @(posedge clk) r <= a;
            assign t1 = r + 8'd1;
            always_comb t2 = t1 * 8'd2;
            assign c2 = t2[1];
            always_ff @(posedge c2) s <= t2;
            assign o = s + 8'd1;
            always @(a) begin #0 z = a; #0 z = z + 8'd1; end
            initial begin z2 <= 8'd1; #0 z2 <= 8'd2; #0 z2 = z2 + 8'd5; end
        endmodule";
    let mut script = String::from("(run_until 0) (assert_eq z2 2)");
    // Every write and read is at the current time: settling never advances it.
    let (mut s, mut c2) = (None, None);
    for (time, a) in [(0u32, 3u32), (0, 4), (5, 6), (5, 9)] {
        let t2 = (2 * (a + 1)) & 0xff;
        let bit = t2 >> 1 & 1;
        if c2 != Some(1) && bit == 1 {
            s = Some(t2);
        }
        c2 = Some(bit);
        script.push_str(&format!(
            " (run_until {time}) (modify (set clk 0) (set a {a})) (modify (set clk 1)) \
             (assert_eq t2 {t2}) (assert_eq z {})",
            (a + 1) & 0xff
        ));
        if let Some(s) = s {
            script.push_str(&format!(" (assert_eq o {})", s + 1));
        }
    }
    script.push_str(" (run_until 6)");
    assert_eq!(xezim_script("settle", DESIGN, &script), Ok(()));
    // The checks are live: a wrong expectation fails.
    assert!(xezim_script("settle_live", DESIGN, "(run_until 0) (assert_eq z2 7)").is_err());
}

#[cfg(feature = "xezim")]
#[test]
#[ignore = "requires xezim and timeout on PATH"]
fn xezim_early_finish_and_event_exhaustion_do_not_skip_script_checks() {
    for (name, source, script) in [
        (
            "early",
            "module Top(output bit done); initial #5 $finish; endmodule",
            "(run_until 10) (assert_eq done 1) (run_to_finish)",
        ),
        (
            "finish_at_target",
            "module Top(output bit done); initial #5 $finish; endmodule",
            "(run_until 5) (assert_eq done 0)",
        ),
        (
            "finish_on_eval",
            "module Top(input bit go); initial begin wait(go); $finish; end endmodule",
            "(run_until 0) (set go 1) (eval) (run_to_finish)",
        ),
        (
            "idle",
            "module Top(output bit done); initial done = 1; endmodule",
            "(run_to_finish) (assert_eq done 1)",
        ),
    ] {
        assert!(
            xezim_script(name, source, script).is_err(),
            "xezim accepted {name}"
        );
    }
    // A script can run until any time up to RUN_UNTIL_LIMIT, past xezim's
    // default and any smaller --max-time.
    assert_eq!(
        xezim_script(
            "late",
            "module Top(output bit done); initial done = 1; endmodule",
            "(run_until 9000000000001) (assert_eq done 1)"
        ),
        Ok(())
    );
    let finished = "module Top(output bit done); initial begin done = 0; #5 done = 1; #5 $finish; end endmodule";
    assert_eq!(
        xezim_script(
            "finished",
            finished,
            "(run_until 5) (assert_eq done 1) (run_to_finish) (assert_eq done 1)"
        ),
        Ok(())
    );
}

/// A two-state run starts all storage at zero, but xezim leaves variables
/// that no flip-flop or memory initialization covers at X. Reading one fails
/// the run instead of letting `!==`, conditions and `&&`/`||` decide on X.
#[cfg(feature = "xezim")]
#[test]
#[ignore = "requires xezim and timeout on PATH"]
fn xezim_two_state_runs_reject_unknown_reads() {
    const DESIGN: &str = "
        module Top(input logic [7:0] a, output logic [7:0] o, output logic [7:0] y);
            logic [7:0] never;
            assign o = never;
            assign y = a;
        endmodule";
    for (name, script) in [
        ("ne", "(eval) (assert (!= (get o) 0))"),
        ("condition", "(eval) (if (== (get o) 0) (assert_eq y 1))"),
        ("short_circuit", "(eval) (assert (|| (== (get o) 0) 1))"),
    ] {
        let result = xezim_case(name, DESIGN, script);
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("unknown bits in a two-state run")),
            "{name}: {result:?}"
        );
    }
    assert_eq!(
        xezim_case("known", DESIGN, "(set a 5) (eval) (assert_eq y 5)"),
        Ok(())
    );
}
