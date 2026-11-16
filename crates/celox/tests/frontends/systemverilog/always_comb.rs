use super::*;

// A constant select that an unrolled loop makes reach past the vector reads
// X for the bits outside it, or 0 in two-state simulation (IEEE 1800-2023
// 11.5.1), also in the branch of a conditional the iteration does not take.
const OUT_OF_RANGE_SELECTS: &str = r#"
    module Top(input logic [4:0] stat, input logic [0:3] up, input logic [1:0][3:0] pk,
               output logic [4:0] after, shifted, hit, cnt, comb, output logic [3:0] down, nested);
      integer i;
      always @(*) begin
        for (i = 4; i >= 0; i = i - 1) begin
          after[i] = (i != 4) ? stat[i + 1] : 1'b1;
          shifted[i] = stat[i + 1];
          hit[i] = stat[i + 1] inside {1'b1};
        end
        for (i = 0; i < 4; i = i + 1) down[i] = up[i + 1];
        for (i = 0; i < 5; i = i + 1) cnt[i] = $countones(stat[i + 1]);
        for (i = 0; i < 4; i = i + 1) nested[i] = pk[1][i + 1];
      end
      always_comb
        for (int k = 0; k < 5; k++) comb[k] = stat[k + 1];
    endmodule"#;

/// Unrolled selects past a vector inside a run-time index and of a signed
/// packed element.
const OUT_OF_RANGE_OPERANDS: &str = r#"
    module Top(input logic [3:0] data, input logic [4:0] addr, input logic [7:0] pk,
               output logic [4:0] idx, part, output logic [1:0] sg);
      typedef logic signed [3:0] n_t;
      n_t [1:0] a;
      assign a = pk;
      always_comb begin
        for (int i = 0; i < 5; i++) idx[i] = data[addr[i + 1]];
        for (int i = 0; i < 5; i++) part[i] = data[addr[i + 1 -: 2]];
        for (int i = 0; i < 2; i++) sg[i] = a[i + 1] == -1;
      end
    endmodule"#;

fn out_of_range_operands<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, four_state: bool) {
    let (data, addr, pk) = (sim.signal("data"), sim.signal("addr"), sim.signal("pk"));
    sim.modify(|io| {
        io.set(data, 0b0110u8);
        io.set(addr, 0b10110u8);
        io.set(pk, 0xf5u8);
    })
    .unwrap();
    // `addr[5]` and `a[2]` read X, or 0 in two states; so does the upper
    // bit of `addr[5 -: 2]`, whose lower one is `addr[4]` (Icarus: x1101).
    let unknown = |mask: u8| if four_state { mask } else { 0 };
    for (name, value, mask) in [
        ("idx", 0b01011u8, unknown(0b10000)),
        ("part", 0b11101, unknown(0b10000)),
        ("sg", 0b01, unknown(0b10)),
    ] {
        let (bits, actual) = sim.get_four_state(sim.signal(name));
        assert_eq!(actual, BigUint::from(mask), "{name}");
        assert_eq!(
            bits & BigUint::from(!mask),
            BigUint::from(value & !mask),
            "{name}"
        );
    }
}

sv_backends! {
    fn unrolled_selects_past_operands_read_their_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(OUT_OF_RANGE_OPERANDS, Path::new("out_of_range_operands.sv"))], "Top");
        out_of_range_operands(&mut sim, false);
    }

    fn unrolled_selects_past_operands_read_their_state_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(OUT_OF_RANGE_OPERANDS, Path::new("out_of_range_operands.sv"))], "Top").four_state(true);
        out_of_range_operands(&mut sim, true);
    }
}

