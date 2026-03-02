use celox::Simulator;

/// Basic param override: change WIDTH from default 8 to 16 and verify signal width.
#[test]
fn test_param_override_basic_width() {
    let code = r#"
        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            assign b = a;
        }
    "#;

    // With default WIDTH=8, signals are 8-bit.
    let mut sim_default = Simulator::builder(code, "Top").build().unwrap();
    let a = sim_default.signal("a");
    let b = sim_default.signal("b");
    sim_default.modify(|io| io.set(a, 0xABu8)).unwrap();
    assert_eq!(sim_default.get(b), 0xABu8.into());

    // Override WIDTH=16 → signals are 16-bit, can hold larger values.
    let mut sim_wide = Simulator::builder(code, "Top")
        .param("WIDTH", 16)
        .build()
        .unwrap();
    let a = sim_wide.signal("a");
    let b = sim_wide.signal("b");
    sim_wide.modify(|io| io.set(a, 0xABCDu16)).unwrap();
    assert_eq!(sim_wide.get(b), 0xABCDu16.into());
}

/// Param value reflected in logic (assign b = a + OFFSET).
#[test]
fn test_param_override_logic_reflection() {
    let code = r#"
        module Top #(
            param OFFSET: u32 = 10,
        )(
            a: input  logic<32>,
            b: output logic<32>,
        ) {
            assign b = a + OFFSET;
        }
    "#;

    // Default OFFSET=10
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 5u32)).unwrap();
    assert_eq!(sim.get(b), 15u32.into());

    // Override OFFSET=100
    let mut sim = Simulator::builder(code, "Top")
        .param("OFFSET", 100)
        .build()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 5u32)).unwrap();
    assert_eq!(sim.get(b), 105u32.into());
}

/// No override → default value is used.
#[test]
fn test_param_override_default_value() {
    let code = r#"
        module Top #(
            param INIT: u32 = 42,
        )(
            o: output logic<32>,
        ) {
            assign o = INIT;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 42u32.into());
}

/// Multiple params overridden simultaneously.
#[test]
fn test_param_override_multiple() {
    let code = r#"
        module Top #(
            param A: u32 = 1,
            param B: u32 = 2,
        )(
            o: output logic<32>,
        ) {
            assign o = A + B;
        }
    "#;

    // Default: 1 + 2 = 3
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 3u32.into());

    // Override both: 10 + 20 = 30
    let mut sim = Simulator::builder(code, "Top")
        .param("A", 10)
        .param("B", 20)
        .build()
        .unwrap();
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 30u32.into());
}

/// Param propagation to a child module via inst param override.
#[test]
fn test_param_override_child_propagation() {
    let code = r#"
        module Child #(
            param WIDTH: u32 = 8,
        )(
            i_data: input  logic<WIDTH>,
            o_data: output logic<WIDTH>,
        ) {
            assign o_data = i_data;
        }

        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            inst u_child: Child #(WIDTH: WIDTH) (
                i_data: a,
                o_data: b,
            );
        }
    "#;

    // Default WIDTH=8: signals are 8-bit
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABu8)).unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());

    // Override WIDTH=16 → child also gets 16-bit ports
    let mut sim = Simulator::builder(code, "Top")
        .param("WIDTH", 16)
        .build()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABCDu16)).unwrap();
    assert_eq!(sim.get(b), 0xABCDu16.into());
}
