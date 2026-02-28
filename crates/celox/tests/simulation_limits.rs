use celox::Simulation;

/// [CAN DO] Clock Division
#[test]
fn test_can_clock_division() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset,
            cnt: output logic<8>
        ) {
            var div: logic;
            var counter: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    div = 1'b0;
                } else {
                    div = ~div;
                }
            }
            always_ff (div, rst) {
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
    vsim.add_clock("clk", 10, 0);
    let rst = vsim.signal("rst");
    let cnt = vsim.signal("cnt");

    vsim.modify(|io| io.set::<u8>(rst, 1)).unwrap();
    vsim.step().unwrap();
    vsim.modify(|io| io.set::<u8>(rst, 0)).unwrap();

    vsim.step().unwrap(); // t=5
    vsim.step().unwrap(); // t=10
    let val = vsim.get(cnt);
    assert!(val == 1u8.into() || val == 2u8.into());
}

/// [CAN DO] Clock Delay / Phase offset
#[test]
fn test_can_delay_clock() {
    let code = r#"
        module Top (clk: input clock) {
            always_ff (clk) {}
        }
    "#;
    let mut vsim = Simulation::builder(code, "Top").build().unwrap();

    // Clock with period 10, starting its first rising edge at t=5.
    vsim.add_clock("clk", 10, 5);

    // The first event should be at t=5
    assert_eq!(vsim.next_event_time(), Some(5));
    vsim.step().unwrap();
    assert_eq!(vsim.time(), 5);
}

/// [CAN DO] One-shot Scheduling (e.g. Asynchronous Reset Pulse)
#[test]
fn test_can_schedule_reset() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset_async_high,
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
    vsim.add_clock("clk", 10, 5); // Posedge at 5, 15, 25...

    // Schedule reset high at t=0, low at t=20
    vsim.schedule("rst", 0, 1).unwrap();
    vsim.schedule("rst", 20, 0).unwrap();

    vsim.run_until(40).unwrap();

    // At t=5, posedge clk, but rst is high -> counter remains 0
    // At t=15, posedge clk, but rst is high -> counter remains 0
    // At t=20, rst becomes low
    // At t=25, posedge clk, rst is low -> counter becomes 1
    // At t=35, posedge clk, rst is low -> counter becomes 2
    let cnt = vsim.signal("cnt");
    assert_eq!(vsim.get(cnt), 2u8.into());
}

/// [CANNOT DO] One-shot Scheduling of non-event signals
#[test]
fn test_cannot_schedule_non_event() {
    let code = r#"
        module Top (
            in_data:  input  logic,
            out_data: output logic
        ) {
            assign out_data = in_data;
        }
    "#;
    let mut vsim = Simulation::builder(code, "Top").build().unwrap();

    // "in_data" is not an event because it doesn't trigger any always_ff
    let res = vsim.schedule("in_data", 100, 1);
    assert!(res.is_err());
}

/// [CAN DO] Mixed Edge Triggering (Posedge and Negedge)
#[test]
fn test_mixed_edge_triggering() {
    let code = r#"
        module Top (
            clk:   input clock,
            clk_n: input clock_negedge,
            rst_n: input reset_async_low,
            out_p: output logic<8>,
            out_n: output logic<8>
        ) {
            var cnt_p: logic<8>;
            var cnt_n: logic<8>;

            always_ff (clk, rst_n) {
                if_reset {
                    cnt_p = 8'd0;
                } else {
                    cnt_p = cnt_p + 8'd1;
                }
            }

            always_ff (clk_n) {
                cnt_n = cnt_n + 8'd1;
            }

            assign out_p = cnt_p;
            assign out_n = cnt_n;
        }
    "#;

    let mut vsim = Simulation::builder(code, "Top").build().unwrap();

    // clk: posedge at 5, 15, 25...
    vsim.add_clock("clk", 10, 5);
    // clk_n: negedge at 5, 15, 25... (initial_delay=5 means first rising edge at 5, but we trigger on negedge)
    // Actually add_clock schedules first rising edge at initial_delay.
    // Rising edge at 5, falling edge at 10, rising at 15, falling at 20...
    vsim.add_clock("clk_n", 10, 5);

    // Initial state: rst_n = 0 (reset)
    vsim.schedule("rst_n", 0, 0).unwrap();
    vsim.schedule("rst_n", 12, 1).unwrap(); // release reset at 12

    let out_p = vsim.signal("out_p");
    let out_n = vsim.signal("out_n");

    vsim.run_until(50).unwrap();

    // Trace:
    // t=0: rst_n=0. cnt_p=0
    // t=5: clk↑, but rst_n=0 -> cnt_p=0. clk_n↑ -> cnt_n no change.
    // t=10: clk↓. clk_n↓ -> cnt_n becomes 1 (first negedge)
    // t=12: rst_n=1.
    // t=15: clk↑, rst_n=1 -> cnt_p becomes 1 (first posedge after reset)
    // t=20: clk_n↓ -> cnt_n becomes 2 (second negedge)
    // t=25: clk↑ -> cnt_p becomes 2
    // t=30: clk_n↓ -> cnt_n becomes 3
    // t=35: clk↑ -> cnt_p becomes 3
    // t=40: clk_n↓ -> cnt_n becomes 4
    // t=45: clk↑ -> cnt_p becomes 4
    // t=50: clk_n↓ -> cnt_n becomes 5

    assert_eq!(vsim.get(out_p), 4u8.into());
    assert_eq!(vsim.get(out_n), 5u8.into());
}