sv_backends! {
    // Also when the vector may be forced, which wraps its reads.
    fn unrolled_selects_past_a_forceable_vector_read_x(sim) {
        @setup { let source = OUT_OF_RANGE_SELECTS; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("out_of_range.sv"))], "Top")
            .four_state(true)
            .native_force_support(true)
            .signal_access(celox::SignalAccessOptions {
                force_signals: vec!["Top.stat".into()],
                ..Default::default()
            });
        let (stat, up, pk) = (sim.signal("stat"), sim.signal("up"), sim.signal("pk"));
        sim.modify(|io| {
            io.set(stat, 0b01101u8);
            io.set(up, 0b1010u8);
            io.set(pk, 0b1011_0110u8);
        })
        .unwrap();
        sim.run_until(0).unwrap();
        let (bits, mask) = sim.get_four_state(sim.signal("shifted"));
        assert_eq!(mask, BigUint::from(0b10000u8));
        assert_eq!(bits & BigUint::from(0b01111u8), BigUint::from(0b00110u8));
    }

    fn unrolled_selects_past_the_vector_read_zero_in_two_state(sim) {
        @setup { let source = OUT_OF_RANGE_SELECTS; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("out_of_range.sv"))], "Top");
        let (stat, up, pk) = (sim.signal("stat"), sim.signal("up"), sim.signal("pk"));
        sim.modify(|io| {
            io.set(stat, 0b01101u8);
            io.set(up, 0b1010u8);
            io.set(pk, 0b1011_0110u8);
        })
        .unwrap();
        for (name, value) in [
            ("after", 0b10110u8),
            ("shifted", 0b00110),
            ("hit", 0b00110),
            ("cnt", 0b00110),
            ("comb", 0b00110),
            ("down", 0b0010),
            ("nested", 0b0101),
        ] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(value), "{name}");
        }
    }

    fn unrolled_selects_past_the_vector_read_x(sim) {
        @setup { let source = OUT_OF_RANGE_SELECTS; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("out_of_range.sv"))], "Top").four_state(true);
        let (stat, up, pk) = (sim.signal("stat"), sim.signal("up"), sim.signal("pk"));
        sim.modify(|io| {
            io.set(stat, 0b01101u8);
            io.set(up, 0b1010u8);
            io.set(pk, 0b1011_0110u8);
        })
        .unwrap();
        sim.run_until(0).unwrap();
        for (name, value, unknown) in [
            ("after", 0b10110u8, 0u8),
            ("shifted", 0b00110, 0b10000),
            ("hit", 0b00110, 0b10000),
            ("cnt", 0b00110, 0),
            ("comb", 0b00110, 0b10000),
            ("down", 0b0010, 0b1000),
            ("nested", 0b0101, 0b1000),
        ] {
            let (bits, mask) = sim.get_four_state(sim.signal(name));
            assert_eq!(mask, BigUint::from(unknown), "{name}");
            assert_eq!(bits & BigUint::from(!unknown & 0x1f), BigUint::from(value), "{name}");
        }
    }
}

/// A select past a four-state variable whose bits the block already knows
/// reads X past it all the same: the variable is four-state whatever it
/// holds (Icarus: x1010, Verilator, two-state: 01010), also inside a
/// run-time index, where `d[X]` reads X (Icarus: x101, Verilator: 0101).
const OUT_OF_RANGE_KNOWN: &str = r#"
    module Top(input logic [1:0] d, output logic [4:0] y, output logic [3:0] z);
      always_comb begin
        logic [3:0] v;
        v = 4'b1010;
        for (int i = 0; i < 5; i++) y[i] = v[i];
        for (int i = 0; i < 4; i++) z[i] = d[v[i + 1]];
      end
    endmodule"#;

sv_backends! {
    fn unrolled_selects_past_known_values_read_their_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(OUT_OF_RANGE_KNOWN, Path::new("known.sv"))], "Top");
        let d = sim.signal("d");
        sim.modify(|io| io.set(d, 0b10u8)).unwrap();
        assert_eq!(sim.get_four_state(sim.signal("y")), (BigUint::from(0b01010u8), BigUint::from(0u8)));
        assert_eq!(sim.get_four_state(sim.signal("z")), (BigUint::from(0b0101u8), BigUint::from(0u8)));
    }

    fn unrolled_selects_past_known_values_read_their_state_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(OUT_OF_RANGE_KNOWN, Path::new("known.sv"))], "Top").four_state(true);
        let d = sim.signal("d");
        sim.modify(|io| io.set(d, 0b10u8)).unwrap();
        let (bits, mask) = sim.get_four_state(sim.signal("y"));
        assert_eq!(mask, BigUint::from(0b10000u8));
        assert_eq!(bits & BigUint::from(0b01111u8), BigUint::from(0b1010u8));
        let (bits, mask) = sim.get_four_state(sim.signal("z"));
        assert_eq!(mask, BigUint::from(0b1000u8));
        assert_eq!(bits & BigUint::from(0b0111u8), BigUint::from(0b101u8));
    }
}

sv_backends! {
    // A loop whose condition depends on the values of the process, such as
    // one that stops at the first sign change, runs in a continuous
    // assignment's function and in an `always_comb` as the values make it
    // stop (IEEE 1800-2023 12.7).
    fn loops_with_value_dependent_conditions(sim) {
        @setup {
            let source = r#"
            module Top(input logic [15:0] a, output logic [3:0] lead, output logic [4:0] ones);
              function [3:0] lead_of(input [15:0] v);
                reg [3:0] i;
                reg done;
                begin
                  done = 0;
                  lead_of = 15;
                  for (i = 14; done == 0; i = i - 1) begin
                    if (v[i + 1] != v[i]) begin
                      lead_of = 14 - i;
                      done = 1;
                    end else if (i == 0) done = 1;
                  end
                end
              endfunction
              assign lead = lead_of(a);
              always_comb begin
                int k;
                ones = 0;
                k = 0;
                while (k < 16 && a[k]) begin
                  ones = ones + 1;
                  k = k + 1;
                end
              end
            endmodule"#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("value_loops.sv"))], "Top");
        let a = sim.signal("a");
        for (value, lead, ones) in [
            (0x0f00u16, 3u8, 0u8),
            (0xfff7, 11, 3),
            (0x0000, 15, 0),
            (0x7fff, 0, 15),
            (0xffff, 15, 16),
        ] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("lead")), BigUint::from(lead), "{value:#x}");
            assert_eq!(sim.get(sim.signal("ones")), BigUint::from(ones), "{value:#x}");
        }
    }
}
