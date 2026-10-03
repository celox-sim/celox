use crate::Design;

cases! { Hierarchy, "hierarchical_assignment";
    fn selections(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/hierarchical_assignment_selections.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
    fn disjoint_loop(sim) {
        @setup { let code = include_str!("../../fixtures/testbench/hierarchical_assignment_disjoint_loop.veryl"); }
        @build Design::new(code, "Top");
        sim.run_testbench().unwrap();
    }
}
