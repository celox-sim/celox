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
