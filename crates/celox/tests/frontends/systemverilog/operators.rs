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

// A replication count is a constant expression (IEEE 1800-2023 11.4.12),
// here a concatenation of parameters, one of which an enumeration member
// gives, as Ariane's `ValidRule = {{NrRegion * NB_PERIPHERALS}{1'b1}}`
// does in a package. The parameters are known when the enumeration members
// are, after the scope's first pass over its parameters.
#[test]
fn replication_counts_of_parameters_given_by_enumeration_members() {
    let source = r#"
        `timescale 1ns/1ns
        package soc;
            typedef enum int unsigned { DRAM = 0, GPIO = 1, Debug = 9 } slaves_t;
            localparam NB = Debug + 1;
            localparam NrRegion = 2;
            localparam logic [NrRegion-1:0][NB-1:0] ValidRule = {{NrRegion * NB}{1'b1}};
        endpackage
        module Top;
            typedef enum logic [1:0] { A, B, C } abc_t;
            localparam W = C + 1;
            localparam logic [2*W-1:0] MASK = {{W}{2'b10}};
            initial begin
                #1 $display("%b %0d", soc::ValidRule, $bits(soc::ValidRule));
                $display("%b", MASK);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("counts.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            ["11111111111111111111 20", "101010"],
            "four_state={four_state}"
        );
    }
}

// A count that stays unknown once every parameter of the scope is collected
// is still rejected, and so is a replication of a value that is no constant,
// whether or not its count is known yet.
#[test]
fn replication_counts_of_unresolved_parameters_are_rejected() {
    for source in [
        "module Top(output logic [7:0] y);
            localparam A = undefined_name + 1;
            localparam logic [7:0] V = {{A}{1'b1}};
            assign y = V;
        endmodule",
        "module Top #(parameter N) (input logic sig, output logic [7:0] y);
            localparam logic [7:0] P = {{N}{sig}};
            assign y = '0;
        endmodule",
    ] {
        assert!(
            Simulator::from_sv_sources(vec![(source, Path::new("unresolved.sv"))], "Top")
                .build_cranelift()
                .is_err(),
            "{source}"
        );
    }
}
