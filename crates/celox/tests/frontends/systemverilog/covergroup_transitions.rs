//! Transition bins of coverpoints (IEEE 1800-2023 19.5.2), with the ignored
//! and illegal transitions of 19.5.5 and 19.5.6, read through the coverage
//! report of the simulation.
use super::*;

/// The bins of `bins` of kind `kind`.
fn of_kind(
    kind: celox::BinReportKind,
    bins: &[(&str, u64)],
) -> Vec<(String, celox::BinReportKind, u64)> {
    bins.iter()
        .map(|(name, hits)| (name.to_string(), kind, *hits))
        .collect()
}

sv_backends! {
    /// The example of 19.5.2 with its trace `4 5 7 11 8 12 2 2 3`: `sa`
    /// counts `7=>11` and `8=>12`, `sb[]` has a bin for each of the nine
    /// sequences, `sc` counts `12=>2=>2=>3`, and `allother` "increments
    /// twice", on 7 and on 8. `default sequence` is no bin of the coverage.
    fn transition_bins_of_cg_in_19_5_2(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst, ty);
    bit [4:1] v_a;
    covergroup cg;
        coverpoint v_a
        {
            bins sa   = (4 => 5 => 6), ([7:9],10 => 11,12);
            bins sb[] = (4 => 5 => 6), ([7:9],10 => 11,12);
            bins sc   = (12 => 3 [-> 1]);
            bins allother = default sequence ;
        }
    endgroup
    cg c = new;
    bit [4:1] trace [9] = '{4, 5, 7, 11, 8, 12, 2, 2, 3};
    initial begin
        foreach (trace[i]) begin
            v_a = trace[i];
            c.sample();
        end
        inst = $realtobits(c.get_inst_coverage());
        ty = $realtobits(cg::get_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let mut expected = counting(&[
            ("sa", 2),
            ("sb[4=>5=>6]", 0),
            ("sb[7=>11]", 1),
            ("sb[8=>11]", 0),
            ("sb[9=>11]", 0),
            ("sb[10=>11]", 0),
            ("sb[7=>12]", 0),
            ("sb[8=>12]", 1),
            ("sb[9=>12]", 0),
            ("sb[10=>12]", 0),
            ("sc", 1),
        ]);
        expected.extend(of_kind(celox::BinReportKind::Default, &[("allother", 2)]));
        assert_eq!(rows(&report, "Top.cg", 0, "v_a"), (expected, 0));
        // `sa`, `sb[7=>11]`, `sb[8=>12]` and `sc` of 11 bins (19.11).
        assert_eq!(counted(&report, "Top.cg", 0, "v_a"), (4, 11));
        assert!(close(real(&mut sim, "inst"), 400.0 / 11.0));
        assert!(close(real(&mut sim, "ty"), 400.0 / 11.0));
    }

    /// The example `sg` of 19.5.2, sample by sample: `b2` counts on the 8th
    /// and 10th samples, `b3` on the 6th, 9th and 11th, `b5` on the 15th to
    /// 18th, `b6` on the 12th and `b7` on the 13th. Each counts at most once
    /// for a sample.
    fn overlapping_transitions_of_sg_in_19_5_2(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit [3:0] v;
    covergroup sg;
        coverpoint v
        {
            bins b2 = (2 [-> 3:5]);
            bins b3 = (3 [-> 3:5]);
            bins b5 = (5 [* 3]);
            bins b6 = (1 => 3 [-> 4:6] => 1);
            bins b7 = (1 => 2 [= 3:6] => 5);
        }
    endgroup
    sg c = new;
    bit [3:0] trace [18] = '{1, 4, 3, 2, 3, 3, 2, 2, 3, 2, 3, 1, 5, 5, 5, 5, 5, 5};
    initial foreach (trace[i]) begin
        v = trace[i];
        c.sample();
        #1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        let mut before = vec![0; 5];
        let mut counted_at: Vec<Vec<u64>> = vec![Vec::new(); 5];
        // The sample `k` from 1 is taken at time `k - 1`.
        for sample in 1..=18u64 {
            sim.run_until(sample - 1).unwrap();
            let now: Vec<u64> = bins(&sim.coverage(), "Top.sg", 0, "v")
                .into_iter()
                .map(|(_, hits)| hits)
                .collect();
            for (bin, (now, before)) in now.iter().zip(&before).enumerate() {
                assert!(now - before <= 1, "sample {sample}");
                if now > before {
                    counted_at[bin].push(sample);
                }
            }
            before = now;
        }
        assert_eq!(
            counted_at,
            [
                vec![8, 10],
                vec![6, 9, 11],
                vec![15, 16, 17, 18],
                vec![12],
                vec![13]
            ]
        );
    }

    /// The forms of 19.5.2 on one trace: a set of transitions (`1,5 =>
    /// 6,7`), consecutive repetitions with a count and a range, goto
    /// repetitions alone and between steps, and nonconsecutive ones. A
    /// trailing goto or nonconsecutive repetition counts at the sample that
    /// gives it its count; the gap after a nonconsecutive repetition ends
    /// at a sample of its value or with the first match after it.
    fn repetitions_of_19_5_2(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    covergroup cg;
        coverpoint v
        {
            bins sets = (1, 5 => 6, 7);
            bins five = (3 [* 5]);
            bins some = (3 [* 3:5]);
            bins goto3 = (3 [-> 3]);
            bins between = (1 => 3 [-> 3] => 5);
            bins non2 = (3 [= 2]);
            bins later = (1 => 3 [= 2] => 6);
        }
    endgroup
    cg c = new;
    bit [3:0] trace [24] = '{1, 6, 5, 7, 3, 3, 3, 3, 3, 3, 1, 2, 3, 4, 3, 0, 3, 5, 1, 3, 3, 2, 6, 6};
    initial begin
        foreach (trace[i]) begin
            v = trace[i];
            c.sample();
        end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [24]);
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[
                // 1=>6 and 5=>7.
                ("sets", 2),
                // The run of six 3s from the 5th sample.
                ("five", 2),
                ("some", 4),
                // Every 3 from the third.
                ("goto3", 9),
                // 1 at the 11th sample, 3 at the 13th, 15th and 17th, and
                // 5 at once after it.
                ("between", 1),
                // Every 3 from the second.
                ("non2", 10),
                // Only after the 1 of the 19th sample does no third 3 come
                // before a 6; the second 6 is after the first match.
                ("later", 1),
            ])
        );
    }

    /// 19.5.2: `name[]` is a bin for each sequence of values, over each
    /// length of a consecutive repetition range (`3 [* 3:5]` is "the same
    /// as" its three sequences), named `name[transition]`; a sequence that
    /// a declaration lists twice is one bin. A wildcard transition takes x,
    /// z and `?` as 0 or 1 (19.5.4).
    fn transition_arrays_and_wildcard_transitions(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    covergroup cg;
        coverpoint v
        {
            bins r[] = (3 [* 3:5]);
            bins p[] = (1, 2 => 3), (2 => 3);
            wildcard bins w = (4'b11?? => 4'b00??);
        }
    endgroup
    cg c = new;
    bit [3:0] trace [15] = '{3, 3, 3, 3, 3, 3, 1, 3, 2, 3, 12, 1, 15, 3, 14};
    initial begin
        foreach (trace[i]) begin
            v = trace[i];
            c.sample();
        end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[
                ("r[3=>3=>3]", 4),
                ("r[3=>3=>3=>3]", 3),
                ("r[3=>3=>3=>3=>3]", 2),
                ("p[1=>3]", 1),
                ("p[2=>3]", 1),
                // 12=>1 and 15=>3.
                ("w", 2),
            ])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "v"), (6, 6));
    }

    /// The examples `cg23` of 19.5.5 and `cg3` of 19.5.6: an ignored or
    /// illegal sequence removes a match of another bin that it is part of
    /// ("the ignored sequence 2=>3 would remove the covered sequence
    /// 1=>2=>3=>4"), a bin left without a sequence is excluded from
    /// coverage, ignored and illegal values have "no effect on a
    /// transition that includes the value", and an illegal transition is a
    /// run-time error as an illegal value is.
    fn ignored_and_illegal_transitions(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] a, b, d;
    covergroup cg23;
        coverpoint a
        {
            ignore_bins ignore_vals = {7,8};
            ignore_bins ignore_trans = (1=>3=>5);
            bins long = (1 => 2 => 3 => 4);
            ignore_bins short = (2 => 3);
            bins t = (7 => 8);
            bins u = (0 => 1 => [3:4] => 5);
            bins v = (0 => 1);
        }
    endgroup
    covergroup cg3;
        coverpoint b
        {
            illegal_bins bad_vals = {1,2,3};
            illegal_bins bad_trans = (4=>5=>6);
            bins w = (1 => 2);
            bins x = (4 => 5);
        }
    endgroup
    covergroup cgd;
        coverpoint d { bins t = (1 => 2); illegal_bins other = default sequence; }
    endgroup
    cg23 c23 = new;
    cg3 c3 = new;
    cgd cd = new;
    bit [3:0] av [14] = '{1, 2, 3, 4, 7, 8, 0, 1, 3, 5, 0, 1, 4, 5};
    bit [3:0] bv [9] = '{4, 5, 6, 1, 2, 0, 4, 5, 0};
    bit [3:0] dv [4] = '{1, 2, 3, 1};
    initial begin
        foreach (av[i]) begin a = av[i]; c23.sample(); end
        #1 foreach (bv[i]) begin b = bv[i]; c3.sample(); end
        #1 foreach (dv[i]) begin d = dv[i]; cd.sample(); end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(3).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        use celox::BinReportKind::{Bins, Ignore, Illegal};
        let row = |name: &str, kind, hits| (name.to_string(), kind, hits);
        // `long` has no sequence left and is not listed. `u` counts
        // 0=>1=>4=>5 and not 0=>1=>3=>5.
        assert_eq!(
            rows(&report, "Top.cg23", 0, "a"),
            (
                vec![
                    row("ignore_vals", Ignore, 2),
                    row("ignore_trans", Ignore, 1),
                    row("short", Ignore, 1),
                    row("t", Bins, 1),
                    row("u", Bins, 1),
                    row("v", Bins, 2),
                ],
                0
            )
        );
        assert_eq!(counted(&report, "Top.cg23", 0, "a"), (3, 3));
        assert_eq!(
            rows(&report, "Top.cg3", 0, "b"),
            (
                vec![
                    row("bad_vals", Illegal, 2),
                    row("bad_trans", Illegal, 1),
                    row("w", Bins, 1),
                    row("x", Bins, 2),
                ],
                0
            )
        );
        let illegal = |group: &str, point: &str, bin: &str, what: &str| {
            (
                celox::Severity::Error,
                format!(
                    "illegal_bins `{bin}` of coverpoint `{point}` of covergroup `Top.{group}` \
                     (instance `Top.{group}#0`): {what}"
                ),
            )
        };
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [
                illegal("cg3", "b", "bad_trans", "transition 4=>5=>6"),
                illegal("cg3", "b", "bad_vals", "value 1"),
                illegal("cg3", "b", "bad_vals", "value 2"),
                illegal("cgd", "d", "other", "transition 2=>3"),
                illegal("cgd", "d", "other", "transition 3=>1"),
            ]
        );
    }

    /// 19.5.2 beside 19.5: a bin whose sequence matches with a false guard
    /// does not count, and `default sequence` does not count that sample
    /// either, as a value `default` bin does not count the value of a bin
    /// whose guard is false.
    fn default_sequence_beside_a_guarded_match(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    bit en;
    covergroup cg;
        coverpoint v { bins g = (5 => 6) iff (en); bins o = default sequence; }
    endgroup
    cg c = new;
    bit [3:0] vs [5] = '{5, 6, 7, 5, 6};
    bit ens [5] = '{1, 0, 1, 1, 1};
    initial begin
        foreach (vs[i]) begin v = vs[i]; en = ens[i]; c.sample(); end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        let mut expected = counting(&[("g", 1)]);
        expected.extend(of_kind(celox::BinReportKind::Default, &[("o", 2)]));
        assert_eq!(rows(&report, "Top.cg", 0, "v"), (expected, 0));
    }

    /// 19.5: the values of the steps and the counts of the repetitions are
    /// covergroup expressions: constants, parameters, enumerators, the
    /// arguments of the covergroup and calls of functions of these, which
    /// `new` evaluates, so that instances differ.
    fn transition_operands_of_each_instance(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum bit [3:0] { IDLE, RD = 5, WR = 9 } st_t;
    st_t s;
    bit [3:0] v;
    localparam bit [3:0] P = 4;
    function automatic int twice(int x); return 2 * x; endfunction
    covergroup cg(int a, int n);
        coverpoint s { bins t = (IDLE => RD, WR); }
        cv: coverpoint v { bins r = (a => P => twice(a) [* n]); bins q[] = (a => [P:P+1] [* 1:n]); }
    endgroup
    cg c1 = new(1, 2), c2 = new(3, 1);
    bit [3:0] vs [10] = '{1, 4, 2, 2, 3, 4, 6, 1, 5, 4};
    st_t ss [10] = '{IDLE, RD, IDLE, WR, WR, RD, IDLE, IDLE, RD, WR};
    initial begin
        foreach (vs[i]) begin
            v = vs[i];
            s = ss[i];
            c1.sample();
            c2.sample();
        end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        for instance in [0, 1] {
            assert_eq!(bins(&report, "Top.cg", instance, "s"), hits(&[("t", 3)]));
        }
        // `r` is 1=>4=>2=>2 for the first and 3=>4=>6 for the second; the
        // bins of `q` are those of the lengths 1 and 2, then 1 alone.
        assert_eq!(
            bins(&report, "Top.cg", 0, "cv"),
            hits(&[
                ("r", 1),
                ("q[1=>4]", 1),
                ("q[1=>5]", 1),
                ("q[1=>4=>4]", 0),
                ("q[1=>5=>4]", 1),
                ("q[1=>4=>5]", 0),
                ("q[1=>5=>5]", 0),
            ])
        );
        assert_eq!(
            bins(&report, "Top.cg", 1, "cv"),
            hits(&[("r", 1), ("q[3=>4]", 1), ("q[3=>5]", 0)])
        );
    }

    /// 19.6, 19.6.1.1: transition bins are bins of a cross, and `binsof`
    /// selects them by name or, with `intersect`, by "the last value of the
    /// transition".
    fn crosses_of_transition_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    bit e;
    covergroup cg;
        cv: coverpoint v { bins t = (1 => 2), (3 => 5); bins u[] = (4 => 5, 6); bins low = {[7:8]}; }
        x: cross cv, e { bins b = binsof(cv.t) intersect {5}; bins c = binsof(cv) intersect {6}; }
    endgroup
    cg c = new;
    bit [3:0] vs [9] = '{1, 2, 3, 5, 4, 6, 7, 4, 5};
    bit es [9] = '{0, 1, 0, 0, 1, 1, 0, 0, 1};
    initial begin
        foreach (vs[i]) begin v = vs[i]; e = es[i]; c.sample(); end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        assert_eq!(
            bins(&report, "Top.cg", 0, "cv"),
            hits(&[("t", 2), ("u[4=>5]", 1), ("u[4=>6]", 1), ("low", 1)])
        );
        // `b` is `t` with either value of `e`, `c` is `u[4=>6]` with
        // either; the four other products are automatic bins.
        assert_eq!(
            rows(&report, "Top.cg", 0, "x"),
            (
                counting(&[
                    ("b", 2),
                    ("c", 1),
                    ("<u[4=>5],auto[1]>", 1),
                    ("<low,auto[0]>", 1),
                ]),
                2
            )
        );
        assert_eq!(counted(&report, "Top.cg", 0, "x"), (4, 6));
    }

    /// Options and methods of a coverpoint with transitions: `at_least`
    /// (19.7), `get_inst_coverage` of the coverpoint (19.8), and a
    /// coverpoint that is not sampled, while the covergroup is stopped or
    /// its guard is false, keeps its transitions where they were.
    fn transitions_freeze_when_stopped(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned point, inst);
    bit [3:0] v;
    bit en = 1;
    covergroup cg;
        option.at_least = 2;
        cv: coverpoint v iff (en) { bins t = (1 => 2 => 3); bins o = default sequence; }
    endgroup
    cg c = new;
    initial begin
        v = 1; c.sample();
        v = 2; c.sample();
        c.stop();
        v = 9; c.sample();
        c.start();
        v = 3; c.sample();
        v = 1; c.sample();
        v = 2; c.sample();
        en = 0; v = 7; c.sample();
        en = 1; v = 3; c.sample();
        v = 5; c.sample();
        point = $realtobits(c.cv.get_inst_coverage());
        inst = $realtobits(c.get_inst_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [8]);
        let mut expected = counting(&[("t", 2)]);
        expected.extend(of_kind(celox::BinReportKind::Default, &[("o", 2)]));
        assert_eq!(rows(&report, "Top.cg", 0, "cv"), (expected, 0));
        assert_eq!(real(&mut sim, "point"), 100.0);
        assert_eq!(real(&mut sim, "inst"), 100.0);
    }

    /// 19.5.4 applied to transitions in four-state simulation: an x or z
    /// sample is in a step of exactly that value and breaks the
    /// transitions it is not in.
    fn unknown_samples_in_transitions(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [1:0] l;
    covergroup cg;
        coverpoint l { bins t = (2'b1x => 0); bins u = (1 => 2); }
    endgroup
    cg c = new;
    logic [1:0] ls [8] = '{2'b1x, 0, 1, 2'bx1, 2, 2'b1z, 1, 2};
    initial begin
        foreach (ls[i]) begin l = ls[i]; c.sample(); end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        sim.drain_runtime_events();
        let report = sim.coverage();
        assert_eq!(bins(&report, "Top.cg", 0, "l"), hits(&[("t", 1), ("u", 1)]));
    }

    /// The shape of a real design: the transitions of a bus state between
    /// parameters, a write, a run of idle states and a read, as an array of
    /// bins, beside an ignore bin of values, which does not change the
    /// transitions (19.5.5).
    fn bus_transitions_of_parameters(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    localparam bit [3:0] IDLE = 0, AHB_WR = 1, AXI_WR = 2, AHB_RD = 3, AXI_RD = 4;
    bit [3:0] st;
    covergroup cg;
        coverpoint st {
            bins wr_rd[] = (AHB_WR, AXI_WR => IDLE [*1:30] => AHB_RD, AXI_RD);
            ignore_bins dont_care = {IDLE, 4'hf, (AXI_RD | AXI_WR), (AHB_RD | AHB_WR)};
        }
    endgroup
    cg c = new;
    task automatic burst(bit [3:0] write, int idles, bit [3:0] read);
        st = write; c.sample();
        repeat (idles) begin st = IDLE; c.sample(); end
        st = read; c.sample();
    endtask
    initial begin
        burst(AHB_WR, 1, AHB_RD);
        burst(AXI_WR, 30, AXI_RD);
        burst(AXI_WR, 31, AXI_RD);
        burst(AHB_WR, 2, AXI_RD);
        burst(AHB_WR, 2, AXI_RD);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        let name = |write: u8, idles: usize, read: u8| {
            format!("wr_rd[{write}=>{}{read}]", "0=>".repeat(idles))
        };
        let all = bins(&report, "Top.cg", 0, "st");
        // 2 writes, 30 lengths and 2 reads, and `dont_care`.
        assert_eq!(all.len(), 121);
        assert_eq!(
            hit(all),
            [
                (name(1, 1, 3), 1),
                (name(1, 2, 4), 2),
                (name(2, 30, 4), 1),
                ("dont_care".to_string(), 67),
            ]
        );
        // The ignored values are still values of the transitions.
        assert_eq!(counted(&report, "Top.cg", 0, "st"), (3, 120));
    }

    /// The same coverpoint through a `ref` argument of a covergroup sampled
    /// at its clocking event (19.3): the coverpoint expression reads the
    /// variable of the actual, the bins read constants.
    fn bus_transitions_through_a_reference(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    localparam bit [3:0] IDLE = 0, AHB_WR = 1, AXI_WR = 2, AHB_RD = 3, AXI_RD = 4;
    bit clk = 0;
    always #5 clk = ~clk;
    logic [3:0] state = IDLE;
    covergroup cg(ref logic [3:0] st) @(posedge clk);
        coverpoint st {
            bins wr_rd[] = (AHB_WR, AXI_WR => IDLE [*1:30] => AHB_RD, AXI_RD);
            ignore_bins dont_care = {IDLE, 4'hf, (AXI_RD | AXI_WR), (AHB_RD | AHB_WR)};
        }
    endgroup
    cg c = new(state);
    logic [3:0] trace [10] = '{AXI_WR, IDLE, IDLE, AHB_RD, AHB_WR, IDLE, AXI_RD, AHB_WR, IDLE, AXI_RD};
    initial foreach (trace[i]) begin
        @(negedge clk) state = trace[i];
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        // The rising edges at 15, ..., 105 sample the ten states.
        sim.run_until(110).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [11]);
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "st")),
            [
                ("wr_rd[1=>0=>4]".to_string(), 2),
                ("wr_rd[2=>0=>0=>3]".to_string(), 1),
                ("dont_care".to_string(), 6),
            ]
        );
    }
}

