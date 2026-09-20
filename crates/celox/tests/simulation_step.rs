use celox::{RuntimeErrorCode, Simulation};

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

#[test]
fn test_scheduled_clock_event_remains_one_shot() {
    let code = r#"
        module Top (
            clk: input clock
        ) {
            always_ff (clk) {}
        }
    "#;
    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    vsim.add_clock("clk", 10, 0);
    vsim.schedule("clk", 2, 0).unwrap();

    let event_times: Vec<_> = (0..4).map(|_| vsim.step().unwrap().unwrap()).collect();

    assert_eq!(event_times, vec![0, 2, 5, 10]);
    assert_eq!(vsim.next_event_time(), Some(15));
}

#[test]
fn test_scheduled_clock_event_does_not_duplicate_a_periodic_edge() {
    let code = r#"
        module Top (
            clk: input clock
        ) {
            always_ff (clk) {}
        }
    "#;
    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    let clk = vsim.signal("clk");
    vsim.add_clock("clk", 10, 0);
    vsim.schedule("clk", 0, 0).unwrap();

    assert_eq!(vsim.step().unwrap(), Some(0));
    assert_eq!(vsim.get(clk), 1u8.into());

    assert_eq!(vsim.step().unwrap(), Some(5));
    assert_eq!(vsim.get(clk), 0u8.into());
    assert_eq!(vsim.next_event_time(), Some(10));
}

#[test]
fn test_step_decorates_runtime_errors() {
    let code = r#"
        module Top (
            clk: input clock,
            start: input logic<8>,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..count step *= 2 {
                    q = i as 8;
                }
            }
        }
    "#;

    let mut vsim = Simulation::builder(code, "Top").build().unwrap();
    let start = vsim.signal("start");
    let count = vsim.signal("count");

    vsim.modify(|io| {
        io.set(start, 0u8);
        io.set(count, 4u8);
    })
    .unwrap();
    vsim.add_clock("clk", 10, 0);

    assert_eq!(
        vsim.step().unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_simulation_by_id_methods_validate_before_enqueuing() {
    let code = r#"
        module Top (
            clk: input clock,
            q: output logic
        ) {
            always_ff (clk) {
                q = ~q;
            }
        }
    "#;
    let mut sim = Simulation::builder(code, "Top").build().unwrap();
    let events = sim.named_events();
    assert_eq!(events.len(), 1);
    let event_id = events[0].id as u32;
    assert_eq!(event_id, 0);
    let first_unknown_id = events.len() as u32;

    assert_eq!(sim.next_event_time(), None);
    assert_eq!(
        sim.schedule_by_id(first_unknown_id, 5, 1),
        Err(RuntimeErrorCode::NotAnEvent(format!(
            "event_id={first_unknown_id}"
        )))
    );
    assert_eq!(
        sim.schedule_by_id(u32::MAX, 5, 1),
        Err(RuntimeErrorCode::NotAnEvent(format!(
            "event_id={}",
            u32::MAX
        )))
    );
    assert_eq!(
        sim.try_add_clock_by_id(first_unknown_id, 10, 0),
        Err(RuntimeErrorCode::NotAnEvent(format!(
            "event_id={first_unknown_id}"
        )))
    );
    assert_eq!(
        sim.try_add_clock_by_id(u32::MAX, 10, 0),
        Err(RuntimeErrorCode::NotAnEvent(format!(
            "event_id={}",
            u32::MAX
        )))
    );

    sim.add_clock_by_id(first_unknown_id, 10, 0);
    sim.add_clock_by_id(u32::MAX, 10, 0);
    assert_eq!(sim.next_event_time(), None);

    sim.try_add_clock_by_id(event_id, 10, 30).unwrap();
    assert_eq!(sim.next_event_time(), Some(30));
    sim.add_clock_by_id(event_id, 10, 20);
    assert_eq!(sim.next_event_time(), Some(20));
    sim.schedule_by_id(event_id, 5, 1).unwrap();
    assert_eq!(sim.next_event_time(), Some(5));
}

#[test]
fn test_simulation_by_id_methods_reject_ids_when_no_events_exist() {
    let code = r#"
        module Top (
            q: output logic
        ) {
            assign q = 0;
        }
    "#;
    let mut sim = Simulation::builder(code, "Top").build().unwrap();
    assert!(sim.named_events().is_empty());

    assert_eq!(
        sim.schedule_by_id(0, 5, 1),
        Err(RuntimeErrorCode::NotAnEvent("event_id=0".to_string()))
    );
    assert_eq!(
        sim.try_add_clock_by_id(0, 10, 0),
        Err(RuntimeErrorCode::NotAnEvent("event_id=0".to_string()))
    );
    sim.add_clock_by_id(0, 10, 0);
    assert_eq!(sim.next_event_time(), None);
}
