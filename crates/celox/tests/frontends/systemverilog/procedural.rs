use super::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native_merged_optimization_preserves_large_and_wide_array_boundaries() {
    for width in [58, 65, 129] {
        let source = format!(
            r#"
            module Top(input logic [7:0] index, output logic [{top}:0] y);
                function [{width}:0] identity(input logic [7:0] selected);
                    logic [{top}:0] words[{top}:0];
                    for (integer i=0; i<{width}; i=i+1) begin
                        words[i]='0;
                        words[i][i]=1'b1;
                    end
                    identity[{top}:0]=words[selected];
                endfunction
                assign y=identity(index);
            endmodule
            "#,
            top = width - 1,
        );
        for four_state in [false, true] {
            for level in [
                celox::OptLevel::O0,
                celox::OptLevel::O1,
                celox::OptLevel::O2,
            ] {
                let mut sim = Simulator::from_sv_sources(
                    vec![(&source, Path::new("native_array_boundaries.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .opt_level(level)
                .build_native()
                .unwrap();
                let index = sim.signal("index");
                let y = sim.signal("y");
                for bit in 0..width {
                    sim.modify(|io| io.set(index, bit as u32)).unwrap();
                    assert_eq!(
                        sim.get(y),
                        BigUint::from(1u8) << bit,
                        "width={width}, four_state={four_state}, level={level:?}, bit={bit}",
                    );
                }
            }
        }
    }
}

sv_backends! {
    // A `break` in a named block that a `disable` names leaves the whole
    // multidimensional foreach, as one outside the block does (IEEE
    // 1800-2023 9.6.2, 12.8). Icarus 13.0 leaves only the innermost
    // dimension, so the case is not a shared one.
    fn disable_block_keeps_foreach_break(sim) {
        @setup { let source = r#"
module Top(output int count);
  int m[2][3];
  initial begin
    foreach (m[i, j]) begin : body
      if (j == 2) disable body;
      count++;
      if (i == 0 && j == 1) break;
    end
  end
endmodule
"#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("disable_block_keeps_foreach_break.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("count")), 2u32.into());
    }
    // A sibling `disable` that runs before the fork branch it names has
    // started finds a block that is not executing, so it has no effect and
    // the branch then runs (IEEE 1800-2023 9.6.2). The order in which the
    // branches of a fork start is not specified (4.7), and Icarus 13.0 ends
    // the branch instead, so the case is not a shared one.
    fn disable_before_fork_branch_starts_has_no_effect(sim) {
        @setup { let source = r#"
`timescale 1ns/1ns
module Top(output int ran, output int late);
  initial begin
    fork
      disable second;
      begin : second
        ran = 1;
        #5;
        late = 1;
      end
    join
  end
endmodule
"#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("disable_before_fork_branch_starts_has_no_effect.sv"))], "Top");
        sim.run_until(10).unwrap();
        assert_eq!(sim.get(sim.signal("ran")), 1u32.into());
        assert_eq!(sim.get(sim.signal("late")), 1u32.into());
    }

}

sv_backends! {
    fn static_function_real_storage_restores_checkpoint(sim) {
        @setup { let source = r#"
            module Top(input bit clk, output logic [63:0] value);
                function real step();
                    static real saved = 0.25;
                    saved += 0.5;
                    return saved;
                endfunction
                always_ff @(posedge clk) value <= $realtobits(step());
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("static_checkpoint.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        let value = sim.signal("value");
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), num_bigint::BigUint::from(0.75f64.to_bits()));
        let checkpoint = sim.checkpoint().unwrap();
        sim.tick(clk).unwrap();
        let next = sim.get(value);
        assert_eq!(next, num_bigint::BigUint::from(1.25f64.to_bits()));
        sim.restore(&checkpoint).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), next);
    }
}

/// Two `always @*` blocks that use a module-level variable as their loop
/// index (#2129). Each block's writes to `j` wake the other, which reads it
/// (IEEE 1800-2023 9.4.2.2); whether they settle depends on the permitted
/// Active-region interleaving (4.7).
const SHARED_INDEX: &str = r#"`timescale 1ns/1ns
module top;
  reg [3:0] a = 4'b0000;
  reg [3:0] b = 4'b0000;
  reg any_a, any_b;
  integer j;
  always @(*) begin
    any_a = 1'b0;
    for (j = 0; j < 4; j = j + 1) any_a = any_a | a[j];
  end
  always @(*) begin
    any_b = 1'b0;
    for (j = 0; j < 4; j = j + 1) any_b = any_b | b[j];
  end
  initial begin
    #1 a = 4'b0100;
    #1 b = 4'b0010;
    #1 $display("%b %b", any_a, any_b);
    $finish;
  end
endmodule
"#;

fn shared_index(
    source: &str,
    level: Option<celox::DiagnosticLevel>,
) -> Result<celox::Simulation, celox::SimulatorError> {
    let mut policy = celox::DiagnosticPolicy::default();
    if let Some(level) = level {
        policy.set("shared_always_temporary", level);
    }
    celox::Simulation::from_sv_sources(vec![(source, Path::new("shared.sv"))], "top")
        .diagnostic_policy(policy)
        .build()
}

fn display_lines(sim: &mut celox::Simulation) -> Vec<String> {
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

/// The time the blocks keep waking each other, and both blocks.
fn assert_does_not_settle(mut sim: celox::Simulation) {
    let error = sim.run_until(10).unwrap_err().to_string();
    assert!(error.contains("does not settle"), "{error}");
    assert!(error.contains("shared.sv:7:"), "{error}");
    assert!(error.contains("shared.sv:11:"), "{error}");
}

#[test]
fn level_blocks_sharing_an_isolated_loop_index_can_settle() {
    // 4.7 permits other Active work between the body and the next @* wait.
    // Shared storage and event notifications remain; this is not privatization.
    for (four_state, mode) in [false, true].into_iter().flat_map(|four_state| {
        [
            celox::SchedulerMode::Compiled,
            celox::SchedulerMode::Differential,
            celox::SchedulerMode::DifferentialRuns,
        ]
        .map(move |mode| (four_state, mode))
    }) {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(SHARED_INDEX, Path::new("shared.sv"))], "top")
                .four_state(four_state)
                .scheduler_mode(mode)
                .build()
                .unwrap();
        assert!(sim.warnings().is_empty());
        sim.run_until(10).unwrap();
        assert!(
            sim.scheduler_stats().compiled_slots > 0,
            "{mode:?} four_state={four_state}: {:?}",
            sim.scheduler_stats()
        );
        assert_eq!(display_lines(&mut sim), ["1 1"]);
        let j = sim.signal("j");
        assert_eq!(sim.get(j), BigUint::from(4u8));
    }
}

#[test]
fn shared_always_rearms_before_flip_flop_nba_updates() {
    let source = SHARED_INDEX
        .replace("reg any_a, any_b;", "reg any_a, any_b; reg clk=0;")
        .replace("#1 a = 4'b0100;", "#1 a = 4'b0100; clk=1;")
        .replace("#1 b = 4'b0010;", "")
        .replace(
            "  initial begin",
            "  always @(posedge clk) b <= 4'b0010;\n  initial begin",
        );
    for (four_state, mode) in [false, true].into_iter().flat_map(|four_state| {
        [
            celox::SchedulerMode::Compiled,
            celox::SchedulerMode::Differential,
            celox::SchedulerMode::DifferentialRuns,
        ]
        .map(move |mode| (four_state, mode))
    }) {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("shared-nba.sv"))], "top")
                .four_state(four_state)
                .scheduler_mode(mode)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        assert!(
            sim.scheduler_stats().compiled_slots > 0,
            "{mode:?} four_state={four_state}: {:?}",
            sim.scheduler_stats()
        );
        assert_eq!(display_lines(&mut sim), ["1 1"]);
    }
}

#[test]
fn shared_always_active_continuations_survive_stop() {
    let source = SHARED_INDEX.replace("endmodule", "initial #1 $stop;\nendmodule");
    for four_state in [false, true] {
        let build = || {
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("shared-stop.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap()
        };
        let mut sim = build();
        sim.run_until(10).unwrap();
        assert!(sim.is_stopped());
        sim.run_until(10).unwrap();
        assert_eq!(display_lines(&mut sim), ["1 1"]);
    }
}

#[test]
fn shared_always_temporary_runs_each_block_with_its_own_index() {
    // Icarus 13 prints `1 1`: it does not wake a block on the other's writes.
    for four_state in [false, true] {
        let mut policy = celox::DiagnosticPolicy::default();
        policy.set("shared_always_temporary", celox::DiagnosticLevel::Allow);
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(SHARED_INDEX, Path::new("shared.sv"))], "top")
                .four_state(four_state)
                .diagnostic_policy(policy)
                .build()
                .unwrap();
        assert!(sim.warnings().is_empty());
        sim.run_until(10).unwrap();
        assert_eq!(display_lines(&mut sim), ["1 1"], "four_state={four_state}");
    }
    let sim = shared_index(SHARED_INDEX, Some(celox::DiagnosticLevel::Warn)).unwrap();
    let warnings: Vec<String> = sim.warnings().iter().map(ToString::to_string).collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("`j`"), "{warnings:?}");
    assert!(warnings[0].contains("shared.sv:7"), "{warnings:?}");
    assert!(warnings[0].contains("shared.sv:11"), "{warnings:?}");
}

