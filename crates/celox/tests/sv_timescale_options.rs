#![cfg(feature = "systemverilog")]
//! Celox's default timing configuration must preserve authored scope rounding.
use std::path::Path;

macro_rules! timing {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let source = r#"
module Child(output bit value);
    timeunit 1ns / 1ps;
    initial begin value = 0; #1.5ps value = 1; end
endmodule
module Top(output bit default_value, explicit_value, queries);
    Child child(explicit_value);
    assign queries = ($timeunit() == -9) && ($timeprecision() == -15)
        && ($timeprecision(child) == -12) && ($timeprecision($root) == -15);
    initial begin default_value = 0; #1.5ps default_value = 1; end
endmodule
"#;
            let mut policy = celox::DiagnosticPolicy::default();
            policy.set_override("mixed_timescale=allow").unwrap();
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("configured_timing.sv"))],
                "Top",
            )
            .sv_timescale(Some("1ns/1fs".parse().unwrap()))
            .diagnostic_policy(policy)
            .$backend()
            .unwrap();
            sim.run_until(1499).unwrap();
            assert_eq!(sim.get(sim.signal("queries")), 1u8.into());
            assert_eq!(sim.get(sim.signal("default_value")), 0u8.into());
            sim.run_until(1500).unwrap();
            assert_eq!(sim.get(sim.signal("default_value")), 1u8.into());
            sim.run_until(1999).unwrap();
            assert_eq!(sim.get(sim.signal("explicit_value")), 0u8.into());
            sim.run_until(2000).unwrap();
            assert_eq!(sim.get(sim.signal("explicit_value")), 1u8.into());
        }
    };
}
timing!(interpreter, build_interpreter);
timing!(cranelift, build);
timing!(wasm, build_wasm);
#[cfg(target_arch = "x86_64")]
timing!(native, build_native);

#[test]
fn configuring_defaults_does_not_relax_mixed_timescale_diagnostics() {
    let source = "module Child; timeunit 1ns/1ps; endmodule module Top; Child child(); endmodule";
    let error = celox::Simulator::from_sv_sources(vec![(source, Path::new("mixed.sv"))], "Top")
        .sv_timescale(Some("1ns/1fs".parse().unwrap()))
        .build_interpreter()
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("mixed explicit and default time"),
        "{error}"
    );
}

#[test]
fn mixed_language_interface_probes_receive_configured_timing() {
    let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
    let bus = "interface Bus; logic [7:0] req, rsp; assign rsp = req + ($timeprecision($root) == -15); endinterface";
    let mut sim = celox::Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(bus, Path::new("bus.sv"))],
        "Top",
    )
    .sv_timescale(Some("1ns/1fs".parse().unwrap()))
    .build_interpreter()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 41u8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 42u8.into());
}