/// A repetition count that an argument of the covergroup gives is checked
/// when `new` builds the bins: a count that is not positive ends the
/// simulation with an error, as the other faults of a construction do,
/// such as a bin array of no bins.
#[test]
fn a_repetition_count_of_an_argument_fails_the_construction() {
    let run = |bins: &str| {
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    covergroup cg(int n); coverpoint v {{ {bins} }} endgroup
    cg c;
    initial begin
        c = new(0);
        done = 1;
    end
endmodule
"
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        sim.run_until(1)
            .expect_err("the construction faults")
            .to_string()
    };
    let count = run("bins t = (1 => 2 [* n]);");
    let size = run("bins b[n] = {[0:3]};");
    let (head, _) = count.split_once("type `Top.cg`: ").expect(&count);
    assert_eq!(size.split_once("type `Top.cg`: ").expect(&size).0, head);
    assert!(
        count.contains("type `Top.cg`: bins `t` of coverpoint `v` has the repetition count 0"),
        "{count}"
    );
    let range = run("bins t = (1 => 2 [* 2:n]);");
    assert!(
        range.contains("type `Top.cg`: bins `t` of coverpoint `v` has the repetition count 0"),
        "{range}"
    );
}

/// The transitions of a coverpoint have at most
/// `celox_heap::coverage::MAX_TRANSITION_STATES` states: the bus shape of
/// `bus_transitions_of_parameters` with up to 1000 idle states is a bin
/// array of 4000 sequences of more states, which the construction refuses.
#[test]
fn transitions_of_more_states_than_the_limit_fail_the_construction() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    localparam bit [3:0] IDLE = 0, AHB_WR = 1, AXI_WR = 2, AHB_RD = 3, AXI_RD = 4;
    bit [3:0] st;
    covergroup cg;
        coverpoint st {
            bins wr_rd[] = (AHB_WR, AXI_WR => IDLE [*1:1000] => AHB_RD, AXI_RD);
            ignore_bins dont_care = {IDLE, 4'hf, (AXI_RD | AXI_WR), (AHB_RD | AHB_WR)};
        }
    endgroup
    cg c;
    initial begin
        c = new;
        done = 1;
    end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build().unwrap();
    let error = sim
        .run_until(1)
        .expect_err("the construction faults")
        .to_string();
    assert!(
        error.contains("type `Top.cg`: coverpoint `st` has transitions of more than 65536 states"),
        "{error}"
    );
}

