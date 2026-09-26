use crate::Design;

cases! { Combinational, "false_loop";


fn test_read_then_overwrite_convergence(sim) {
    // This exercises Celox's fixed-point scheduling. The Veryl simulator
    // adapter evaluates these continuous assignments in source order.
    @setup { let code = r#"
        module Top (
            i: input logic,
            o: output logic,
        ) {
            var a: logic<2>;
            assign a[0] = a[1];
            assign a[1] = i;
            assign o = a[0];
        }
    "#; }
    @build Design::new(code, "Top");

    let i_port = sim.signal("i");
    let o_port = sim.signal("o");

    sim.modify(|io| io.set(i_port, 1u8)).unwrap();

    let result = sim.get(o_port);

    assert_eq!(
        result,
        1u8.into(),
        "The false loop failed to converge to the stable state (Fixed Point)"
    );
}
}
