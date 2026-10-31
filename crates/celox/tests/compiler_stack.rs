//! Compiler traversals must not consume one host stack frame per decoder arm.
use celox::Simulator;

#[test]
fn deep_decoder_with_function_calls_compiles_on_default_stack() {
    let arms = (0..512)
        .map(|i| format!("10'd{i}: q = identity(a) ^ 8'd{};", i % 256))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        "module Top(clk: input clock, sel: input logic<10>, a: input logic<8>, q: output logic<8>) {{
            function identity(v: input logic<8>) -> logic<8> {{ return v; }}
            always_ff {{ case sel {{ {arms} default: q = a; }} }}
        }}"
    );
    let mut sim = Simulator::builder(&source, "Top").build_native().unwrap();
    let clk = sim.event("clk");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let q = sim.signal("q");
    for value in [0u16, 255, 511, 512, 1023] {
        sim.modify(|io| {
            io.set(sel, value);
            io.set(a, 0xa5u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        let expected = 0xa5 ^ if value < 512 { value as u8 } else { 0 };
        assert_eq!(sim.get(q), expected.into());
    }
}

#[test]
fn deep_pure_ternary_with_function_calls_compiles_on_default_stack() {
    let mut expression = "a".to_owned();
    for index in (0..192).rev() {
        expression = format!("if sel == 8'd{index} ? identity(a) ^ 8'd{index} : ({expression})");
    }
    let source = format!(
        "module Top(clk: input clock, sel: input logic<8>, a: input logic<8>, q: output logic<8>) {{
            function identity(v: input logic<8>) -> logic<8> {{ return v; }}
            always_ff {{ q = {expression}; }}
        }}"
    );
    let mut sim = Simulator::builder(&source, "Top").build_native().unwrap();
    let clk = sim.event("clk");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let q = sim.signal("q");
    for value in [0u8, 95, 191, 192, 255] {
        sim.modify(|io| {
            io.set(sel, value);
            io.set(a, 0xa5u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        let expected = 0xa5 ^ if value < 192 { value } else { 0 };
        assert_eq!(sim.get(q), expected.into());
    }
}
