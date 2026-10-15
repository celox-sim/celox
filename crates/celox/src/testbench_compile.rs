use crate::ir::{AbsoluteAddr, FrontendLookup, RuntimeSchema};
use celox_frontend_veryl::VerylTestbenchSource;
use celox_testbench::TestbenchProgram;

pub(crate) fn project_observability(
    lookup: &FrontendLookup,
    runtime_schema: &mut RuntimeSchema<AbsoluteAddr>,
    source: &VerylTestbenchSource,
) -> Result<(), celox_frontend_veryl::ParserError> {
    let (sites, read_variables) =
        celox_frontend_veryl::collect_testbench_observability(lookup, source)?;
    // Kernels register their own sites; the roots still keep the state the
    // testbench reads alive.
    if !source.kernels {
        runtime_schema.runtime_event_sites.extend(sites);
    }
    runtime_schema.testbench_read_roots.extend(read_variables);
    Ok(())
}

pub(crate) fn compile_semantic_testbench(
    lookup: &FrontendLookup,
    runtime_event_site_count: usize,
    source: &VerylTestbenchSource,
    random_seed: Option<u64>,
) -> Result<Option<TestbenchProgram<AbsoluteAddr>>, celox_frontend_veryl::ParserError> {
    celox_frontend_veryl::compile_semantic_testbench(
        lookup,
        source,
        runtime_event_site_count,
        random_seed,
    )
}

#[cfg(all(test, feature = "systemverilog"))]
mod process_root_tests {
    use crate::{OptLevel, Simulation, Simulator};
    #[test]
    fn semantic_testbench_projection_preserves_process_counter_roots() {
        let veryl = r#"#[test(Top)]module Top{inst clk:$tb::clock_gen;var q:logic;inst dut:$sv::Pulse(q);initial{clk.next(2);$assert(q==1);$finish();}}"#;
        let sv = "module Pulse(output bit q);bit clk;always @(posedge clk)q<=1;initial begin #1;clk=1;clk=0;end endmodule";
        let sim = Simulator::from_mixed_sources(
            vec![(veryl, std::path::Path::new("top.veryl"))],
            vec![(sv, std::path::Path::new("pulse.sv"))],
            "Top",
        )
        .testbench_kernels(false)
        .opt_level(OptLevel::O2)
        .build_interpreter()
        .unwrap();
        let schema = &sim.program.runtime_schema;
        assert!(!schema.process_event_counters.is_empty());
        for counters in &schema.process_event_counters {
            assert!(schema.testbench_read_roots.contains(&counters.rises));
            assert!(schema.testbench_read_roots.contains(&counters.falls));
        }
        let mut sim = Simulation::new(sim);
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    }
}
