use crate::Design;

cases! { ControlFlow, "concurrent_initial";
    fn shared_clock(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_shared_clock.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn control_flow(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_control_flow.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn clock_periods(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_clock_periods.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn simultaneous_edges(sim) {
        @tags [TwoStateInitialization];
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_simultaneous_edges.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn reset(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_reset.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn reset_between_edges(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_reset_between_edges.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn hierarchy(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_hierarchy.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn mixed_edges(sim) {
        @tags [TwoStateInitialization];
        @setup { let code = include_str!("../../fixtures/testbench/concurrent_initial_mixed_edges.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
}
