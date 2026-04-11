use celox::{RuntimeErrorCode, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Converging true loop via assign statements.
// assign passes the analyzer (no UnassignVariable), and true_loop
// declaration allows the SIR scheduler to accept the cycle.
fn test_converging_true_loop_with_assign(sim) {
    @setup { let code = r#"
        module Top (i: input logic<2>, o: output logic<2>) {
            var v: logic<2>;
            assign v[0] = v[1] ^ i[0];
            assign v[1] = v[0] ^ i[1];
            assign o = v;
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top")
        .true_loop(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
            10,
        );

    let i_port = sim.signal("i");
    let o_port = sim.signal("o");

    // i=0b00 → v[0]=v[1]^0, v[1]=v[0]^0 → v[0]=v[1], v[1]=v[0] → converges to 0
    sim.modify(|io| io.set(i_port, 0u8)).unwrap();
    assert_eq!(sim.get(o_port), 0u8.into());
}

// Non-converging true loop: oscillation detected at runtime.
// Uses cross-bit assign to bypass the analyzer's UnassignVariable check.
fn test_true_loop_oscillation_detected(sim) {
    @omit_veryl;
    @setup {
    // v[0] = ~v[1] & a, v[1] = v[0]
    // When a=1: v[0]=~v[1], v[1]=v[0] → oscillates (0,0)→(1,0)→(1,1)→(0,1)→(0,0)→...
    let code = r#"
        module Top (a: input logic, y: output logic) {
            var v: logic<2>;
            assign v[0] = ~v[1] & a;
            assign v[1] = v[0];
            assign y = v[0];
        }
    "#;
    }
    @build SimulatorBuilder::new(code, "Top")
        .true_loop(
            (vec![], vec!["v".to_string()]),
            (vec![], vec!["v".to_string()]),
            10,
        );

    let id_a = sim.signal("a");
    // a=1 triggers oscillation
    sim.modify(|io| io.set(id_a, 1u8)).unwrap();
    let res = sim.eval_comb();
    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

}
