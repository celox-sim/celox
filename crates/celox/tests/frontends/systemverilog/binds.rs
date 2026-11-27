//! Bind directives (IEEE 1800-2023 23.11): the instances they make are in
//! the hierarchy of their targets.
use super::*;

sv_backends! {
    /// A bound instance is a scope of each target instance: `%m` and the
    /// signal paths name it under the target, not under the module that
    /// holds the directive.
    fn bound_instances_are_scopes_of_their_targets(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module probe(input logic [7:0] in, output logic [7:0] out);
    assign out = in + 8'd1;
    initial #1 $display("%m %0d", out);
endmodule
module unit(input logic [7:0] x);
    logic [7:0] y;
    assign y = x * 2;
endmodule
module Top;
    unit ua(.x(8'd3));
    unit ub(.x(8'd5));
    bind unit probe d(.in(y), .out());
endmodule
"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("binds.sv"))], "Top");
        sim.run_until(1).unwrap();
        for (unit, value) in [("ua", 7u32), ("ub", 11)] {
            let signal = sim.child_signal(&[(unit, 0), ("d", 0)], "out");
            assert_eq!(sim.get(signal), BigUint::from(value), "{unit}");
        }
        let mut displays = sim.drain_runtime_events();
        displays.sort_by_key(|event| format!("{event:?}"));
        assert_eq!(displays, vec![
            celox::RuntimeEvent::Display { message: b"Top.ua.d 7".to_vec() },
            celox::RuntimeEvent::Display { message: b"Top.ub.d 11".to_vec() },
        ]);
    }

    /// A module may bind into itself: its bound instance drives its net
    /// output port like one it declares.
    fn modules_bind_into_themselves(sim) {
        @setup {
            let source = r#"
module Driver(output logic y);
    assign y = 1'b1;
endmodule
module Top(output wire y);
    bind Top Driver driver(.y(y));
endmodule
"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("binds.sv"))], "Top");
        sim.run_until(0).unwrap();
        let y = sim.signal("y");
        assert_eq!(sim.get(y), BigUint::from(1u32));
    }
}

/// Each source of a design is parsed on its own, so a bind directive binds
/// into the modules and interfaces of its own source; Celox rejects one
/// whose target another source declares, by name.
#[test]
fn binds_into_another_source_are_unsupported() {
    let target = "module unit(input logic x); endmodule module probe(input logic in); endmodule";
    let top = "module Top; unit u(.x(1'b0)); bind unit probe d(.in(x)); endmodule";
    let Err(error) = celox::Simulation::from_sv_sources(
        vec![(target, Path::new("unit.sv")), (top, Path::new("top.sv"))],
        "Top",
    )
    .build() else {
        panic!("a bind into another source was accepted");
    };
    let text = error.to_string();
    assert!(
        text.contains("bind directive whose target `unit`"),
        "{text}"
    );
}
