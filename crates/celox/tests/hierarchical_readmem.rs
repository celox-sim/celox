use celox::testbench::{compile_initial_testbench, run_compiled_testbench};
use celox::{BigUint, Simulator, TestResult};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn memory_file(contents: &str) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.hex");
    std::fs::write(&path, contents).unwrap();
    (directory, path.to_string_lossy().replace('\\', "\\\\"))
}

all_backends! {

fn hierarchical_readmemh_runs_in_statement_order_across_branches_and_clock_edges(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_first_directory, first) = memory_file("2a\n11\n");
        let (_second_directory, second) = memory_file("7e\n22\n");
        let code = format!(r#"
            module Memory (clk: input clock, word: output logic<8>, sampled: output logic<8>) {{
                #[allow(unassign_variable)]
                var mem: logic<8>[2];
                assign word = mem[0];
                always_ff (clk) {{ sampled = mem[0]; }}
            }}
            #[test(Top)]
            module Top {{
                inst clk: $tb::clock_gen;
                var word: logic<8>;
                var sampled: logic<8>;
                var load: logic;
                inst dut: Memory (clk, word, sampled);
                initial {{
                    $assert(word == 0);
                    load = 0;
                    if load {{ $readmemh("{second}", dut.mem); }}
                    $assert(word == 0);
                    for i in 0..2 {{
                        if i == 0 {{ $readmemh("{first}", dut.mem); }}
                        else {{ $readmemh("{second}", dut.mem); }}
                        clk.next();
                        if i == 0 {{ $assert(sampled == 8'h2a); }}
                        else {{ $assert(sampled == 8'h7e); }}
                    }}
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("word")), 0x7eu32.into());
}

fn hierarchical_readmemh_initializes_child_and_settles_comb(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_directory, path) = memory_file("1\n2\n3\n4\n");
        let code = format!(r#"
            module Memory (sum: output logic<32>) {{
                #[allow(unassign_variable)]
                var mem: logic<32>[4];
                assign sum = mem[0] + mem[1] + mem[2] + mem[3];
            }}
            #[test(Top)]
            module Top {{
                var sum: logic<32>;
                inst dut: Memory (sum);
                initial {{
                    $readmemh("{path}", dut.mem);
                    $assert(dut.mem[0] == 32'd1);
                    $assert(dut.mem[3] == 32'd4);
                    $assert(sum == 32'd10);
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("sum")), 10u32.into());
}

fn hierarchical_readmemh_resolves_nested_parameterized_memories(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let expected = (BigUint::from(1u8) << 64) | BigUint::from(0x1abu32);
        let (_directory, path) = memory_file(&format!("{expected:x}\n155\n"));
        let code = format!(r#"
            module Memory #(param W: u32 = 8)(word: output logic<W>) {{
                #[allow(unassign_variable)]
                var mem: logic<W>[2];
                assign word = mem[0];
            }}
            module Bank #(param W: u32 = 8)(word: output logic<W>) {{
                inst memory: Memory #(W: W)(word);
            }}
            #[test(Top)]
            module Top {{
                var narrow: logic<9>;
                var wide: logic<65>;
                inst narrow_bank: Bank #(W: 9)(word: narrow);
                inst wide_bank: Bank #(W: 65)(word: wide);
                initial {{
                    $readmemh("{path}", narrow_bank.memory.mem);
                    $readmemh("{path}", wide_bank.memory.mem);
                    $assert(narrow_bank.memory.mem[1] == 9'h155);
                    $assert(wide_bank.memory.mem[1] == 65'h155);
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("narrow")), 0x1abu32.into());
    assert_eq!(sim.get(sim.signal("wide")), expected);
}

fn hierarchical_readmemh_uses_each_calling_instance_and_preserves_write_order(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_first_directory, first) = memory_file("11\n22\n");
        let (_second_directory, second) = memory_file("@1\naa\n");
        let code = format!(r#"
            module Memory (words: output logic<16>) {{
                #[allow(unassign_variable)]
                var mem: logic<8>[2];
                assign words = {{mem[1], mem[0]}};
            }}
            #[test(Bank)]
            module Bank (words: output logic<16>) {{
                inst memory: Memory (words);
                initial {{
                    $readmemh("{first}", memory.mem);
                }}
            }}
            #[test(Top)]
            module Top {{
                var a: logic<16>;
                var b: logic<16>;
                inst first: Bank (words: a);
                inst second: Bank (words: b);
                initial {{
                    $readmemh("{second}", first.memory.mem);
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("a")), 0xaa11u32.into());
    assert_eq!(sim.get(sim.signal("b")), 0x2211u32.into());
}

fn hierarchical_readmemh_preserves_four_state_and_sparse_addresses(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_directory, path) = memory_file("/* skip first element */ @1\nx5\nz0\n");
        let code = format!(r#"
            module Memory (words: output logic<24>) {{
                #[allow(unassign_variable)]
                var mem: logic<8>[3];
                assign words = {{mem[2], mem[1], mem[0]}};
            }}
            #[test(Top)]
            module Top {{
                var words: logic<24>;
                inst dut: Memory (words);
                initial {{
                    $readmemh("{path}", dut.mem);
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top").four_state(true);
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    let (value, mask) = sim.get_four_state(sim.signal("words"));
    assert_eq!(mask, 0xf0f0ffu32.into());
    assert_eq!(value & BigUint::from(0xffff00u32), 0xf00500u32.into());
}

fn hierarchical_readmemh_honors_constant_branches_and_loops(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_directory, path) = memory_file("ab\n");
        let code = format!(r#"
            module Memory (word: output logic<8>) {{
                #[allow(unassign_variable)]
                var mem: logic<8>[1];
                assign word = mem[0];
            }}
            #[test(Top)]
            module Top {{
                var word: logic<8>;
                inst dut: Memory (word);
                initial {{
                    if 1'd0 {{ $readmemh("missing.hex", dut.mem); }}
                    for i in 0..2 {{
                        if i == 1 {{ $readmemh("{path}", dut.mem); }}
                    }}
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("word")), 0xabu32.into());
}

fn hierarchical_readmemh_flattens_multidimensional_arrays(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let (_directory, path) = memory_file("11\n22\n33\n44\n");
        let code = format!(r#"
            module Memory {{
                #[allow(unassign_variable)]
                var mem: logic<8>[2, 2];
            }}
            #[test(Top)]
            module Top {{
                inst dut: Memory;
                initial {{
                    $readmemh("{path}", dut.mem);
                    $assert(dut.mem[0][0] == 8'h11);
                    $assert(dut.mem[0][1] == 8'h22);
                    $assert(dut.mem[1][0] == 8'h33);
                    $assert(dut.mem[1][1] == 8'h44);
                    $finish();
                }}
            }}
        "#);
    }
    @build Simulator::builder(&code, "Top");
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
}

}