#[test]
fn shared_always_temporary_keeps_the_standard_when_it_would_show() {
    let allow = Some(celox::DiagnosticLevel::Allow);
    for (what, source) in [
        (
            "another reader",
            SHARED_INDEX.replace("  initial begin\n", "  wire [31:0] q = j;\n  initial begin\n"),
        ),
        (
            "a read before the write",
            SHARED_INDEX.replace(
                "    any_b = 1'b0;\n",
                "    any_b = j[0];\n",
            ),
        ),
        (
            "a hierarchical reader",
            SHARED_INDEX.replace("module top;", "module dut;").replace(
                "endmodule\n",
                "endmodule\nmodule top;\n  dut d();\n  initial #5 $display(\"%0d\", d.j);\nendmodule\n",
            ),
        ),
    ] {
        let sim = shared_index(&source, allow).unwrap();
        assert!(sim.warnings().is_empty(), "{what}");
        assert_does_not_settle(sim);
    }
}

#[test]
fn always_comb_blocks_may_not_share_a_variable() {
    let source = SHARED_INDEX.replace("always @(*)", "always_comb");
    let error = match shared_index(&source, Some(celox::DiagnosticLevel::Allow)) {
        Ok(_) => panic!("always_comb restricts a variable to one process"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("multiple variable drivers for `j`"),
        "{error}"
    );
    assert!(error.contains("shared.sv:7"), "{error}");
    assert!(error.contains("shared.sv:11"), "{error}");
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn plan_builds_report_kernels_and_warnings_without_code() {
    // `build_plan` stops after the compiled scheduler's plan: each kernel
    // is scheduled or falls back, or, without a compiled plan, runs in the
    // reference loop; a warning a build would return is reported too.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("short.hex");
    std::fs::write(&file, "aa").unwrap();
    let source = format!(
        r#"`timescale 1ns/1ns
        module Top;
            logic clk = 0;
            logic [7:0] mem [0:1];
            always #5 clk = ~clk;
            initial $readmemh("{}", mem);
        endmodule"#,
        file.display()
    );
    let report =
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("top.sv"))], "Top")
            .build_plan()
            .unwrap();
    assert!(report.contains("processes: 1"), "{report}");
    assert!(
        report.contains("`always` at top.sv:5:13 in `Top`: scheduled"),
        "{report}"
    );
    assert!(report.contains("warning: `$readmemh` reading"), "{report}");
    let report =
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("top.sv"))], "Top")
            .scheduler_mode(celox::SchedulerMode::Reference)
            .build_plan()
            .unwrap();
    assert!(
        report.contains("`always` at top.sv:5:13 in `Top`: reference loop"),
        "{report}"
    );
    assert!(
        report.contains("scheduler: none (reference loop)"),
        "{report}"
    );
}

