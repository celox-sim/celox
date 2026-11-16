use super::*;

sv_backends! {

    // Compare cached runtime calls with elaboration-time constants through
    // Celox's four-state API, including the payload and mask of unknown bits.
    fn constant_function_inside_unknowns(sim) {
        @setup { let source = r#"
    module Top(output logic [4:0] q, output logic [4:0] p);
      function automatic logic unknown_match();
        logic x;
        return x inside {1'b1};
      endfunction
      function automatic logic definite_match(); return 1'bx inside {1'b0, 1'b?}; endfunction
      function automatic logic definite_mismatch(); return 2'bx0 inside {2'b11}; endfunction
      function automatic logic unknown_range(); return 1'bx inside {[1'b0:1'b1]}; endfunction
      function automatic logic empty_range(); return 2'b11 inside {[2'bxx:2'b01]}; endfunction
      localparam logic [4:0] P = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
      assign p = P;
      assign q = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
    endmodule
  "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constant_inside.sv"))], "Top").four_state(true);
        for name in ["q", "p"] {
            assert_eq!(sim.get_four_state(sim.signal(name)), (BigUint::from(0x0bu8), BigUint::from(0x09u8)), "{name}");
        }
    }

}

fn display_lines(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("operators.sv"))], "top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(10).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

/// The X and Z bits of a parameter value are don't-care bits of `inside` and
/// `==?` (IEEE 1800-2023 11.4.6, 11.4.13) also when the comparison reads an
/// element of a packed array parameter, here one an instantiation overrides
/// as the match addresses of a decoder. Values agree with Verilator 5.054.
#[test]
fn wildcard_comparisons_read_unknown_bits_of_parameter_elements() {
    let source = r#"`timescale 1ns/1ns
module dec #(parameter els_p = 2, parameter [els_p-1:0][19:0] base_addr_p = '0)
  (input [19:0] addr, output logic [els_p-1:0] m);
  for (genvar i = 0; i < els_p; i++) begin : g
    assign m[i] = (addr inside {base_addr_p[i]});
  end
endmodule
module top;
  localparam a_gp = 20'h2_????;
  localparam b_gp = 20'h0_1???;
  localparam [19:0] t_gp = 20'h2_????;
  localparam [1:0][19:0] arr = {a_gp, b_gp};
  logic [19:0] addr;
  logic [1:0] m, n;
  dec #(.els_p(2), .base_addr_p({a_gp, b_gp})) d(.addr(addr), .m(m));
  dec #(.els_p(2), .base_addr_p(arr)) e(.addr(addr), .m(n));
  task show;
    $display("%h %b%b%b%b%b %b %b", addr, addr inside {a_gp}, addr inside {t_gp},
             addr inside {arr[1]}, addr ==? arr[1], addr[15:0] ==? arr[0][15:0], m, n);
  endtask
  initial begin
    addr = 20'h20000; #1 show();
    addr = 20'h20004; #1 show();
    addr = 20'h01008; #1 show();
    addr = 20'h30000; #1 show();
  end
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            display_lines(source, four_state),
            [
                "20000 11110 10 10",
                "20004 11110 10 10",
                "01008 00001 01 01",
                "30000 00000 00 00",
            ],
            "four_state={four_state}"
        );
    }
}

/// A parameter, and an element of a packed array parameter, keep the Z bits
/// of their value (IEEE 1800-2023 5.7.1) in four-state simulation.
#[test]
fn parameter_values_keep_unknown_bits() {
    let source = r#"`timescale 1ns/1ns
module top;
  localparam a_gp = 20'h2_????;
  localparam [19:0] t_gp = 20'h2_????;
  localparam [1:0][19:0] arr = {20'h2_????, 20'h0_1???};
  initial $display("%h %h %h %h %h", a_gp, t_gp, arr[1], arr[0][15:0], arr);
endmodule"#;
    assert_eq!(
        display_lines(source, true),
        ["2zzzz 2zzzz 2zzzz 1zzz 2zzzz01zzz"]
    );
}