/// [CAN DO] Multiple Independent Clocks
#[test]
fn test_multiple_clocks() {
    let code = r#"
        module Top (
            clk0: input clock,
            clk1: input clock,
            cnt0: output logic<8>,
            cnt1: output logic<8>
        ) {
            var r_cnt0: logic<8>;
            var r_cnt1: logic<8>;
            always_ff (clk0) {
                r_cnt0 = r_cnt0 + 8'd1;
            }
            always_ff (clk1) {
                r_cnt1 = r_cnt1 + 8'd1;
            }
            assign cnt0 = r_cnt0;
            assign cnt1 = r_cnt1;
        }
    "#;

    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    vsim.add_clock("clk0", 10, 5); // 5, 15, 25, 35, 45...
    vsim.add_clock("clk1", 20, 5); // 5, 25, 45...

    let cnt0 = vsim.signal("cnt0");
    let cnt1 = vsim.signal("cnt1");

    vsim.run_until(10).unwrap(); // After t=5 (both tick)
    assert_eq!(vsim.get(cnt0), 1u8.into(), "t=10, cnt0");
    assert_eq!(vsim.get(cnt1), 1u8.into(), "t=10, cnt1");

    vsim.run_until(20).unwrap(); // After t=15 (clk0 ticks)
    assert_eq!(vsim.get(cnt0), 2u8.into(), "t=20, cnt0");
    assert_eq!(vsim.get(cnt1), 1u8.into(), "t=20, cnt1");

    vsim.run_until(30).unwrap(); // After t=25 (both tick)
    assert_eq!(vsim.get(cnt0), 3u8.into(), "t=30, cnt0");
    assert_eq!(vsim.get(cnt1), 2u8.into(), "t=30, cnt1");

    vsim.run_until(50).unwrap();
    assert_eq!(vsim.get(cnt0), 5u8.into(), "t=50, cnt0");
    assert_eq!(vsim.get(cnt1), 3u8.into(), "t=50, cnt1");
}

/// [CAN DO] Regular Synchronous Reset
#[test]
fn test_regular_reset() {
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            cnt: output logic<8>
        ) {
            var r_cnt: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    r_cnt = 8'd0;
                } else {
                    r_cnt = r_cnt + 8'd1;
                }
            }
            assign cnt = r_cnt;
        }
    "#;

    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    vsim.add_clock("clk", 10, 5);

    let rst = vsim.signal("rst");
    vsim.modify(|io| {
        io.set(rst, 1u8);
    })
    .unwrap();
    vsim.run_until(7).unwrap(); // after first posedge
    let cnt = vsim.signal("cnt");
    assert_eq!(vsim.get(cnt), 0u8.into());
    vsim.modify(|io| {
        io.set(rst, 0u8);
    })
    .unwrap();
    vsim.run_until(10).unwrap();

    vsim.run_until(15).unwrap();
    assert_eq!(vsim.get(cnt), 1u8.into());

    vsim.modify(|io| {
        io.set(rst, 1u8);
    })
    .unwrap();
    // rst is high but clk hasn't ticked yet, so cnt should still be 1
    assert_eq!(vsim.get(cnt), 1u8.into());

    vsim.run_until(25).unwrap(); // after second posedge
    assert_eq!(vsim.get(cnt), 0u8.into());
    vsim.run_until(35).unwrap();
    vsim.modify(|io| {
        io.set(rst, 0u8);
    })
    .unwrap();
    vsim.run_until(45).unwrap(); // after third posedge
    assert_eq!(vsim.get(cnt), 1u8.into());
}
