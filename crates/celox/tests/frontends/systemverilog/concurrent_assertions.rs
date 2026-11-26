use super::*;

/// A name in an `expect` resolves in the expect's scope (IEEE 1800-2023
/// 16.17): a task formal `p` is the formal, not the module's property `p`,
/// which an `expect (p)` outside the task instantiates. The body of `p`
/// binds where `p` is declared (16.8): with a task formal `a`, `p` still
/// reads the module's `a`.
#[test]
fn expect_names_resolve_in_their_scope() {
    let source = "`timescale 1ns/1ns
        module Top(output int r1, r2, r3);
            bit clk, a;
            property p;
                @(posedge clk) a;
            endproperty
            task automatic t(bit p, output int r);
                expect (@(posedge clk) p) r = 1; else r = 2;
            endtask
            task automatic u(bit a, output int r);
                expect (p) r = 1; else r = 2;
            endtask
            initial begin
                t(1, r1);
                expect (p) r2 = 1; else r2 = 2;
                u(1, r3);
            end
            initial repeat (4) begin #5 clk = 1; #5 clk = 0; end
        endmodule";
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("expect.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(50).unwrap();
        let (r1, r2, r3) = (sim.signal("r1"), sim.signal("r2"), sim.signal("r3"));
        assert_eq!(
            sim.get(r1),
            num_bigint::BigUint::from(1u8),
            "four-state {four_state}"
        );
        assert_eq!(
            sim.get(r3),
            num_bigint::BigUint::from(2u8),
            "four-state {four_state}"
        );
        assert_eq!(
            sim.get(r2),
            num_bigint::BigUint::from(2u8),
            "four-state {four_state}"
        );
    }
}

sv_backends! {
    // An assertion whose action calls a subroutine that reads its process
    // (`process::self`) runs in a process with a frame of its own. The
    // sampled copies and `$past` histories of its predicate are the
    // runtime's: it samples them at each time step and commits the
    // histories, so they are no state of the frame (IEEE 1800-2023 16.5.1,
    // 16.9.3).
    fn sampled_values_of_processes_with_frames(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                module Top(output int passes, output int passed_at, output int failures);
                    bit clk = 0;
                    bit q = 1;
                    int cycle = 0;
                    function automatic void note();
                        process p = process::self();
                        failures++;
                    endfunction
                    always @(posedge clk) begin
                        cycle <= cycle + 1;
                        if (cycle == 5) q = 0;
                    end
                    assert property (@(posedge clk) $fell(q)) begin
                        passes++;
                        passed_at = cycle;
                    end else note();
                    initial repeat (20) #5 clk = ~clk;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("framed_sampled.sv"))], "Top");
        sim.run_until(200).unwrap();
        // `q` falls at the sixth edge; the seventh samples the fall. The
        // action runs in the Reactive region, after that edge's nonblocking
        // update of `cycle` (IEEE 1800-2023 16.14.1, 4.4.2).
        assert_eq!(sim.get(sim.signal("passes")), 1u32.into());
        assert_eq!(sim.get(sim.signal("passed_at")), 7u32.into());
        assert_eq!(sim.get(sim.signal("failures")), 9u32.into());
    }
}

/// The pass action of an assertion runs on vacuous success too, by default
/// (IEEE 1800-2023 20.12): an until property behind `|->` or `|=>` passes
/// vacuously at each tick its antecedent misses, besides the attempts its
/// operands decide (16.12.12). Bit `t` of each vector is the value at tick
/// `t`.
#[test]
fn until_pass_actions_run_on_vacuous_success() {
    let source = "`timescale 1ns/1ns
        module Top(output int p1, f1, p2, f2, p3, f3, p4, f4);
            bit clk;
            int t;
            bit [15:0] av = 16'hfcf7, bv = 16'h02c4, cv = 16'h8b2d;
            bit a, b, c;
            always @(posedge clk) t <= t + 1;
            assign a = av[t];
            assign b = bv[t];
            assign c = cv[t];
            assert property (@(posedge clk) c |-> a until b) p1++; else f1++;
            assert property (@(posedge clk) c |-> a s_until_with b) p2++; else f2++;
            assert property (@(posedge clk) c |=> a s_until b) p3++; else f3++;
            assert property (@(posedge clk) c |=> a until_with b) p4++; else f4++;
            initial repeat (16) begin #1 clk = 1; #1 clk = 0; end
        endmodule";
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("until.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(33).unwrap();
        let counts: Vec<u32> = ["p1", "f1", "p2", "f2", "p3", "f3", "p4", "f4"]
            .iter()
            .map(|name| {
                let signal = sim.signal(name);
                u32::try_from(sim.get(signal)).unwrap()
            })
            .collect();
        assert_eq!(
            counts,
            [12, 2, 11, 3, 12, 1, 11, 2],
            "four-state {four_state}"
        );
    }
}

