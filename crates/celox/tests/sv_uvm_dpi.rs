#![cfg(feature = "systemverilog")]
//! The functions of UVM's C library that Celox provides itself.
use std::path::Path;

const SOURCE: &str = r#"
package dpi;
  import "DPI-C" function chandle uvm_re_comp(string re, bit deglob);
  import "DPI-C" function int uvm_re_exec(chandle rexp, string str);
  import "DPI-C" function void uvm_re_free(chandle rexp);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function bit uvm_re_compexecfree(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function string uvm_re_buffer();
  import "DPI-C" function string uvm_re_deglobbed(string glob, bit with_brackets);
  import "DPI-C" function string uvm_dpi_get_next_arg_c (int init);
  import "DPI-C" function string uvm_dpi_get_tool_name_c ();
  import "DPI-C" function string uvm_dpi_get_tool_version_c ();
  typedef logic [1023:0] uvm_hdl_data_t;
  import "DPI-C" context function int uvm_hdl_read(string path, output uvm_hdl_data_t value);
  import "DPI-C" context function chandle uvm_polling_create(input string name, input int sv_key);

  function automatic bit glob_hit(string re, string str);
    chandle rexp = uvm_re_comp(re, 1);
    bit hit;
    if (rexp == null) return 0;
    hit = uvm_re_exec(rexp, str) == 0;
    uvm_re_free(rexp);
    return hit;
  endfunction
endpackage

module Top(output int checks, output int failed);
  import dpi::*;
  int selections;
  function automatic int next_slot();
    selections++;
    return 1;
  endfunction
  initial begin
    automatic chandle rexp;
    automatic int ret = 7;
    automatic string args[$];
    automatic string arg;
    automatic uvm_hdl_data_t data = '0;
    automatic uvm_hdl_data_t slots[2] = '{'0, '0};
    checks = 0; failed = 0;
    if (glob_hit("uvm_test_top.*", "uvm_test_top.env.agent")) checks++; else failed |= 1;
    if (!glob_hit("uvm_test_top.*", "top.env")) checks++; else failed |= 2;
    if (glob_hit("/^a[0-9]+b$/", "a42b") && !glob_hit("/^a[0-9]+b$/", "ab")) checks++; else failed |= 4;
    if (uvm_re_deglobbed("top.u?.*", 0) == "^top\\.u.\\..*$") checks++; else failed |= 8;
    rexp = uvm_re_compexec("env*", "env_1", 1, ret);
    if (rexp != null && ret == 0) checks++; else failed |= 16;
    uvm_re_free(rexp);
    if (uvm_re_compexecfree("x", "y", 0, ret) && ret == 1) checks++; else failed |= 32;
    if (uvm_re_comp("(", 0) == null && uvm_re_buffer() != "") checks++; else failed |= 64;
    arg = uvm_dpi_get_next_arg_c(1);
    while (arg != "") begin args.push_back(arg); arg = uvm_dpi_get_next_arg_c(0); end
    if (args.size() == 3 && args[1] == "+UVM_TESTNAME=smoke" && args[2] == "+UVM_VERBOSITY=LOW") checks++; else failed |= 128;
    if (uvm_dpi_get_tool_name_c() == "Celox" && uvm_dpi_get_tool_version_c() != "") checks++; else failed |= 256;
    // Not provided yet: failure, outputs unchanged.
    if (uvm_hdl_read("Top.checks", data) == 0 && data == '0 && uvm_polling_create("x", 1) == null) checks++; else failed |= 512;
    // The selector of the output is still evaluated.
    if (uvm_hdl_read("Top.checks", slots[next_slot()]) == 0 && selections == 1 && slots[1] == '0) checks++; else failed |= 1024;
  end
endmodule
"#;

macro_rules! run_uvm_dpi {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            for four_state in [false, true] {
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(SOURCE, Path::new("uvm_dpi.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .plusargs(["+UVM_TESTNAME=smoke", "UVM_VERBOSITY=LOW"])
                .$backend()
                .unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(
                    sim.get(sim.signal("failed")),
                    0u8.into(),
                    "four_state={four_state}"
                );
                assert_eq!(
                    sim.get(sim.signal("checks")),
                    11u8.into(),
                    "four_state={four_state}"
                );
            }
        }
    };
}
run_uvm_dpi!(uvm_dpi_interpreter, build_interpreter);
run_uvm_dpi!(uvm_dpi_cranelift, build);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
run_uvm_dpi!(uvm_dpi_native, build_native);
run_uvm_dpi!(uvm_dpi_wasm, build_wasm);

/// The traced builder seeds the arguments too.
#[test]
fn uvm_dpi_traced_builder_sees_arguments() {
    let source = r#"
module Top(output bit ok);
  import "DPI-C" function string uvm_dpi_get_next_arg_c (int init);
  function automatic bit second_is_test();
    string arg = uvm_dpi_get_next_arg_c(1);
    arg = uvm_dpi_get_next_arg_c(0);
    return arg == "+UVM_TESTNAME=smoke";
  endfunction
  assign ok = second_is_test();
endmodule
"#;
    let mut sim = celox::Simulator::from_sv_sources(vec![(source, Path::new("args.sv"))], "Top")
        .plusargs(["+UVM_TESTNAME=smoke"])
        .build_with_trace()
        .res
        .unwrap();
    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
}

const CLOCKED: &str = r#"
`timescale 1ns/1ns
module Top(output int ret_seen, output bit skipped, output bit indexed, output bit hit, output bit freed);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function bit uvm_re_compexecfree(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function string uvm_dpi_get_next_arg_c (int init);
  bit clk = 0;
  int ret = 7;
  chandle rexp;
  always #1 clk = ~clk;
  // The output is written before the block reads it.
  int free_ret;
  always_ff @(posedge clk) begin
    rexp = uvm_re_compexec("a*", "aa", 1, ret);
    ret_seen <= ret;
    hit <= rexp != null;
    freed <= uvm_re_compexecfree("a*", "aa", 1, free_ret) && free_ret == 0;
  end
  // A process whose only effect is the call still runs.
  always_ff @(posedge clk) void'(uvm_dpi_get_next_arg_c(0));
  initial begin
    void'(uvm_dpi_get_next_arg_c(1));
    #2 skipped = uvm_dpi_get_next_arg_c(0) == "+B";
  end
  // The output's selector makes a host request after the result exists.
  int status[2] = '{7, 7};
  initial begin
    automatic chandle r = uvm_re_compexec("a*", "aa", 1, status[$test$plusargs("B")]);
    indexed = r != null && status[1] == 0 && status[0] == 7;
  end
endmodule
"#;

macro_rules! run_clocked {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            for four_state in [false, true] {
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(CLOCKED, Path::new("clocked.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .plusargs(["+A", "+B"])
                .$backend()
                .unwrap();
                sim.run_until(3).unwrap();
                assert_eq!(
                    sim.get(sim.signal("ret_seen")),
                    0u32.into(),
                    "four_state={four_state}"
                );
                assert_eq!(
                    sim.get(sim.signal("skipped")),
                    1u8.into(),
                    "four_state={four_state}"
                );
                assert_eq!(
                    sim.get(sim.signal("indexed")),
                    1u8.into(),
                    "four_state={four_state}"
                );
                // The results of clocked calls, not only their outputs.
                assert_eq!(
                    sim.get(sim.signal("hit")),
                    1u8.into(),
                    "four_state={four_state}"
                );
                assert_eq!(
                    sim.get(sim.signal("freed")),
                    1u8.into(),
                    "four_state={four_state}"
                );
            }
        }
    };
}
run_clocked!(clocked_interpreter, build_interpreter);
run_clocked!(clocked_cranelift, build);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
run_clocked!(clocked_native, build_native);
run_clocked!(clocked_wasm, build_wasm);

/// A result outlives the publication of the output, which may suspend a
/// forked process waiting on a compound event.
#[test]
fn uvm_dpi_results_survive_output_publication() {
    let source = r#"
`timescale 1ns/1ns
module Top(output bit ok, output bit freed);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function bit uvm_re_compexecfree(string re, string str, bit deglob, output int exec_ret);
  int value, ret;
  initial begin
    automatic chandle rexp;
    fork #1 value = 1; join_none
    rexp = uvm_re_compexec("env*", "env_1", 1, ret);
    ok = rexp != null && ret == 0;
    freed = uvm_re_compexecfree("x", "y", 0, ret) && ret == 1;
    void'(uvm_re_compexec("env*", "env_2", 1, ret));
    uvm_re_compexecfree("a", "b", 0, ret);
    @(value == 1);
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("publish.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(
            sim.get(sim.signal("ok")),
            1u8.into(),
            "four_state={four_state}"
        );
        assert_eq!(
            sim.get(sim.signal("freed")),
            1u8.into(),
            "four_state={four_state}"
        );
    }
}

/// An output of a builtin forwarded from a reference formal bound to a class
/// property is written to the property (IEEE 1800-2023 13.5.2).
#[test]
fn uvm_dpi_outputs_reach_forwarded_class_properties() {
    let source = r#"
module Top(output bit ok);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  import "DPI-C" function bit uvm_re_compexecfree(string re, string str, bit deglob, output int exec_ret);
  class Holder;
    int status = 7;
    int freed = 7;
  endclass
  function automatic chandle match_into(ref int status);
    return uvm_re_compexec("env*", "env_1", 1, status);
  endfunction
  function automatic bit miss_into(ref int status);
    return uvm_re_compexecfree("x", "y", 0, status);
  endfunction
  initial begin
    automatic Holder h = new;
    automatic chandle rexp = match_into(h.status);
    ok = rexp != null && h.status == 0 && miss_into(h.freed) && h.freed == 1;
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("forward.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(
            sim.get(sim.signal("ok")),
            1u8.into(),
            "four_state={four_state}"
        );
    }
}

