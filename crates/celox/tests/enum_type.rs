use celox::{Simulator, SimulatorBuilder};

#[test]
fn test_enum_case_match() {
    let code = r#"
        module Top (
            sel: input logic<2>,
            o:   output logic<8>
        ) {
            enum State: logic<2> {
                Idle = 2'd0,
                Run  = 2'd1,
                Done = 2'd2,
            }
            always_comb {
                case sel {
                    State::Idle: o = 8'h00;
                    State::Run:  o = 8'hAA;
                    State::Done: o = 8'hFF;
                    default:     o = 8'h01;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0x00u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());

    sim.modify(|io| io.set(sel, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xFFu8.into());

    sim.modify(|io| io.set(sel, 3u8)).unwrap();
    assert_eq!(sim.get(o), 0x01u8.into());
}

#[test]
fn test_enum_ff_state_machine() {
    let code = r#"
        module Top (
            clk:   input clock,
            rst:   input reset,
            start: input logic,
            state_out: output logic<2>
        ) {
            enum State: logic<2> {
                Idle = 2'd0,
                Run  = 2'd1,
                Done = 2'd2,
            }
            var state: State;
            always_ff (clk, rst) {
                if_reset {
                    state = State::Idle;
                } else {
                    case state {
                        State::Idle: {
                            if start {
                                state = State::Run;
                            }
                        }
                        State::Run: {
                            state = State::Done;
                        }
                        State::Done: {
                            state = State::Idle;
                        }
                        default: {
                            state = State::Idle;
                        }
                    }
                }
            }
            assign state_out = state;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let start = sim.signal("start");
    let state_out = sim.signal("state_out");

    // Reset (AsyncLow: rst=0 means active)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(start, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state_out), 0u8.into()); // Idle

    // Release reset, start = 0 -> stay Idle
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state_out), 0u8.into()); // Idle

    // start = 1 -> go to Run
    sim.modify(|io| io.set(start, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state_out), 1u8.into()); // Run

    // Run -> Done
    sim.modify(|io| io.set(start, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state_out), 2u8.into()); // Done

    // Done -> Idle
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state_out), 0u8.into()); // Idle
}

/// Enum-typed variables can be assigned from logic inputs
/// and compared against enum members.
#[test]
fn test_enum_assign_and_compare() {
    let code = r#"
        module Top (
            i: input logic<2>,
            o_is_b: output logic
        ) {
            enum Color: logic<2> {
                Red   = 2'd0,
                Green = 2'd1,
                Blue  = 2'd2,
            }
            var c: Color;
            assign c = i;
            assign o_is_b = c == Color::Blue;
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_sim_modules()
        .build_with_trace();
    let trace = result.trace;
    println!("{}", trace.format_slt().unwrap());

    let mut sim = result.res.unwrap();
    let i = sim.signal("i");
    let o_is_b = sim.signal("o_is_b");

    sim.modify(|io| io.set(i, 0u8)).unwrap();
    assert_eq!(sim.get(o_is_b), 0u8.into());

    sim.modify(|io| io.set(i, 2u8)).unwrap();
    assert_eq!(sim.get(o_is_b), 1u8.into());
}