/// Transitions under the untimed `Simulator`, in the clocked procedure that
/// constructs and samples the covergroup: a guard reads the state of the
/// design, an illegal transition is reported, and `default sequence` counts
/// the samples that end and continue no sequence. At every optimization
/// level, in both state modes and with each dead-store policy.
#[test]
fn transitions_in_clocked_logic_under_simulator() {
    let source = r#"module Top(input bit clk, input logic [3:0] d, output bit [7:0] q);
    covergroup cg;
        coverpoint d {
            bins up = (1 => 2 => 3);
            bins rep[] = (4 [* 2:3]);
            bins go = (5 => 6 [-> 2] => 7) iff (q[1]);
            illegal_bins bad = (8 => 9);
            bins other = default sequence;
        }
    endgroup
    cg c;
    always_ff @(posedge clk) q <= q + 8'd1;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end
endmodule
"#;
    for level in [
        celox::OptLevel::O0,
        celox::OptLevel::O1,
        celox::OptLevel::O2,
    ] {
        for four_state in [false, true] {
            for policy in [
                celox::DeadStorePolicy::Off,
                celox::DeadStorePolicy::PreserveTopPorts,
                celox::DeadStorePolicy::PreserveAllPorts,
            ] {
                let context = format!("{level:?} four_state={four_state} {policy:?}");
                let mut sim = Simulator::from_sv_sources(
                    vec![(source, Path::new("covergroup_ff.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .opt_level(level)
                .dead_store_policy(policy)
                .build()
                .unwrap_or_else(|error| panic!("{context}: {error}"));
                let clk = sim.event("clk");
                let d = sim.signal("d");
                // `q` is the number of edges before the one that samples.
                for value in [1u8, 2, 3, 4, 4, 4, 5, 0, 6, 6, 7, 8, 9, 2] {
                    sim.modify(|io| io.set(d, value)).unwrap();
                    sim.tick(clk).unwrap();
                }
                assert_eq!(
                    reports(sim.drain_runtime_events()),
                    [(
                        celox::Severity::Error,
                        "illegal_bins `bad` of coverpoint `d` of covergroup `Top.cg` (instance \
                         `Top.cg#0`): transition 8=>9"
                            .to_string()
                    )],
                    "{context}"
                );
                let report = sim.coverage();
                assert_eq!(samples(&report, "Top.cg"), [14], "{context}");
                // `go` ends at the 11th sample, with `q` 10.
                let mut expected =
                    counting(&[("up", 1), ("rep[4=>4]", 2), ("rep[4=>4=>4]", 1), ("go", 1)]);
                expected.extend(of_kind(celox::BinReportKind::Illegal, &[("bad", 1)]));
                expected.extend(of_kind(celox::BinReportKind::Default, &[("other", 4)]));
                assert_eq!(rows(&report, "Top.cg", 0, "d"), (expected, 0), "{context}");
            }
        }
    }
}

/// 19.5.7: a value that the coverpoint type does not have "shall not
/// participate" in its bin, also in a step of a transition. A transition with
/// a step that no value is left in is matched by no sample and is none of the
/// bin: a bin left without one is empty and is not listed, an ignored one
/// takes nothing from other bins, an array has no bin for it, and the other
/// bins and the instance count as before. A constant is warned of by the
/// build, a value that an argument gives by `new`, alike. A value with x or z
/// bits takes part on a coverpoint of a four-state type only, in either state
/// mode.
#[test]
fn transition_steps_without_a_value_are_no_transitions() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    logic [3:0] l;
    covergroup cg;
        cv: coverpoint v {
            bins w = (20 => 1);
            bins w2 = (20, 2 => 1);
            bins wa[] = (20 => 1), (2 => 0);
            bins neg = (-1 => 0);
            bins xz = (4'b1x00 => 0);
            bins mid = (2 => 20 => 1);
            ignore_bins ig = (20 => 1);
            bins rr = ([16:20] => 1);
            bins rep = (2 => 20 [* 2]);
            bins str = ("ab" => 2);
            bins keep = (2 => 1);
        }
        cl: coverpoint l { bins xz = (4'b1x00 => 0); bins keep = (2 => 1); }
    endgroup
    covergroup ag(int a); coverpoint v { bins w = (a => 1); bins keep = (2 => 1); } endgroup
    cg c = new;
    ag g = new(20);
    bit [3:0] tv [5] = '{2, 1, 15, 2, 0};
    initial begin
        foreach (tv[i]) begin v = tv[i]; l = tv[i]; c.sample(); g.sample(); end
        done = 1;
    end
endmodule
"#;
    for four_state in [false, true] {
        let context = format!("four_state={four_state}");
        let mut sim = simulation(source)
            .four_state(four_state)
            .build_native()
            .unwrap_or_else(|error| panic!("{context}: {error}"));
        let warned: Vec<(String, String)> = sim
            .warnings()
            .iter()
            .map(|warning| match warning {
                celox::CompilationWarning::SystemVerilog(celox::SvFrontendWarning {
                    kind: celox::SvFrontendWarningKind::CovergroupBinValue { bin, detail, .. },
                    ..
                }) => (bin.clone(), detail.clone()),
                warning => panic!("{context}: unexpected warning: {warning}"),
            })
            .collect();
        let outside = |value: &str| {
            format!(
                "value {value} is not a value of the coverpoint type and does not take part \
                 (IEEE 1800-2023 19.5.7)"
            )
        };
        let warned_bins: Vec<&str> = warned.iter().map(|(bin, _)| bin.as_str()).collect();
        assert_eq!(
            warned_bins,
            [
                "w", "w2", "wa", "neg", "xz", "mid", "ig", "rr", "rep", "str"
            ],
            "{context}: {warned:?}"
        );
        assert_eq!(warned[0].1, outside("20"), "{context}");
        assert_eq!(warned[3].1, outside("-1"), "{context}");
        assert_eq!(
            warned[4].1,
            "value 4'b1x00 has x or z bits and does not take part (IEEE 1800-2023 19.5.7)",
            "{context}"
        );
        assert_eq!(warned[9].1, outside("24930"), "{context}");
        sim.run_until(1)
            .unwrap_or_else(|error| panic!("{context}: {error}"));
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8), "{context}");
        // The argument gives `new` the same warning as the constant gives the
        // build.
        let reported = reports(sim.drain_runtime_events());
        assert_eq!(reported.len(), 1, "{context}: {reported:?}");
        assert_eq!(reported[0].0, celox::Severity::Warning, "{context}");
        assert!(
            reported[0]
                .1
                .contains("bins `w` of coverpoint `v` of covergroup `Top.ag`")
                && reported[0].1.contains(&outside("20")),
            "{context}: {reported:?}"
        );
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [5], "{context}");
        // `w2` is 2=>1, `wa` the bin of 2=>0; `keep` is not taken by `ig`.
        assert_eq!(
            bins(&report, "Top.cg", 0, "cv"),
            hits(&[("w2", 1), ("wa[2=>0]", 1), ("keep", 1)]),
            "{context}"
        );
        assert_eq!(counted(&report, "Top.cg", 0, "cv"), (3, 3), "{context}");
        // 4'b1x00 is a value of a four-state type, never sampled in two-state
        // simulation.
        assert_eq!(
            bins(&report, "Top.cg", 0, "cl"),
            hits(&[("xz", 0), ("keep", 1)]),
            "{context}"
        );
        assert_eq!(
            bins(&report, "Top.ag", 0, "v"),
            hits(&[("keep", 1)]),
            "{context}"
        );
    }
}

/// `option.detect_overlap` (Table 19-1) warns of value bins whose range lists
/// overlap and does not look at transition bins: pinned as it behaves, which
/// the reference page lists as unsupported.
#[test]
fn detect_overlap_does_not_look_at_transition_bins() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v, w;
    covergroup cg;
        option.detect_overlap = 1;
        cv: coverpoint v { bins a = (1 => 2); bins b = (1 => 2), (3 => 4); bins c = (1 => [2:3]); }
        cw: coverpoint w { bins x = {1, 2}; bins y = {2, 3}; }
    endgroup
    cg c = new;
    initial begin v = 1; w = 2; c.sample(); done = 1; end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build().unwrap();
    sim.run_until(1).unwrap();
    let reported = reports(sim.drain_runtime_events());
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0].0, celox::Severity::Warning);
    assert!(
        reported[0].1.contains("coverpoint `cw`") && !reported[0].1.contains("coverpoint `cv`"),
        "{reported:?}"
    );
}

