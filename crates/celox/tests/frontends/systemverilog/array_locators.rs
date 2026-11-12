use super::*;

sv_backends! {

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
