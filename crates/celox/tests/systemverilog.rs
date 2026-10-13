use std::path::Path;

use celox::Simulator;
use num_bigint::BigUint;

// Each entry is either a suite case, `@case "group::name";`, or a test written
// against the Celox API with `@setup` and `@build`. Either runs on every backend.
macro_rules! sv_backends {
    () => {};
    (
        fn $name:ident($sim:ident) { @case $case:literal; }
        $($rest:tt)*
    ) => {
        mod $name {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            fn native() {
                crate::suite::run_case($case, "native");
            }

            #[test]
            fn cranelift() {
                crate::suite::run_case($case, "cranelift");
            }

            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            fn native_parallel() {
                crate::suite::run_case($case, "native-parallel");
            }

            #[test]
            fn cranelift_parallel() {
                crate::suite::run_case($case, "cranelift-parallel");
            }

            #[test]
            fn wasm() {
                crate::suite::run_case($case, "wasm");
            }
        }
        sv_backends! { $($rest)* }
    };
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
#[path = "frontends/systemverilog/generate.rs"]
mod generate;
#[path = "frontends/systemverilog/hierarchy.rs"]
mod hierarchy;
#[path = "frontends/systemverilog/indexed_select.rs"]
mod indexed_select;
#[path = "frontends/systemverilog/interfaces.rs"]
mod interfaces;
#[path = "frontends/systemverilog/literals.rs"]
mod literals;
#[path = "frontends/systemverilog/mixed.rs"]
mod mixed;
#[path = "frontends/systemverilog/operators.rs"]
mod operators;
#[path = "frontends/systemverilog/packed_structs.rs"]
mod packed_structs;
#[path = "frontends/systemverilog/procedural.rs"]
mod procedural;
#[path = "frontends/systemverilog/processes.rs"]
mod processes;
#[path = "frontends/systemverilog/review_regressions.rs"]
mod review_regressions;
#[path = "frontends/systemverilog/suite.rs"]
mod suite;
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

sv_backends! {
 fn multiple_import_items_preserve_untimed_edges(sim) { @case "timing::multiple_import_items_preserve_untimed_edges"; }
 fn escaped_timed_tasks_do_not_replay_setup_edges(sim) { @case "timing::escaped_timed_tasks_do_not_replay_setup_edges"; }
 fn control_names_do_not_rewrite_the_dut_type(sim) { @case "timing::control_names_do_not_rewrite_the_dut_type"; }
}

sv_backends! {
 fn attributes_preserve_single_generate_arms(sim) { @case "timing::attributes_preserve_single_generate_arms"; }
 fn adapter_defines_preserve_line_macros(sim) { @case "timing::adapter_defines_preserve_line_macros"; }
 fn callable_timing_keeps_generate_scope(sim) { @case "timing::callable_timing_keeps_generate_scope"; }
}

sv_backends! {
 fn timed_file_macros_keep_each_source_path(sim) { @case "timing::timed_file_macros_keep_each_source_path"; }
 fn escaped_identifiers_do_not_collide_with_control_names(sim) { @case "timing::escaped_identifiers_do_not_collide_with_control_names"; }
}
