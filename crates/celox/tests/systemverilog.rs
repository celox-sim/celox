use std::path::Path;

use celox::Simulator;
use num_bigint::BigUint;

// Each entry is a test written against the Celox API with `@setup` and
// `@build`; it runs on every backend. scripts/run-vtest.py runs the suite
// cases.
macro_rules! sv_backends {
    () => {};
    (
        $(#[$attr:meta])*
        fn $name:ident($sim:ident) {
            @setup { $($setup:tt)* }
            @build $builder:expr;
            $($body:tt)*
        }
        $($rest:tt)*
    ) => {
        mod $name {
            use super::*;

            #[test]
            $(#[$attr])*
            #[allow(unused_mut, unused_variables)]
            fn native() {
                $($setup)*
                let mut $sim = { $builder }.build_native().unwrap();
                $($body)*
            }

            #[test]
            $(#[$attr])*
            #[allow(unused_mut, unused_variables)]
            fn cranelift() {
                $($setup)*
                let mut $sim = { $builder }.build_cranelift().unwrap();
                $($body)*
            }

            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            $(#[$attr])*
            #[allow(unused_mut, unused_variables)]
            fn native_parallel() {
                $($setup)*
                let mut $sim = { $builder }
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()
                    .unwrap();
                $($body)*
            }

            #[test]
            $(#[$attr])*
            #[allow(unused_mut, unused_variables)]
            fn wasm() {
                $($setup)*
                let mut $sim = { $builder }.build_wasm().unwrap();
                $($body)*
            }
        }
        sv_backends! { $($rest)* }
    };
}

#[path = "frontends/systemverilog/always_comb.rs"]
mod always_comb;
#[path = "frontends/systemverilog/array_locators.rs"]
mod array_locators;
#[path = "frontends/systemverilog/array_parameters.rs"]
mod array_parameters;
#[path = "frontends/systemverilog/assertion_control.rs"]
mod assertion_control;
#[path = "frontends/systemverilog/binary_file_io.rs"]
mod binary_file_io;
#[path = "frontends/systemverilog/class.rs"]
mod class;
#[path = "frontends/systemverilog/corpus.rs"]
mod corpus;
#[path = "frontends/systemverilog/file_core.rs"]
mod file_core;
#[path = "frontends/systemverilog/file_io.rs"]
mod file_io;
#[path = "frontends/systemverilog/file_lines.rs"]
mod file_lines;
#[path = "frontends/systemverilog/final.rs"]
mod final_procedures;
#[path = "frontends/systemverilog/generate.rs"]
mod generate;
#[path = "frontends/systemverilog/hierarchical_calls.rs"]
mod hierarchical_calls;
#[path = "frontends/systemverilog/hierarchical_references.rs"]
mod hierarchical_references;
#[path = "frontends/systemverilog/hierarchy.rs"]
mod hierarchy;
#[path = "frontends/systemverilog/indexed_select.rs"]
mod indexed_select;
#[path = "frontends/systemverilog/interfaces.rs"]
mod interfaces;
#[path = "frontends/systemverilog/latch.rs"]
mod latch;
#[path = "frontends/systemverilog/literals.rs"]
mod literals;
#[path = "frontends/systemverilog/mixed.rs"]
mod mixed;
#[path = "frontends/systemverilog/operators.rs"]
mod operators;
#[path = "frontends/systemverilog/out_of_line_calls.rs"]
mod out_of_line_calls;
#[path = "frontends/systemverilog/packed_structs.rs"]
mod packed_structs;
#[path = "frontends/systemverilog/procedural.rs"]
mod procedural;
#[path = "frontends/systemverilog/procedural_assigns.rs"]
mod procedural_assigns;
#[path = "frontends/systemverilog/realtime.rs"]
mod realtime;
#[path = "frontends/systemverilog/review_regressions.rs"]
mod review_regressions;
#[path = "frontends/systemverilog/scan.rs"]
mod scan;
#[path = "frontends/systemverilog/streaming.rs"]
mod streaming;
#[path = "frontends/systemverilog/string.rs"]
mod string;
#[path = "frontends/systemverilog/string_format.rs"]
mod string_format;
#[path = "frontends/systemverilog/synthesizable.rs"]
mod synthesizable;
#[path = "frontends/systemverilog/system_functions.rs"]
mod system_functions;
#[path = "frontends/systemverilog/timing.rs"]
mod timing;
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

#[path = "frontends/systemverilog/floating_point.rs"]
mod floating_point;

#[path = "frontends/systemverilog/tagged_unions.rs"]
mod tagged_unions;

#[path = "frontends/systemverilog/references.rs"]
mod references;

#[path = "frontends/systemverilog/virtual_interfaces.rs"]
mod virtual_interfaces;

#[path = "frontends/systemverilog/wide_container_elements.rs"]
mod wide_container_elements;

#[path = "frontends/systemverilog/container_formals.rs"]
mod container_formals;

#[path = "frontends/systemverilog/events.rs"]
mod events;

#[path = "frontends/systemverilog/semaphores.rs"]
mod semaphores;

#[path = "frontends/systemverilog/mailboxes.rs"]
mod mailboxes;

#[path = "frontends/systemverilog/containers.rs"]
mod containers;

#[path = "frontends/systemverilog/concurrent_assertions.rs"]
mod concurrent_assertions;
#[path = "frontends/systemverilog/unpacked_structs.rs"]
mod unpacked_structs;

#[path = "frontends/systemverilog/file_errors.rs"]
mod file_errors;

#[path = "frontends/systemverilog/strobe.rs"]
mod strobe;

#[path = "frontends/systemverilog/monitor.rs"]
mod monitor;
