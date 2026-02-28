use celox::{RuntimeErrorCode, SimulatorBuilder};

#[test]
fn test_converging_true_loop_with_explicit_limit() {
    // A True Loop that performs a 4-bit shift operation.
    // The longest structural path (FAS+1) is 2, but 5 iterations are required for logical convergence.
    let code = r#"
        module Top (i: input logic, o: output logic<4>) {
            var v: logic<4>;
            // 自身の値を参照してシフトするTrue Loop
            // v = {v[2:0], i}
            assign v = (v << 1) | i;
            assign o = v;
        }
    "#;

    let result = SimulatorBuilder::new(code, "Top")
        // If true_loop is not specified, an OscillationDetected error should occur
        // at the 3rd iteration due to the default safety_limit (FAS+1 = 2).
        // Here, we explicitly provide N=10 to allow logical convergence.
        .true_loop(
            (vec![], vec!["v".to_owned()]), // From: Top.v
            (vec![], vec!["v".to_owned()]), // To:   Top.v (自己ループ)
            10,
        )
        .build();

    assert!(result.is_ok(), "Convergence should be allowed by true_loop");
    let mut sim = result.unwrap();

    let i_port = sim.signal("i");
    let o_port = sim.signal("o");
    assert!(sim.modify(|io| io.set(i_port, 1u8)).is_ok());
    assert_eq!(sim.get(o_port), 0xFu32.into());
    assert!(sim.modify(|io| io.set(i_port, 0u8)).is_ok());
    assert_eq!(sim.get(o_port), 0x0u32.into());
}

#[test]
fn test_true_loop_convergence_failure() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = ~y & a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .true_loop(
            (vec![], vec!["y".to_string()]),
            (vec![], vec!["y".to_string()]),
            10,
        )
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    // Initially a=0, so y=0 (stable)
    // Set a=1 to trigger oscillation
    sim.modify(|io| {
        io.set(id_a, 1u8);
    })
    .unwrap();

    // With lazy eval, oscillation is detected when eval_comb is explicitly called
    let res = sim.eval_comb();
    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}



