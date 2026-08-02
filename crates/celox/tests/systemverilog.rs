use std::path::Path;

use celox::Simulator;
use num_bigint::BigUint;

macro_rules! sv_backends {
    ($(
        fn $name:ident($sim:ident) {
            @setup { $($setup:tt)* }
            @build $builder:expr;
            $($body:tt)*
        }
    )*) => {
        $(
            mod $name {
                use super::*;

                #[test]
                #[allow(unused_mut, unused_variables)]
                fn native() {
                    $($setup)*
                    let mut $sim = { $builder }.build_native().unwrap();
                    $($body)*
                }

                #[test]
                #[allow(unused_mut, unused_variables)]
                fn cranelift() {
                    $($setup)*
                    let mut $sim = { $builder }.build_cranelift().unwrap();
                    $($body)*
                }

                #[test]
                #[allow(unused_mut, unused_variables)]
                fn wasm() {
                    $($setup)*
                    let mut $sim = { $builder }.build_wasm().unwrap();
                    $($body)*
                }
            }
        )*
    };
}

#[path = "frontends/systemverilog/always_comb.rs"]
mod always_comb;
#[path = "frontends/systemverilog/hierarchy.rs"]
mod hierarchy;
#[path = "frontends/systemverilog/literals.rs"]
mod literals;
#[path = "frontends/systemverilog/mixed.rs"]
mod mixed;
#[path = "frontends/systemverilog/operators.rs"]
mod operators;
#[path = "frontends/systemverilog/types.rs"]
mod types;

#[test]
fn simulates_systemverilog_always_comb() {
    let source = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic [7:0] y
        );
            always_comb y = a ^ b;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("always_comb.sv"))], "Top")
        .build_cranelift()
        .unwrap();

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 0x55u8);
        io.set(b, 0x0fu8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0x5au8.into());
}

#[test]
fn simulates_systemverilog_hierarchy() {
    let source = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule

        module Top(input logic [7:0] lhs, input logic [7:0] rhs, output logic [7:0] out);
            Xor8 u_xor(.a(lhs), .b(rhs), .y(out));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("hierarchy.sv"))], "Top")
        .build_cranelift()
        .unwrap();

    let lhs = sim.signal("lhs");
    let rhs = sim.signal("rhs");
    let out = sim.signal("out");
    sim.modify(|io| {
        io.set(lhs, 0xa5u8);
        io.set(rhs, 0x3cu8);
    })
    .unwrap();
    assert_eq!(sim.get(out), 0x99u8.into());
}

#[test]
fn simulates_systemverilog_always_ff() {
    let source = r#"
        module Top(input logic clk, input logic rst_n, input logic d, output logic q);
            always_ff @(posedge clk, negedge rst_n) begin
                if (!rst_n) q <= 1'b0;
                else q <= d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();

    let clk = sim.event("clk");
    let rst_n = sim.signal("rst_n");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(rst_n, 0u8);
        io.set(d, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(rst_n, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
}
