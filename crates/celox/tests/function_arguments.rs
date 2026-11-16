use celox::Simulator;

#[test]
fn test_ff_inout_copyout_directly_to_register_is_rejected() {
    let code = r#"
        module Top (clk: input clock, state: output logic<8>, returned: output logic<8>) {
            function advance (value: inout logic<8>) -> logic<8> {
                let previous: logic<8> = value;
                value += 8'd1;
                return previous;
            }
            always_ff (clk) {
                returned = advance(state);
            }
        }
    "#;
    let error = Simulator::builder(code, "Top").build().unwrap_err();
    let celox::SimulatorErrorKind::Analyzer(errors) = error.kind() else {
        panic!("expected analyzer rejection, got: {error:?}");
    };
    assert!(
        errors.iter().any(|error| matches!(
            error,
            veryl_analyzer::AnalyzerError::SideEffectFunctionCallInAlwaysFf { .. }
        )),
        "expected SideEffectFunctionCallInAlwaysFf, got: {errors:?}"
    );
}

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_comb_output_copyout_observer_sees_formal_sign_extension(sim) {
    @omit_veryl;
    // The expected values follow Celox's Veryl order, which writes the later
    // parts of a concatenated destination before it evaluates the select of an
    // earlier part. In SystemVerilog every position of the destination is
    // evaluated before the assignment (IEEE 1800-2023 10.4.1, 11.4.12).
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (
            d: input logic<8>, copied: output logic<16>,
            bits: output logic<2>, returned: output logic,
        ) {
            function observe (x: input logic<16>) -> logic {
                $display("copied=%0d", x);
                return x[15];
            }
            function produce (x: input logic<8>, dst: output signed logic<8>) -> logic {
                dst = x;
                return 1'b1;
            }
            always_comb {
                copied = 16'd0;
                bits = 2'b00;
                returned = produce(d, {bits[observe(copied)], copied});
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let copied = sim.signal("copied");
    let bits = sim.signal("bits");
    let returned = sim.signal("returned");
    sim.drain_runtime_events();

    for value in [0x80u8, 0x7f, 0xff, 0] {
        sim.modify(|io| io.set(d, value)).unwrap();
        let expected = value as i8 as i16 as u16;
        assert_eq!(sim.get(copied), expected.into());
        let expected_bits = if value & 0x80 != 0 { 2u8 } else { 0 };
        assert_eq!(sim.get(bits), expected_bits.into());
        assert_eq!(sim.get(returned), 1u8.into());
        assert_eq!(
            sim.drain_runtime_events(),
            vec![celox::RuntimeEvent::Display {
                message: format!("copied={expected}").into_bytes(),
            }],
        );
    }
}

}