sv_backends! {
    // Several ordinary `always` processes may write one variable or memory;
    // each write stores as it executes and the last one determines the
    // value (IEEE 1800-2023 6.5). Only always_comb, always_latch and
    // always_ff restrict a variable to one process (9.2.2).
    fn ordinary_always_processes_share_variables(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output logic [3:0] x, m0, m1, seen);
                logic a = 0, b = 0;
                logic [3:0] m [0:1];
                assign m0 = m[0];
                assign m1 = m[1];
                always @(a) if (a) begin
                    x = 4'h3;
                    m[0] = 4'h3;
                end
                always @(b) if (b) begin
                    x = x | 4'hc;
                    m[0][3:2] = 2'b11;
                    m[1] <= 4'h5;
                end
                initial begin
                    #1 a = 1;
                    #1 b = 1;
                    #1 seen = {x[3], m[0][3], x[0], m[0][0]};
                    a = 0;
                    b = 0;
                    #1 a = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("shared_always.sv"))], "Top");
        sim.run_until(10).unwrap();
        for (name, value) in [("seen", 0xfu8), ("x", 3), ("m0", 3), ("m1", 5)] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(value), "{name}");
        }
    }

    // Edge procedures that call timed tasks run as processes, and two of
    // them may write one variable; the later write determines its value.
    fn timed_edge_processes_share_a_variable(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output bit q, output int changes);
                bit clk = 0, a = 0;
                task automatic pause; #1; endtask
                always @(posedge clk) begin pause(); q = a; end
                always @(negedge clk) begin pause(); q = ~a; end
                initial begin
                    #1 clk = 1;
                    #3 clk = 0;
                    #3 a = 1; clk = 1;
                    #3 clk = 0;
                end
                initial begin
                    changes = 0;
                    #1 forever @(q) changes = changes + 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("timed_shared.sv"))], "Top");
        sim.run_until(20).unwrap();
        assert_eq!(sim.get(sim.signal("q")), BigUint::from(0u8));
        assert_eq!(sim.get(sim.signal("changes")), BigUint::from(2u8));
    }

    // The writers of a generate loop each write one bit of the element a
    // run-time address selects, as a behavioural memory with bit write
    // enables does.
    fn generated_always_processes_write_the_bits_of_one_memory(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Ram #(parameter bits = 4) (
                input w_clk, input [2:0] w_addr, input [bits-1:0] w_din, w_bwe,
                input [2:0] r_addr, output [bits-1:0] r_dout);
                reg [bits-1:0] array [0:7];
                wire [bits-1:0] bitclk = {bits{w_clk}} & w_bwe;
                genvar idx;
                generate
                    for (idx = 0; idx < bits; idx = idx + 1) begin : write
                        always @(bitclk[idx] or w_clk or w_addr or w_din[idx]) begin
                            if (bitclk[idx] && w_clk) array[w_addr][idx] <= w_din[idx];
                        end
                    end
                endgenerate
                assign r_dout = array[r_addr];
            endmodule
            module Top(output logic [3:0] whole, masked, cleared);
                reg clk = 0, clobber = 0;
                reg [2:0] wa = 0, ra = 0;
                reg [3:0] din = 0, bwe = 4'hf;
                wire [3:0] dout;
                integer i;
                Ram ram(.w_clk(clk), .w_addr(wa), .w_din(din), .w_bwe(bwe), .r_addr(ra), .r_dout(dout));
                // Another module's process writes the memory too.
                always @(clobber) begin
                    if (clobber) begin
                        for (i = 0; i < 8; i = i + 1) ram.array[i] <= 4'b0;
                    end
                end
                initial begin
                    #1 wa = 5; din = 4'hf; #1 clk = 1; #1 clk = 0;
                    #1 wa = 3; din = 4'ha; #1 clk = 1; #1 clk = 0;
                    #1 wa = 5; din = 4'h5; bwe = 4'h3; #1 clk = 1; #1 clk = 0;
                    #1 ra = 3; #1 whole = dout;
                    ra = 5; #1 masked = dout;
                    clobber = 1; #1 cleared = dout;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("bit_writers.sv"))], "Top");
        sim.run_until(20).unwrap();
        for (name, value) in [("whole", 0xau8), ("masked", 0xd), ("cleared", 0)] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(value), "{name}");
        }
    }

    // An edge procedure that assigns a memory by blocking assignments, while
    // a process of another module writes the memory too, stores each write
    // as it executes.
    fn blocking_edge_writes_share_a_memory_with_another_module(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Mem(input clk, we, input [3:0] wa, ra, input [7:0] wd, output logic [7:0] rd);
                logic [7:0] memory [16];
                always @(posedge clk) begin
                    if (we) memory[wa][3:0] = wd[3:0];
                    rd <= memory[ra];
                end
            endmodule
            module Loader(input clk, load);
                always @(posedge clk) begin
                    if (load) begin
                        Top.m.memory[3] = 8'hb2;
                        Top.m.memory[4] = 8'hc3;
                    end
                end
            endmodule
            module Top(output logic [7:0] merged, loaded);
                logic clk = 0, we = 0, load = 0;
                logic [3:0] wa = 0, ra = 0;
                logic [7:0] wd = 0, rd;
                Mem m(.*);
                Loader l(.*);
                always #1 clk = ~clk;
                initial begin
                    @(negedge clk) load = 1;
                    @(negedge clk) load = 0;
                    we = 1; wa = 3; wd = 8'hff;
                    @(negedge clk) we = 0; ra = 3;
                    @(negedge clk) merged = rd;
                    ra = 4;
                    @(negedge clk) loaded = rd;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("shared_memory.sv"))], "Top");
        sim.run_until(20).unwrap();
        for (name, value) in [("merged", 0xbfu8), ("loaded", 0xc3)] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(value), "{name}");
        }
    }
}

