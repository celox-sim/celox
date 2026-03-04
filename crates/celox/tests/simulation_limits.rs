use celox::Simulation;

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
            clk:   input '_ clock,
            clk_n: input '_ clock_negedge,
            rst_n: input '_ reset_async_low,
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

    vsim.add_clock("clk", 10, 5);
    vsim.add_clock("clk_n", 10, 5);

    // Initial state: rst_n = 0 (reset)
    vsim.schedule("rst_n", 0, 0).unwrap();
    vsim.schedule("rst_n", 12, 1).unwrap(); // release reset at 12

    let out_p = vsim.signal("out_p");
    let out_n = vsim.signal("out_n");

    vsim.run_until(50).unwrap();

    assert_eq!(vsim.get(out_p), 4u8.into());
    assert_eq!(vsim.get(out_n), 5u8.into());
}

/// [CAN DO] Multiple Independent Clocks
#[test]
fn test_multiple_clocks() {
    let code = r#"
        module Top (
            clk0: input '_ clock,
            clk1: input '_ clock,
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

    vsim.run_until(10).unwrap();
    assert_eq!(vsim.get(cnt0), 1u8.into(), "t=10, cnt0");
    assert_eq!(vsim.get(cnt1), 1u8.into(), "t=10, cnt1");

    vsim.run_until(20).unwrap();
    assert_eq!(vsim.get(cnt0), 2u8.into(), "t=20, cnt0");
    assert_eq!(vsim.get(cnt1), 1u8.into(), "t=20, cnt1");

    vsim.run_until(30).unwrap();
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
        io.set(rst, 0u8);
    })
    .unwrap();
    vsim.run_until(7).unwrap();
    let cnt = vsim.signal("cnt");
    assert_eq!(vsim.get(cnt), 0u8.into());
    vsim.modify(|io| {
        io.set(rst, 1u8);
    })
    .unwrap();
    vsim.run_until(10).unwrap();

    vsim.run_until(15).unwrap();
    assert_eq!(vsim.get(cnt), 1u8.into());

    vsim.modify(|io| {
        io.set(rst, 0u8);
    })
    .unwrap();
    assert_eq!(vsim.get(cnt), 1u8.into());

    vsim.run_until(25).unwrap();
    assert_eq!(vsim.get(cnt), 0u8.into());
    vsim.run_until(35).unwrap();
    vsim.modify(|io| {
        io.set(rst, 1u8);
    })
    .unwrap();
    vsim.run_until(45).unwrap();
    assert_eq!(vsim.get(cnt), 1u8.into());
}
