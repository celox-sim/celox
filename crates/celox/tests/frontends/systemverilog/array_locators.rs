use super::*;

sv_backends! {
    fn array_locator_find(sim) { @case "array_locators::array_locator_find"; }
    fn array_locator_min_max_unique(sim) { @case "array_locators::array_locator_min_max_unique"; }
    fn array_locator_associative(sim) { @case "array_locators::array_locator_associative"; }
    fn array_locator_fixed(sim) { @case "array_locators::array_locator_fixed"; }
    fn array_locator_class_predicates(sim) { @case "array_locators::array_locator_class_predicates"; }
    fn array_locator_index_argument(sim) { @case "array_locators::array_locator_index_argument"; }
    fn array_locator_four_state(sim) { @case "array_locators::array_locator_four_state"; }
    fn array_locator_fixed_struct_enum(sim) { @case "array_locators::array_locator_fixed_struct_enum"; }
    fn array_locator_struct_elements(sim) { @case "array_locators::array_locator_struct_elements"; }
    fn array_locator_iterator_names(sim) { @case "array_locators::array_locator_iterator_names"; }
    fn array_locator_unique_class_handles(sim) { @case "array_locators::array_locator_unique_class_handles"; }
    fn array_locator_ordering_methods_with_expressions(sim) { @case "array_locators::ordering_methods_with_expressions"; }

    // An unknown key orders no element (IEEE 1800-2023 11.4.4): a sort with
    // a with expression keeps unknown elements where they are, as `sort()`
    // does, and sorts the known runs between them.
    fn array_locator_ordering_with_unknown_keys(sim) {
        @setup { let source = r#"
    module Top(output logic [15:0] keyed, output logic [15:0] plain);
      logic [3:0] q[$], r[$];
      initial begin
        q = '{4'd4, 4'bxxxx, 4'd3, 4'd1};
        r = q;
        q.sort with (item);
        r.sort();
        keyed = {q[0], q[1], q[2], q[3]};
        plain = {r[0], r[1], r[2], r[3]};
      end
    endmodule
  "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ordering_unknown_keys.sv"))], "Top").four_state(true);
        sim.run_until(0).unwrap();
        let keyed = sim.get_four_state(sim.signal("keyed"));
        assert_eq!(keyed, sim.get_four_state(sim.signal("plain")));
        assert_eq!(keyed.1, BigUint::from(0x0f00u32));
        assert_eq!(keyed.0 & BigUint::from(0xf0ffu32), BigUint::from(0x4013u32));
    }
}