// A concatenation is the target of an assignment operator as of `=`:
// `{a, b} op= v` is `{a, b} = {a, b} op v` (IEEE 1800-2023 11.4.1, 11.4.12),
// the concatenation unsigned, as OpenTitan's debug module masks its hart
// selection with `{hartselhi, hartsello} &= (2**$clog2(NrHarts))-1`.
#[test]
fn assignment_operators_assign_concatenations() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
          typedef struct packed { logic [3:0] hi; logic [5:0] lo; logic x; } ctl_t;
          ctl_t d;
          logic [3:0] a; logic [3:0] b; logic [7:0] arr [2]; logic [1:0] i;
          int unsigned n;
          initial begin
            d = '{hi: 4'hf, lo: 6'h3f, x: 1'b1};
            {d.hi, d.lo} &= (2**$clog2(5))-1;
            a = 4'h1; b = 4'hf;
            {a, b} += 8'h01;
            arr[0] = 8'h80; arr[1] = 8'h01; i = 1;
            {arr[i], arr[0]} >>= 4;
            n = 3;
            {a, b} <<= n;
            $display("d=%h a=%h b=%h arr=%h %h", d, a, b, arr[1], arr[0]);
          end
          logic clk = 0; logic [3:0] p, q;
          always_ff @(posedge clk) {p, q} |= 8'h81;
          initial begin
            p = 0; q = 0; #2 clk = 1; #1 clk = 0; #1 clk = 1;
            #1 $display("p=%h q=%h", p, q);
          end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("concat_ops.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(20).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            ["d=00f a=0 b=0 arr=00 18", "p=8 q=1"],
            "four-state {four_state}"
        );
    }
}

/// An assignment operator updates each part of a concatenation as a target
/// of its own (IEEE 1800-2023 11.4.1): an enumerated part is updated as a
/// direct one is (6.19.3), and a part whose select has an effectful call, a subroutine or a random
/// function, which the read and the write of the part would each call, is
/// not taken.
#[test]
fn assignment_operators_check_each_part_of_a_concatenation() {
    for (body, expected) in [
        (
            "typedef enum logic [1:0] { A, B, C } e_t; e_t state; logic [1:0] bits;
             initial begin state = A; bits = 0; {state, bits} += 1; end",
            "not assignment compatible",
        ),
        (
            "logic [7:0] arr [4]; logic [3:0] b; int k = 0;
             function automatic int next(); k = k + 1; return k; endfunction
             initial begin {arr[next()], b} += 1; end",
            "select has an effectful call",
        ),
        (
            "logic [7:0] arr [4]; logic [3:0] b;
             initial begin {arr[$urandom % 4], b} += 1; end",
            "select has an effectful call",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("concat_parts.sv"))],
            "Top",
        )
        .build()
        .map(|_| ())
        .expect_err("the assignment is rejected")
        .to_string()
        .replace(['\n', '│'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
        assert!(error.contains(expected), "{body}: {error}");
    }
}