/// Until properties outside the supported subset are rejected: operands
/// that are not Boolean expressions, an until as an operand of another
/// property operator, an instance of a named sequence as an operand, an
/// until in an `expect` statement, and one in a design that calls an
/// assertion control task.
#[test]
fn until_properties_outside_the_subset_are_rejected() {
    let cases = [
        "assert property (@(posedge clk) a ##1 b until c);",
        "assert property (@(posedge clk) (a until b) until c);",
        "assert property (@(posedge clk) a until b until c);",
        "assert property (@(posedge clk) a until (b until c));",
        "assert property (@(posedge clk) a ##1 (b until c));",
        "sequence s; a ##1 b; endsequence assert property (@(posedge clk) s until c);",
        "initial expect (@(posedge clk) a until b);",
        "initial $assertoff; assert property (@(posedge clk) a until b);",
    ];
    for items in cases {
        let source = format!(
            "`timescale 1ns/1ns
            module Top;
                bit clk, a, b, c;
                {items}
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("until.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("accepted: {items}"))
        .to_string();
        assert!(error.contains("until"), "{items}: {error}");
    }
    // Parentheses around an until nest as the groups of a sequence do, up to
    // the same depth.
    let nested = format!(
        "`timescale 1ns/1ns
        module Top;
            bit clk, a, b;
            assert property (@(posedge clk) {}a until b{});
        endmodule",
        "(".repeat(10_000),
        ")".repeat(10_000)
    );
    let error = celox::Simulation::from_sv_sources(
        vec![(nested.as_str(), std::path::Path::new("until.sv"))],
        "Top",
    )
    .build()
    .err()
    .unwrap_or_else(|| panic!("accepted 10000 parentheses around an until"))
    .to_string();
    assert!(error.contains("nested more than 64 deep"), "{error}");
}

/// When the simulation ends, the attempts of a strong until property still
/// pending fail (IEEE 1800-2023 16.12.12, F.5.3.1), unless the `disable iff`
/// condition became true after the last tick: those are disabled, and
/// neither pass nor fail (16.12). An untimed task may be the failure action
/// that runs then.
#[test]
fn strong_until_attempts_at_the_end_of_the_simulation() {
    let source = "`timescale 1ns/1ns
        module Top(output int held, output int reset_held);
            bit clk, a = 1, b, reset;
            task automatic count(inout int failures);
                failures++;
            endtask
            assert property (@(posedge clk) a s_until b) else count(held);
            assert property (@(posedge clk) disable iff (reset) a s_until b)
                else reset_held++;
            initial begin
                repeat (3) begin #1 clk = 1; #1 clk = 0; end
                #1 reset = 1;
                #1 $finish;
            end
        endmodule";
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("until.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(20).unwrap();
        assert!(sim.is_finished(), "four-state {four_state}");
        let held = sim.signal("held");
        let reset_held = sim.signal("reset_held");
        assert_eq!(
            (
                u32::try_from(sim.get(held)).unwrap(),
                u32::try_from(sim.get(reset_held)).unwrap(),
            ),
            (3, 0),
            "four-state {four_state}"
        );
    }
}

/// The action of an until property or a multiclocked sequence runs once for
/// each attempt in a loop, where it must not suspend either; an `expect`
/// whose until property has a named left operand is still an until
/// property in an `expect`.
#[test]
fn assertion_actions_run_per_attempt_are_checked() {
    let cases = [
        (
            "assert property (@(posedge clk) a until b) #1 passed++;",
            "may suspend",
        ),
        (
            "assert property (@(posedge clk) a ##1 @(negedge clk) b) #1 passed++;",
            "may suspend",
        ),
        (
            "property p; a; endproperty initial expect (@(posedge clk) p s_until b) passed++;",
            "until",
        ),
    ];
    for (items, reason) in cases {
        let source = format!(
            "`timescale 1ns/1ns
            module Top;
                bit clk, a, b;
                int passed;
                {items}
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("actions.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("accepted: {items}"))
        .to_string();
        assert!(error.contains(reason), "{items}: {error}");
    }
}