/// An output that selects an inactive tagged union member is an invalid
/// access, as for any output actual (IEEE 1800-2023 7.3.2).
#[test]
fn uvm_dpi_outputs_check_tagged_members() {
    let source = r#"
module Top(output int y);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  union tagged packed { void Empty; logic [31:0] Full; } status;
  initial begin
    status = tagged Empty;
    void'(uvm_re_compexec("a", "a", 0, status.Full));
    y = 0;
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("tagged.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        let error = sim.step().unwrap_err();
        assert!(
            error.to_string().contains("tagged union member"),
            "four_state={four_state}: {error}"
        );
    }
}

/// A variable that processes of one edge share through a builtin's output
/// is not overwritten by a process that leaves it unwritten.
#[test]
fn uvm_dpi_outputs_are_shared_edge_writes() {
    let source = r#"
module Top(input bit clk, output int seen);
  import "DPI-C" function chandle uvm_re_compexec(string re, string str, bit deglob, output int exec_ret);
  bit set = 0;
  int ret = 7;
  always @(posedge clk) void'(uvm_re_compexec("a", "a", 0, ret));
  always @(posedge clk) if (set) ret = 5;
  assign seen = ret;
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulator::from_sv_sources(vec![(source, Path::new("shared.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get(sim.signal("seen")),
            0u32.into(),
            "four_state={four_state}"
        );
    }
}
