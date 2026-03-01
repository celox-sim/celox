use celox::Simulation;

#[test]
fn test_simulation_step() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset,
            cnt: output logic<8>
        ) {
            var counter: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    counter = 8'd0;
                } else {
                    counter = counter + 8'd1;
                }
            }
            assign cnt = counter;
        }
    "#;

    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    vsim.add_clock("clk", 10, 0); // period 10, delay 0
    let rst = vsim.signal("rst");
    let cnt = vsim.signal("cnt");

    // Release reset at t=0 (AsyncLow: rst=1 means inactive)
    vsim.modify(|io| io.set::<u8>(rst, 1)).unwrap();

    // Step 0: clk 0 -> 1 at t=0.
    vsim.step().unwrap();
    assert_eq!(vsim.time(), 0);
    let val0 = vsim.get(cnt);

    // Step 1: clk 1 -> 0 at t=5
    vsim.step().unwrap();
    assert_eq!(vsim.time(), 5);

    // Step 2: clk 0 -> 1 at t=10
    vsim.step().unwrap();
    assert_eq!(vsim.time(), 10);
    let val10 = vsim.get(cnt);

    // Assert that counter increments on clk edges
    assert!(val10 > val0);
}

#[test]
fn test_next_event_time() {
    let code = r#"
        module Top (
            clk: input clock
        ) {
            always_ff (clk) {}
        }
    "#;
    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    vsim.add_clock("clk", 100, 0); // period 100, delay 0

    assert_eq!(vsim.next_event_time(), Some(0));
    vsim.step().unwrap();
    assert_eq!(vsim.next_event_time(), Some(50));
    vsim.step().unwrap();
    assert_eq!(vsim.next_event_time(), Some(100));
}