sv_backends! {
    /// 19.5.1.2: a set expression may be written in parentheses, also with
    /// a number of bins; `(S)` of an array parameter is `S`.
    fn sized_set_expressions_in_parentheses(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    localparam bit [3:0] S [4] = '{1, 2, 3, 4};
    covergroup cg;
        coverpoint v { bins b[2] = (S); bins c[2] = S; }
    endgroup
    cg c = new;
    initial begin v = 1; c.sample(); v = 4; c.sample(); v = 3; c.sample(); done = 1; end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        assert_eq!(
            bins(&sim.coverage(), "Top.cg", 0, "v"),
            hits(&[("b[0]", 1), ("b[1]", 2), ("c[0]", 1), ("c[1]", 2)])
        );
    }
}

/// A repetition count that an argument gives and that no step can have ends
/// the simulation at `new` with an error that says so; a constant count of
/// that size is rejected by the analysis.
#[test]
fn a_repetition_count_too_large_fails_the_construction() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    covergroup cg(longint n); coverpoint v { bins t = (1 => 2 [* n]); } endgroup
    cg c;
    initial begin c = new(64'h1_0000_0002); done = 1; end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build().unwrap();
    let error = sim
        .run_until(1)
        .expect_err("the construction faults")
        .to_string();
    assert!(
        error.contains(
            "type `Top.cg`: bins `t` of coverpoint `v` has the repetition count 4294967298, more \
             than the 65536 states of the transitions of a coverpoint allow"
        ),
        "{error}"
    );
}

