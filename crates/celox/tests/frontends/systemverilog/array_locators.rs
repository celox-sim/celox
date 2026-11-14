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

sv_backends! {
    // `sort` and `rsort` with a `with` expression order elements kept as
    // records, structures with string members (IEEE 1800-2023 7.12.2), by
    // a member of the iterator, stably: uvm-1.2's
    // `m_time_settings.sort() with (item.offset)`.
    fn ordering_with_orders_elements_held_in_records(sim) {
        @setup { let source = r#"
    module Top(output int order, output int counts);
      typedef struct { string comp; string phase; time offset; int verbosity; string id; } s_t;
      class C;
        static s_t settings[$];
        static function void add(string c, time t, int v);
          s_t s;
          s.comp = c;
          s.offset = t;
          s.verbosity = v;
          settings.push_back(s);
        endfunction
      endclass
      s_t q[$];
      function automatic int code(string s);
        return s.getc(0) - "a" + 1;
      endfunction
      initial begin
        C::add("c", 30, 3);
        C::add("a", 10, 1);
        C::add("b", 20, 2);
        C::add("d", 10, 4);
        C::settings.sort() with (item.offset);
        q = C::settings;
        q.rsort() with (item.comp);
        order = 0;
        foreach (C::settings[i]) order = order * 10 + code(C::settings[i].comp);
        foreach (q[i]) order = order * 10 + code(q[i].comp);
        q.sort(x) with (x.verbosity);
        counts = code(q[0].comp) * 10 + code(q[3].comp) + 100 * q.size();
      end
    endmodule
  "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ordering_records.sv"))], "Top");
        sim.run_until(0).unwrap();
        // a d b c (stable by offset), then d c b a (by comp, descending).
        let order = sim.signal("order");
        assert_eq!(sim.get(order), BigUint::from(14_234_321u32));
        let counts = sim.signal("counts");
        assert_eq!(sim.get(counts), BigUint::from(414u32));
    }
}

sv_backends! {
    // Elements wider than 64 bits are kept as records too; a `with`
    // expression orders them by its key (IEEE 1800-2023 7.12.2).
    fn ordering_with_orders_wide_elements(sim) {
        @setup { let source = r#"
    module Top(output int low);
      bit [99:0] q[$];
      initial begin
        q = {{96'd1, 4'd3}, {96'd2, 4'd1}, {96'd3, 4'd2}};
        q.sort with (item[3:0]);
        low = q[0][99:4] * 100 + q[1][99:4] * 10 + q[2][99:4];
      end
    endmodule
  "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ordering_wide.sv"))], "Top");
        sim.run_until(0).unwrap();
        let low = sim.signal("low");
        assert_eq!(sim.get(low), BigUint::from(231u32));
    }
}