/// Transitions under the untimed `Simulator` beside those of
/// `transitions_in_clocked_logic_under_simulator`: a nonconsecutive
/// repetition, a count of an argument, and a cross with `binsof` of a
/// transition bin, in both state modes.
#[test]
fn nonconsecutive_and_crossed_transitions_under_simulator() {
    let source = r#"module Top(input bit clk, input logic [3:0] d, input logic e, output bit [7:0] q);
    covergroup cg(int n);
        cd: coverpoint d {
            bins nc = (1 => 2 [= 2] => 3);
            bins rep = (4 [* n]);
            bins other = {[5:15]};
        }
        x: cross cd, e { bins t = binsof(cd.nc) && binsof(e) intersect {1}; }
    endgroup
    cg c;
    always_ff @(posedge clk) q <= q + 8'd1;
    always @(posedge clk) begin
        if (c == null) c = new(2);
        c.sample();
    end
endmodule
"#;
    for four_state in [false, true] {
        let context = format!("four_state={four_state}");
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{context}: {error}"));
        let clk = sim.event("clk");
        let (d, e) = (sim.signal("d"), sim.signal("e"));
        // 1 2 0 2 0 3 ends `nc`, with `e` 1; 4 4 is `rep` of 2.
        for (value, on) in [
            (1u8, 0u8),
            (2, 0),
            (0, 0),
            (2, 0),
            (0, 0),
            (3, 1),
            (4, 0),
            (4, 0),
        ] {
            sim.modify(|io| {
                io.set(d, value);
                io.set(e, on);
            })
            .unwrap();
            sim.tick(clk).unwrap();
        }
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [8], "{context}");
        assert_eq!(
            bins(&report, "Top.cg", 0, "cd"),
            hits(&[("nc", 1), ("rep", 1), ("other", 0)]),
            "{context}"
        );
        assert_eq!(
            rows(&report, "Top.cg", 0, "x").0[0],
            ("t".to_string(), celox::BinReportKind::Bins, 1),
            "{context}"
        );
    }
}
