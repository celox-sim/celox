use celox::{
    DeadStorePolicy, ResetType, Simulator, SimulatorErrorKind, TestResult,
    testbench::{compile_initial_testbench, run_compiled_testbench},
};
use veryl_analyzer::{AnalyzerError, analyzer_error::InvalidForRangeKind};
use veryl_metadata::Metadata;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const COUNTER: &str = r#"
    module Counter (
        clk: input  clock    ,
        rst: input  reset    ,
        cnt: output logic<32>,
    ) {
        always_ff {
            if_reset {
                cnt = 0;
            } else {
                cnt += 1;
            }
        }
    }
"#;

const CLOCK_TICK_COUNTER: &str = r#"
    module ClockTickCounter (
        clk  : input  clock    ,
        rst  : input  reset    ,
        ticks: output logic<32>,
    ) {
        always_ff {
            if_reset {
                ticks += 1;
            } else {
                ticks += 1;
            }
        }
    }
"#;

const BENCH_NATIVE_TB_COUNTER_N1000: &str = concat!(
    include_str!("../testdata/veryl/top_n1000.veryl"),
    include_str!("../testdata/veryl/native_tb_counter_n1000.veryl"),
);

fn bench_native_tb_std_counter() -> String {
    format!(
        "{}\n{}\n{}",
        test_utils::veryl_std::source(&["counter", "counter.veryl"]),
        include_str!("../testdata/veryl/std_counter_top.veryl"),
        include_str!("../testdata/veryl/native_tb_std_counter.veryl"),
    )
}

// ── Basic ──────────────────────────────────────────────────────────────

#[test]
fn test_native_testbench_ff_condition_reads_pre_edge_value_after_write() {
    let code = r#"
        module Dut (
            clk     : input  clock,
            present : input  logic,
            d       : input  logic<8>,
            captured: output logic<8>,
        ) {
            var in_flight: logic;
            always_ff (clk) {
                in_flight = present;
                if in_flight {
                    captured = d;
                }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var present: logic;
            var d: logic<8>;
            var captured: logic<8>;
            inst dut: Dut (clk, present, d, captured);

            initial {
                present = 1'b1;
                d = 8'hA5;
                clk.next(1);
                $assert(captured == 8'h00);

                present = 1'b0;
                d = 8'h3C;
                clk.next(1);
                $assert(captured == 8'h3C);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_native_testbench_overlapping_ff_writes_preserve_last_write() {
    let code = r#"
        module Dut (
            clk  : input  clock,
            state: output logic<128>,
        ) {
            always_ff (clk) {
                state = 128'h11111111111111111111111111111111;
                state[15:8] = 8'hAA;
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var state: logic<128>;
            inst dut: Dut (clk, state);

            initial {
                clk.next(1);
                $assert(state[23:0] == 24'h11AA11);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_native_testbench_uses_metadata_project_name() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $finish();
            }
        }
    "#;
    let metadata = Metadata::create_default("heliodor").unwrap();

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_random_methods_match_veryl_sequence() {
    let explicit = veryl_parser::resource_table::insert_str("r");
    veryl_simulator::random_table::reset(0);
    veryl_simulator::random_table::seed_handle(explicit, 1234);
    let exact =
        veryl_simulator::random_table::get_range(explicit, 100, 100, 8, false).payload_u64();
    let ranged = veryl_simulator::random_table::get_range(explicit, 0, 7, 8, false).payload_u64();
    let full0 = veryl_simulator::random_table::get(explicit, 8, false).payload_u64();
    let full1 = veryl_simulator::random_table::get(explicit, 8, false).payload_u64();

    let signed = veryl_parser::resource_table::insert_str("s");
    veryl_simulator::random_table::seed_handle(signed, 999);
    let signed_range =
        veryl_simulator::random_table::get_range(signed, 251, 5, 8, true).payload_u64();

    let derived = veryl_parser::resource_table::insert_str("derived");
    veryl_simulator::random_table::reset(42);
    let derived_seed = veryl_simulator::random_table::get_seed_handle(derived);
    let derived_value = veryl_simulator::random_table::get(derived, 16, false).payload_u64();

    let code = format!(
        r#"
        #[test(t)]
        module t {{
            var r      : $tb::random::<u8> ;
            var s      : $tb::random::<i8> ;
            var derived: $tb::random::<u16>;
            var x   : u8 ;
            var sx  : i8 ;
            var x16 : u16;
            var seed: u64;
            initial {{
                r.seed(1234);
                x = r.get_range(100, 100);
                $assert(x == 8'd{exact});
                x = r.get_range(0, 7);
                $assert(x == 8'd{ranged});
                x = r.get();
                $assert(x == 8'd{full0});
                x = r.get();
                $assert(x == 8'd{full1});
                seed = r.get_seed();
                $assert(seed == 64'd1234);

                s.seed(999);
                sx = s.get_range(-5, 5);
                $assert((sx as u8) == 8'd{signed_range});

                seed = derived.get_seed();
                $assert(seed == 64'd{derived_seed});
                x16 = derived.get();
                $assert(x16 == 16'd{derived_value});
                $finish();
            }}
        }}
        "#,
    );
    let mut metadata = Metadata::create_default("prj").unwrap();
    metadata.test.seed = Some(42);

    assert_eq!(
        Simulator::builder(&code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_random_signed_results_sign_extend_on_wider_stores() {
    let handle = veryl_parser::resource_table::insert_str("r");
    let get_seed = (0..10_000)
        .find(|seed| {
            veryl_simulator::random_table::seed_handle(handle, *seed);
            let get_value = veryl_simulator::random_table::get(handle, 8, true).payload_u64();
            get_value & 0x80 != 0
        })
        .expect("a signed random result with its sign bit set");
    let range_seed = (0..10_000)
        .find(|seed| {
            veryl_simulator::random_table::seed_handle(handle, *seed);
            let range_value =
                veryl_simulator::random_table::get_range(handle, 0x80, 0x7f, 8, true).payload_u64();
            range_value & 0x80 != 0
        })
        .expect("a signed ranged result with its sign bit set");
    veryl_simulator::random_table::seed_handle(handle, get_seed);
    let get_value = veryl_simulator::random_table::get(handle, 8, true).payload_u64();
    veryl_simulator::random_table::seed_handle(handle, range_seed);
    let range_value =
        veryl_simulator::random_table::get_range(handle, 0x80, 0x7f, 8, true).payload_u64();

    let code = format!(
        r#"
        #[test(t)]
        module t {{
            var r: $tb::random::<i8>;
            var widened_get: logic<16>;
            var widened_range: logic<16>;
            initial {{
                r.seed({get_seed});
                widened_get = r.get() as i16;
                r.seed({range_seed});
                widened_range = r.get_range(-128, 127) as i16;
                $finish();
            }}
        }}
        "#,
    );
    let mut sim = Simulator::builder(&code, "t").build().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);

    let widened_get = sim.get_as::<u16>(sim.signal("widened_get"));
    let widened_range = sim.get_as::<u16>(sim.signal("widened_range"));
    let expected_get = if get_value & 0x80 != 0 {
        0xff00 | get_value as u16
    } else {
        get_value as u16
    };
    let expected_range = if range_value & 0x80 != 0 {
        0xff00 | range_value as u16
    } else {
        range_value as u16
    };
    assert_eq!(widened_get, expected_get);
    assert_eq!(widened_range, expected_range);
}

#[test]
fn test_unset_testbench_seed_is_fresh_per_execution() {
    let code = r#"
        #[test(t)]
        module t {
            var random_seed: u64;
            var r: $tb::random::<u64>;
            initial {
                random_seed = r.get_seed();
                $finish();
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "t").build().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    let random_seed = sim.signal("random_seed");

    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    let first = sim.get_as::<u64>(random_seed);
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    let second = sim.get_as::<u64>(random_seed);

    assert_ne!(
        first, second,
        "an omitted seed must be drawn for each execution"
    );

    let mut metadata = Metadata::create_default("prj").unwrap();
    metadata.test.seed = Some(42);
    let mut explicit_sim = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap();
    let explicit_tb = compile_initial_testbench(&explicit_sim).unwrap();
    let explicit_seed = explicit_sim.signal("random_seed");
    assert_eq!(
        run_compiled_testbench(&mut explicit_sim, &explicit_tb),
        TestResult::Pass
    );
    let explicit_first = explicit_sim.get_as::<u64>(explicit_seed);
    assert_eq!(
        run_compiled_testbench(&mut explicit_sim, &explicit_tb),
        TestResult::Pass
    );
    let explicit_second = explicit_sim.get_as::<u64>(explicit_seed);
    assert_eq!(explicit_first, explicit_second);
}

#[test]
fn test_selected_testbench_destinations_update_only_selected_targets() {
    let random_handle = veryl_parser::resource_table::insert_str("r");
    veryl_simulator::random_table::seed_handle(random_handle, 42);
    let random_value = veryl_simulator::random_table::get(random_handle, 8, false).payload_u64();

    let cases = [
        format!(
            r#"
            #[test(t)]
            module t {{
                var values: logic<8>[4];
                var index: logic<2>;
                var r: $tb::random::<u8>;
                initial {{
                    values[0] = 8'h11;
                    values[1] = 8'h22;
                    values[2] = 8'h33;
                    values[3] = 8'h44;
                    index = 1;
                    r.seed(42);
                    values[index] = r.get();
                    $assert(values[0] == 8'h11);
                    $assert(values[1] == 8'd{random_value});
                    $assert(values[2] == 8'h33);
                    $assert(values[3] == 8'h44);
                    $finish();
                }}
            }}
        "#
        ),
        r#"
            #[test(t)]
            module t {
                var word: logic<8>;
                initial {
                    word = 8'hAA;
                    word[3] = 1'b0;
                    $assert(word == 8'hA2);
                    word[6:3] = 4'b0011;
                    $assert(word == 8'h9A);
                    $finish();
                }
            }
        "#
        .to_string(),
        format!(
            r#"
            module Driver (
                idx: output logic<2>,
            ) {{
                always_comb {{
                    idx = 2;
                }}
            }}

            #[test(t)]
            module t {{
                inst dut: Driver (idx);
                var idx: logic<2>;
                var values: logic<8>[4];
                var r: $tb::random::<u8>;

                initial {{
                    values[0] = 8'h11;
                    values[1] = 8'h22;
                    values[2] = 8'h33;
                    values[3] = 8'h44;
                    r.seed(42);
                    values[dut.idx] = r.get();
                    $assert(values[0] == 8'h11);
                    $assert(values[1] == 8'h22);
                    $assert(values[2] == 8'd{random_value});
                    $assert(values[3] == 8'h44);
                    $finish();
                }}
            }}
        "#
        ),
    ];
    for code in cases {
        assert_eq!(
            Simulator::builder(&code, "t").run_test().unwrap(),
            TestResult::Pass
        );
        assert_eq!(
            Simulator::builder(&code, "t").run_test_cranelift().unwrap(),
            TestResult::Pass
        );
    }
}

#[test]
fn test_packed_prefix_and_low_bound_testbench_destinations() {
    let code = r#"
        #[test(t)]
        module t {
            const W: u32 = 4;
            var word: logic<8>;
            var matrix: logic<4, 4>;
            initial {
                word = 8'hA0;
                word[W - 1:0] = 4'hF;
                $assert(word == 8'hAF);

                word = 8'hA0;
                word[0 +: W] = 4'h5;
                $assert(word == 8'hA5);

                matrix = 16'h0000;
                matrix[2][1] = 1'b1;
                $assert(matrix == 16'h0200);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass
    );
    assert_eq!(
        Simulator::builder(code, "t").run_test_cranelift().unwrap(),
        TestResult::Pass
    );
}

#[test]
fn test_selected_testbench_destinations_keep_dynamic_reads_and_old_value_live() {
    let random_handle = veryl_parser::resource_table::insert_str("r");
    veryl_simulator::random_table::seed_handle(random_handle, 42);
    let random_value = veryl_simulator::random_table::get(random_handle, 8, false).payload_u64();
    let code = format!(
        r#"
        module Driver (
            index: output logic<2>,
        ) {{
            always_comb {{
                index = 2;
            }}
        }}

        #[test(t)]
        module t {{
            var values: logic<8>[4];
            var index: logic<2>;
            var word: logic<8>;
            var r: $tb::random::<u8>;
            inst dut: Driver(index);

            initial {{
                values = '{{default: 8'h00}};
                word = 8'hA0;
                r.seed(42);
                values[index] = r.get();
                word[3] = 1'b1;
                $assert(values[2] == 8'd{random_value});
                $assert(word == 8'hA8);
                $finish();
            }}
        }}
        "#,
    );

    assert_eq!(
        Simulator::builder(&code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn test_counter_pass() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next  (10);
                $assert   (cnt == 32'd10);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_counter_fail() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next  (5);
                $assert   (cnt == 32'd99);
                $finish   ();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_testbench_direct_reads_are_dead_store_roots() {
    let code = r#"
        #[test(t)]
        module t {
            var hidden: logic<8>;

            always_comb {
                hidden = 8'd7;
            }

            initial {
                $assert(hidden == 8'd7);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_hierarchical_testbench_read_resolves_nested_instance_index_and_select() {
    let code = r#"
        module Core () {
            var words: logic<16>[2];

            always_comb {
                words[0] = 16'h1234;
                words[1] = 16'habcd;
            }
        }

        module Dut () {
            inst u_core: Core ();
        }

        #[test(t)]
        module t {
            inst dut: Dut ();
            var index: u32;

            initial {
                index = 1;
                $assert(dut.u_core.words[index][11:4] == 8'hbc);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_hierarchical_dynamic_reads_preserve_wide_values() {
    let code = r#"
        module Dut () {
            var words: logic<128>[2];

            always_comb {
                words[0] = 128'h0123_4567_89ab_cdef_fedc_ba98_7654_3210;
                words[1] = 128'hffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888;
            }
        }

        #[test(t)]
        module t {
            inst dut: Dut ();
            var index: u32;
            var bit_index: u32;

            initial {
                index = 1;
                bit_index = 68;
                $assert(dut.words[index] == 128'hffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888, "wide indexed read");
                $assert(dut.words[0][bit_index +: 8] == 8'hde, "wide dynamic select");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn test_hierarchical_dynamic_array_read_uses_native_layout() {
    let code = r#"
        module Dut (
            clk: input clock,
            narrow: output logic<3>[2],
        ) {
            always_ff {
                narrow[0] = 3'h1;
                narrow[1] = 3'h5;
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var narrow: logic<3>[2];
            inst dut: Dut (clk, narrow);
            var index: u32;

            initial {
                clk.next();
                index = 1;
                $assert(dut.narrow[index] == 3'h5);
                $finish();
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "t")
        .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
        .build_native()
        .unwrap();
    assert!(
        sim.program()
            .runtime_schema
            .testbench_read_roots
            .iter()
            .all(|address| !sim.layout().unpacked_arrays.contains_key(address))
    );
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench(&mut sim, &testbench),
        TestResult::Pass,
    );
}

#[test]
fn test_hierarchical_assert_message_argument_preserves_selected_width() {
    let code = r#"
        module Dut () {
            var word: logic<8>;

            always_comb {
                word = 8'hab;
            }
        }

        #[test(t)]
        module t {
            inst dut: Dut ();

            initial {
                $assert_continue(1'b0, "got %h", dut.word[3:0]);
                $finish();
            }
        }
    "#;

    let detailed = Simulator::builder(code, "t")
        .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
        .run_test_detailed()
        .unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("got b"));
}

#[test]
fn test_hierarchical_read_ignores_same_named_function_local() {
    let code = r#"
        module Dut () {
            var q: logic<8>;

            function shadow() -> logic<8> {
                var q: logic<8>;
                q = 8'h11;
                return q;
            }

            always_comb {
                q = 8'h42;
            }
        }

        #[test(t)]
        module t {
            inst dut: Dut ();

            initial {
                $assert(dut.q == 8'h42);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_hierarchical_select_widths_and_multidimensional_dynamic_indices() {
    let code = r#"
        module Dut () {
            var word: logic<8>;
            var mem: logic<8>[2, 2];
            var narrow: logic<3>[2, 2];
            var wide: logic<128>[2];
            var pix: logic<4, 4>;

            always_comb {
                word = 8'hab;
                mem[0][0] = 8'h11;
                mem[0][1] = 8'h12;
                mem[1][0] = 8'h21;
                mem[1][1] = 8'h22;
                narrow[0][0] = 3'h1;
                narrow[0][1] = 3'h2;
                narrow[1][0] = 3'h3;
                narrow[1][1] = 3'h5;
                wide[0] = 0;
                wide[1] = 128'h0000_0000_0000_0002_0000_0000_0000_0000;
                pix = 16'h0200;
            }
        }

        #[test(t)]
        module t {
            inst dut: Dut ();
            var i: u32;
            var j: u32;
            var anchor: u32;
            var step_index: u32;

            initial {
                i = 1;
                j = 1;
                anchor = 7;
                step_index = 1;
                $assert(dut.word[3 -: 4] == 4'hb, "minus-colon select");
                $assert(dut.word[anchor -: 4] == 4'ha, "dynamic minus-colon select");
                $assert(dut.word[step_index step 4] == 4'ha, "dynamic step select");
                $assert({dut.word[7:4], dut.word[3:0]} == 8'hab, "selected concat widths");
                $assert(dut.mem[i][1] == 8'h22, "dynamic outer index");
                $assert(dut.mem[i][j] == 8'h22, "multiple dynamic indices");
                $assert(dut.narrow[i][j] == 3'h5, "non-byte-aligned dynamic indices");
                $assert(dut.narrow[1][0][j] == 1'b1, "sub-byte static index and dynamic select");
                $assert(dut.wide[i][64:1] == 64'd0, "wide selected value is masked");
                $assert(dut.pix[2][1] == 1'b1, "multi-dimensional packed index");
                $assert(dut.pix[2][0] == 1'b0, "all packed indices are consumed");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .dead_store_policy(DeadStorePolicy::PreserveListedSignals)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_clock_only_self_updating_ff_advances_in_native_testbench_instance() {
    let code = r#"
        module ClockTickCounter (
            clk  : input  clock    ,
            ticks: output logic<32>,
        ) {
            always_ff (clk) {
                ticks += 1;
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var ticks: logic<32>;
            inst dut: ClockTickCounter (clk, ticks);

            initial {
                clk.next(5);
                $assert(ticks == 32'd5, "ticks=%d", ticks);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Wide signal (>64 bit) ──────────────────────────────────────────────

#[test]
fn test_wide_128bit() {
    let code = r#"
        module W (
            clk: input  clock      ,
            rst: input  reset      ,
            cnt: output logic<128> ,
        ) {
            always_ff {
                if_reset { cnt = 0; }
                else     { cnt += 1; }
            }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<128>;
            inst dut: W (clk, rst, cnt);
            initial {
                rst.assert();
                clk.next  (5);
                $assert   (cnt == 128'd5);
                $finish   ();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Reset polarity ─────────────────────────────────────────────────────

/// DUT uses generic `reset` type; builder overrides to AsyncHigh.
#[test]
fn test_reset_async_high() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next  (7);
                $assert   (cnt == 32'd7);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t")
            .reset_type(ResetType::AsyncHigh)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

// ── Reset duration ─────────────────────────────────────────────────────

#[test]
fn test_reset_explicit_duration() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(5);
                clk.next  (10);
                $assert   (cnt == 32'd10);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_reset_dynamic_duration_from_variable() {
    let code = format!(
        r#"
        {CLOCK_TICK_COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var ticks: logic<32>;
            var duration: logic<32>;
            inst dut: ClockTickCounter (clk, rst, ticks);

            initial {{
                duration = 5;
                rst.assert(duration);
                $assert(ticks == 32'd5, "ticks=%d", ticks);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_reset_zero_duration_clamps_to_one_cycle() {
    let code = format!(
        r#"
        {CLOCK_TICK_COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var ticks: logic<32>;
            var duration: logic<32>;
            inst dut: ClockTickCounter (clk, rst, ticks);

            initial {{
                duration = 0;
                rst.assert(duration);
                $assert(ticks == 32'd1, "ticks=%d", ticks);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_reset_legacy_clock_argument_clamps_to_one_cycle() {
    let code = format!(
        r#"
        {CLOCK_TICK_COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var ticks: logic<32>;
            inst dut: ClockTickCounter (clk, rst, ticks);

            initial {{
                rst.assert(clk);
                $assert(ticks == 32'd1, "ticks=%d", ticks);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_reset_dynamic_duration_from_loop_variable() {
    let code = format!(
        r#"
        {CLOCK_TICK_COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var ticks: logic<32>;
            inst dut: ClockTickCounter (clk, rst, ticks);

            initial {{
                for i in 1..=3 {{
                    rst.assert(i);
                }}
                $assert(ticks == 32'd6, "ticks=%d", ticks);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_reset_dynamic_duration_from_function_argument() {
    let code = format!(
        r#"
        {CLOCK_TICK_COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var ticks: logic<32>;
            inst dut: ClockTickCounter (clk, rst, ticks);

            function reset_for(duration: input logic<32>) {{
                rst.assert(duration);
            }}

            initial {{
                reset_for(2);
                reset_for(4);
                $assert(ticks == 32'd6, "ticks=%d", ticks);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── For loop ───────────────────────────────────────────────────────────

#[test]
fn test_for_loop_basic() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in 0..5 {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_step() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in 0..10 step += 2 {{
                    clk.next(2);
                }}
                $assert(cnt == 32'd20);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_bitwise_steps() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var or_end: logic<32>;
            var xor_end: logic<32>;
            var xor_wide_start: signed logic<32>;
            var xor_wide_end: signed logic<128>;
            var xor_wide_last: signed logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                or_end = 7;
                xor_end = 5;
                for i in 3..=or_end step |= 6 {{
                    clk.next(i);
                    if i == or_end {{
                        break;
                    }}
                }}
                $assert(cnt == 32'd10);
                for i in 3..=xor_end step ^= 6 {{
                    clk.next(i);
                    if i == xor_end {{
                        break;
                    }}
                }}
                $assert(cnt == 32'd18);
                xor_wide_start = (0 - 8) as 32;
                xor_wide_end = 2147483640;
                xor_wide_last = 0;
                for i in xor_wide_start..=xor_wide_end step ^= 2147483648 {{
                    xor_wide_last = i;
                    if i == 2147483640 {{
                        break;
                    }}
                }}
                $assert(xor_wide_last == 32'sh7fff_fff8);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_i32_bitwise_steps_discard_high_step_bits() {
    let code = r#"
        #[test(t)]
        module t {
            var or_end: signed logic<128>;
            var xor_end: signed logic<128>;
            var or_last: signed logic<32>;
            var xor_last: signed logic<32>;
            initial {
                or_end = 7;
                xor_end = 5;
                or_last = 0;
                for i in 3..=or_end step |= 4294967302 {
                    or_last = i;
                    if i == 7 {
                        break;
                    }
                }
                xor_last = 0;
                for i in 3..=xor_end step ^= 4294967302 {
                    xor_last = i;
                    if i == 5 {
                        break;
                    }
                }
                $assert(or_last == 7);
                $assert(xor_last == 5);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_i32_xor_step_with_only_high_bits_fails() {
    let code = r#"
        #[test(t)]
        module t {
            var end_bound: logic<32>;
            var last: logic<32>;
            initial {
                end_bound = 4;
                last = 0;
                for i in 3..end_bound step ^= 4294967296 {
                    last = i;
                }
                $finish();
            }
        }
    "#;
    let TestResult::Fail(message) = Simulator::builder(code, "t").run_test().unwrap() else {
        panic!("expected non-progressing loop failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_i32_or_step_with_only_existing_low_bits_fails() {
    let code = r#"
        #[test(t)]
        module t {
            var end_bound: logic<32>;
            var last: logic<32>;
            initial {
                end_bound = 4;
                last = 0;
                for i in 3..end_bound step |= 4294967299 {
                    last = i;
                }
                $finish();
            }
        }
    "#;
    let TestResult::Fail(message) = Simulator::builder(code, "t").run_test().unwrap() else {
        panic!("expected non-progressing loop failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_rev() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in rev 0..5 {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_break_exits_testbench_loop() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                for i in 0..10 {{
                    if i == 3 {{
                        break;
                    }}
                    clk.next();
                }}
                $assert(cnt == 32'd3);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_forward() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt >> 1;
                for _i in 0..limit {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_reverse() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                for _i in rev 0..(cnt >> 1) {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_inclusive() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(3);
                limit = cnt + 32'd2;
                for _i in 0..=limit {{
                    clk.next();
                }}
                $assert(cnt == 32'd9);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_stepped() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt >> 1;
                for _i in 1..limit step *= 2 {{
                    clk.next();
                }}
                $assert(cnt == 32'd13);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_stepped_non_progress_fails() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt;
                for _i in (limit - limit)..limit step *= 2 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_expression_bound_arith_shift_step() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt >> 1;
                for _i in 1..limit step <<<= 1 {{
                    clk.next();
                }}
                $assert(cnt == 32'd13);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_large_arith_shift_reports_non_progress() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt;
                for _i in 1..limit step <<<= 100 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
        panic!("expected non-progressing loop failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_i32_mul_and_shl_overflow_fail() {
    for (start, end, step) in [
        ("1500000000", "3100000000", "*= 2"),
        ("1073741824", "2147483649", "<<= 1"),
    ] {
        let code = format!(
            r#"
            #[test(t)]
            module t {{
                var end_bound: signed logic<64>;
                initial {{
                    end_bound = 64'sd{end};
                    for _i in {start}..end_bound step {step} {{}}
                    $finish();
                }}
            }}
        "#
        );
        let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
            panic!("expected non-progressing loop failure for step {step}");
        };
        assert!(message.contains("non-progressing stepped for loop"));
    }
}

#[test]
fn test_for_loop_static_bounds_use_signed_i32_progress() {
    for (start, end, step) in [
        ("1500000000", "1600000000", "*= 2"),
        ("1073741824", "1500000000", "<<= 1"),
        ("1", "100", "|= 2147483648"),
        ("1", "100", "^= 2147483648"),
    ] {
        let code = format!(
            r#"
            #[test(t)]
            module t {{
                initial {{
                    for _i in {start}..{end} step {step} {{}}
                    $finish();
                }}
            }}
        "#
        );
        let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
            panic!("expected signed i32 loop failure for step {step}");
        };
        assert!(message.contains("non-progressing stepped for loop"));
    }
}

#[test]
fn test_for_loop_static_signed_i32_upper_bound_is_rejected() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                for _i in 2147483647..2147483648 step += 1 {}
                $finish();
            }
        }
    "#;
    let error = Simulator::builder(code, "t").run_test().unwrap_err();
    let SimulatorErrorKind::Analyzer(errors) = error.kind() else {
        panic!("expected analyzer error, got {error:?}");
    };
    assert!(errors.iter().any(|error| matches!(
        error,
        AnalyzerError::InvalidForRange {
            kind: InvalidForRangeKind::NegativeBound,
            ..
        }
    )));
}

#[test]
fn test_for_loop_unsigned_dynamic_bounds_use_signed_i32_progress() {
    for (start, end, step) in [
        ("2147483647", "2147483648", "+= 1"),
        ("1500000000", "1600000000", "*= 2"),
        ("1073741824", "1500000000", "<<= 1"),
        ("1", "100", "|= 2147483648"),
        ("1", "100", "^= 2147483648"),
    ] {
        let code = format!(
            r#"
            #[test(t)]
            module t {{
                var start: logic<64>;
                var end_bound: logic<64>;
                initial {{
                    start = {start};
                    end_bound = {end};
                    for _i in start..end_bound step {step} {{}}
                    $finish();
                }}
            }}
        "#
        );
        let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
            panic!("expected signed i32 loop failure for dynamic step {step}");
        };
        assert!(message.contains("non-progressing stepped for loop"));
    }
}

#[test]
fn test_for_loop_large_multiplier_preserves_low_i32_bits() {
    let code = r#"
        #[test(t)]
        module t {
            var start: logic<64>;
            var end_bound: logic<64>;
            initial {
                start = 2;
                end_bound = 3;
                for _i in start..end_bound step *= 9223372036854775808 {}
                $finish();
            }
        }
    "#;
    let TestResult::Fail(message) = Simulator::builder(code, "t").run_test().unwrap() else {
        panic!("expected fixed-width multiplication failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_wide_singleton_still_checks_fixed_width_progress() {
    let code = r#"
        #[test(t)]
        module t {
            var bound: logic<128>;
            initial {
                bound = (128'd1 << 100);
                for _i in bound..=bound step *= 2 {}
                $finish();
            }
        }
    "#;
    let TestResult::Fail(message) = Simulator::builder(code, "t").run_test().unwrap() else {
        panic!("expected fixed-width progress failure for wide singleton bound");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_reverse_step_matches_emitted_sv_order() {
    let code = r#"
        #[test(t)]
        module t {
            var digits: logic<32>;
            initial {
                digits = 0;
                for i in rev 0..10 step += 2 {
                    digits = digits * 10 + i as 32;
                }
                $assert(digits == 32'd97531);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_reverse_i32_step_truncation_reports_non_progress() {
    let code = r#"
        #[test(t)]
        module t {
            var start: signed logic<64>;
            var end_bound: signed logic<64>;
            initial {
                start = 0;
                end_bound = 3;
                for _i in rev start..=end_bound step += 4294967296 {}
                $finish();
            }
        }
    "#;
    let TestResult::Fail(message) = Simulator::builder(code, "t").run_test().unwrap() else {
        panic!("expected reverse fixed-width step failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_expression_bound_non_progress_reports_failure() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt;
                for _i in (limit - limit)..limit step *= 2 {{
                    clk.next();
                }}
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_expression_bound_terminal_inclusive_mul_reports_non_progress() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                limit = cnt;
                for _i in (limit - limit)..=(limit - limit) step *= 2 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
        panic!("expected non-progressing loop failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
#[ignore = "upstream Veryl does not preserve reverse zero-step loops into the native testbench IR path exercised by this test"]
fn test_for_loop_expression_bound_reverse_zero_step_singleton_succeeds() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                for _i in rev 4..=4 step += 0 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_wide_bound_overflow_reports_failure() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<128>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                bound = 128'd1;
                for _i in 0..(bound << 64) {{
                    clk.next();
                }}
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_dynamic_signed_bound_preserves_negative_value() {
    let code = r#"
        #[test(t)]
        module t {
            var start: signed logic<32>;
            var hits: logic<32>;
            initial {
                start = (0 - 1) as 32;
                hits = 0;
                for _i in start..=1 {
                    hits += 1;
                }
                $assert(hits == 32'd3, "start=%d hits=%d", start, hits);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_wide_signed_bound_small_value_still_runs() {
    let code = r#"
        #[test(t)]
        module t {
            var start: signed logic<256>;
            var hits: logic<32>;
            initial {
                start = 1;
                hits = 0;
                for _i in start..=3 {
                    hits += 1;
                }
                $assert(hits == 32'd3);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_expression_vm_preserves_width_signedness_and_casts() {
    let code = r#"
        #[test(t)]
        module t {
            const NAMED: signed logic<5> = 5'sh19;
            var s8a: signed logic<8>;
            var s8b: signed logic<8>;
            var s5: signed logic<5>;
            var unsigned8: logic<8>;
            var one8: logic<8>;
            var num_cast: logic<16>;
            var unsigned_cast: logic<16>;
            var signed_cast: logic<16>;
            var implicit_widen: logic<16>;
            var ternary_widen: logic<16>;
            var named_cast: logic<16>;
            initial {
                s8a = 8'shF9;
                s8b = 8'sh02;
                s5 = 5'sh19;
                unsigned8 = 8'h02;
                one8 = 8'h01;

                $assert(
                    s8a / s8b == 8'shFD,
                    "signed div: a=%h b=%h q=%h",
                    s8a,
                    s8b,
                    s8a / s8b,
                );
                $assert(s8a % s8b == 8'shFF, "signed rem");
                $assert(s8a <: s8b, "signed compare");
                $assert(s8a >>> 1 == 8'shFC, "narrow signed shift");
                $assert(
                    s5 / unsigned8 == 8'h0C,
                    "mixed div: a=%h b=%h q=%h",
                    s5,
                    unsigned8,
                    s5 / unsigned8,
                );
                $assert(s5 % unsigned8 == 8'h01, "mixed rem");
                $assert(!(s5 <: unsigned8), "mixed compare");

                $assert(8'hFF + 8'd1 == 8'h00, "add wraps at expression width");
                $assert((8'd0 - 8'd1) >> 7 == 8'h01, "sub wraps at expression width");
                $assert((8'h80 << 1) >> 1 == 8'h00, "shift truncates at lhs width");
                $assert(-one8 == 8'hFF, "unary minus");
                $assert(|8'h02, "reduce or");
                $assert(!(^8'h03), "reduce xor");
                $assert(&8'hFF, "reduce and");
                $assert(~&8'h00, "reduce nand");
                $assert(~|8'h00, "reduce nor");
                $assert(~^8'h03, "reduce xnor");
                $assert(3 ** 4 == 32'd81, "power");
                $assert((8'hAA ~^ 8'hFF) == 8'hAA, "binary xnor");
                $assert(8'hAA ==? 8'hAA, "wildcard equality on two-state values");
                $assert(8'hAA !=? 8'h55, "wildcard inequality on two-state values");
                $assert($signed(s8a as u8) <: 8'sh01, "$signed reinterpretation");
                $assert(!($unsigned(s8a) <: 8'h01), "$unsigned reinterpretation");

                num_cast = s5 as 8;
                unsigned_cast = s5 as u8;
                signed_cast = s5 as i8;
                implicit_widen = s5;
                ternary_widen = if one8 ? s5 : 5'sh00;
                named_cast = NAMED as 8;
                $assert(num_cast == 16'hFFF9, "numeric cast keeps source signedness");
                $assert(unsigned_cast == 16'h00F9, "unsigned type cast reinterprets after resize");
                $assert(signed_cast == 16'hFFF9, "signed type cast remains signed");
                $assert(implicit_widen == 16'hFFF9, "assignment widens from rhs signedness");
                $assert(ternary_widen == 16'hFFF9, "ternary arms use their common context");
                $assert(
                    named_cast == 16'hFFF9,
                    "named constants retain cast signedness: named=%h cast=%h",
                    NAMED,
                    named_cast,
                );
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_expression_vm_preserves_wide_fixed_width_semantics() {
    let code = r#"
        #[test(t)]
        module t {
            var zero: logic<128>;
            var all_ones: logic<128>;
            var inverted: logic<128>;
            var signed_value: signed logic<128>;
            var shifted: logic<128>;
            var concatenated: logic<136>;
            initial {
                zero = 0;
                all_ones = zero - 128'd1;
                inverted = ~zero;
                signed_value = 128'sh8000_0000_0000_0000_0000_0000_0000_0000;
                shifted = signed_value >>> 1;
                concatenated = {8'hAA, zero};

                $assert(
                    all_ones == 128'hFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF,
                    "wide subtraction wraps",
                );
                $assert(
                    inverted == 128'hFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF,
                    "wide bitnot uses the expression width",
                );
                $assert(
                    shifted == 128'hC000_0000_0000_0000_0000_0000_0000_0000,
                    "wide arithmetic shift sign-extends",
                );
                $assert(
                    concatenated == 136'hAA_0000_0000_0000_0000_0000_0000_0000_0000,
                    "wide concatenation preserves the high part",
                );
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_inclusive_unrepresentable_max_bound_reports_non_progress() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<64>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                bound = 64'hffff_ffff_ffff_ffff;
                for _i in bound..=bound {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    let TestResult::Fail(message) = Simulator::builder(&code, "t").run_test().unwrap() else {
        panic!("expected non-progressing loop failure");
    };
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_for_loop_dynamic_wide_singleton_bound_runs_once() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<128>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                bound = (128'd1 << 100);
                for _i in bound..=bound {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Function call in testbench ──────────────────────────────────────────

#[test]
fn test_function_call() {
    let code = r#"
        module Counter2 (
            clk: input clock,
            rst: input reset,
            cnt: output logic<32>,
        ) {
            always_ff {
                if_reset { cnt = 0; }
                else { cnt += 1; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter2 (clk, rst, cnt);

            function step_n(n: input logic<32>) {
                clk.next(n);
            }

            initial {
                rst.assert();
                step_n(5);
                step_n(5);
                $assert(cnt == 32'd10);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

/// Factor::FunctionCall — function return value used in an expression.
#[test]
fn test_function_return_value_in_assert() {
    let code = r#"
        module Dut (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<8> ,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 8'd42; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<8>;
            inst dut: Dut (clk, rst, val);

            function double(x: input logic<8>) -> logic<8> {
                return x + x;
            }

            function add_offset(x: input logic<8>, offset: input logic<8>) -> logic<8> {
                return x + offset;
            }

            initial {
                rst.assert();
                clk.next(1);
                $assert(val == 8'd42);
                $assert(double(val) == 8'd84);
                $assert(add_offset(val, 8'd8) == 8'd50);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Dual clock ─────────────────────────────────────────────────────────

#[test]
fn test_dual_clock() {
    let code = r#"
        module DualClock (
            clk_a: input  'a clock    ,
            rst_a: input  'a reset    ,
            clk_b: input  'b clock    ,
            rst_b: input  'b reset    ,
            cnt_a: output 'a logic<32>,
            cnt_b: output 'b logic<32>,
        ) {
            always_ff (clk_a, rst_a) {
                if_reset { cnt_a = 0; }
                else     { cnt_a += 1; }
            }
            always_ff (clk_b, rst_b) {
                if_reset { cnt_b = 0; }
                else     { cnt_b += 1; }
            }
        }

        #[test(t)]
        module t {
            inst clk_a: $tb::clock_gen;
            inst rst_a: $tb::reset_gen(clk: clk_a);
            inst clk_b: $tb::clock_gen;
            inst rst_b: $tb::reset_gen(clk: clk_b);

            var cnt_a: logic<32>;
            var cnt_b: logic<32>;

            inst dut: DualClock (
                clk_a, rst_a, clk_b, rst_b, cnt_a, cnt_b,
            );

            initial {
                rst_a.assert();
                rst_b.assert();
                clk_a.next  (10);
                $assert     (cnt_a == 32'd10);
                $assert     (cnt_b == 32'd0);
                clk_b.next  (5);
                $assert     (cnt_a == 32'd10);
                $assert     (cnt_b == 32'd5);
                $finish     ();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Implicit $finish (no $finish → Pass) ───────────────────────────────

#[test]
fn test_no_finish_is_pass() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(3);
                $assert(cnt == 32'd3);
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Dynamic indexing ───────────────────────────────────────────────────

#[test]
fn test_dynamic_array_index_in_for() {
    let code = r#"
        module ArrayFill (
            clk: input  clock         ,
            rst: input  reset         ,
            arr: output logic<8>   [4],
        ) {
            for i in 0..4: g {
                always_ff {
                    if_reset { arr[i] = 0; }
                    else     { arr[i] = arr[i] + i as u8 + 8'd10; }
                }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var arr: logic<8>[4];
            inst dut: ArrayFill (clk, rst, arr);
            initial {
                rst.assert();
                clk.next(1);
                // arr[0]=10, arr[1]=11, arr[2]=12, arr[3]=13
                for i in 0..4 {
                    $assert(
                        arr[i] == i as u8 + 8'd10,
                        "i=%d arr=%d expected=%d",
                        i,
                        arr[i],
                        i as u8 + 8'd10,
                    );
                }
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Multiple assertions ────────────────────────────────────────────────

#[test]
fn test_multiple_assertions() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                $assert(cnt == 32'd0);
                clk.next(1);
                $assert(cnt == 32'd1);
                clk.next(1);
                $assert(cnt == 32'd2);
                clk.next(8);
                $assert(cnt == 32'd10);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_assert_continue_records_failure_and_continues() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                $assert_continue(cnt == 32'd99, "first failure: cnt=%d", cnt);
                clk.next(1);
                $assert(cnt == 32'd1, "second assertion");
                $finish();
            }}
        }}
    "#
    );
    let detailed = Simulator::builder(&code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 2);
    assert!(!detailed.assertions[0].passed);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("first failure: cnt=0"),
    );
    assert!(detailed.assertions[1].passed);

    let result = Simulator::builder(&code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("first failure: cnt=0".to_string()));
}

#[test]
fn test_run_test_collects_multiple_assert_continue_failures() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "first");
                $assert_continue(1'b0, "second");
                $finish();
            }
        }
    "#;
    let result = Simulator::builder(code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("first\nsecond".to_string()));
}

#[test]
fn test_run_test_preserves_runtime_error_after_assert_continue_failures() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var limit: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                $assert_continue(1'b0, "first");
                clk.next(2);
                limit = cnt;
                for _i in (limit - limit)..limit step *= 2 {{
                    clk.next();
                }}
                $finish();
            }}
        }}
    "#
    );
    let result = Simulator::builder(&code, "t").run_test().unwrap();
    let TestResult::Fail(message) = result else {
        panic!("expected failure");
    };
    assert!(message.contains("first"));
    assert!(message.contains("non-progressing stepped for loop"));
}

#[test]
fn test_run_test_does_not_duplicate_fatal_assert_message() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert(1'b0, "bad");
                $finish();
            }
        }
    "#;
    let result = Simulator::builder(code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("bad".to_string()));
}

#[test]
fn test_assert_format_args_render_runtime_values() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "mismatch: a=%d b=%d", 8'd3, 8'd7);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("mismatch: a=3 b=7"),
    );
}

#[test]
fn test_benchmark_native_testbench_fixtures_build() {
    Simulator::builder(BENCH_NATIVE_TB_COUNTER_N1000, "Top")
        .build()
        .unwrap();
    Simulator::builder(&bench_native_tb_std_counter(), "Top")
        .build()
        .unwrap();
}

#[test]
fn test_assert_format_args_follow_veryl_single_char_specifiers() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "cnt=%0d hex=%08x", 8'd3, 8'h0f);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("cnt=3 hex=f"),
    );
}

#[test]
fn test_passing_assert_uses_runtime_event_formatting() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b1, "cnt=%0d hex=%08x", 8'd3, 8'h0f);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert!(detailed.assertions[0].passed);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("cnt=3 hex=f"),
    );
}

#[test]
fn test_passing_assert_preserves_single_character_format_specifiers() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b1, "a=%d b=%d", 8'd3, 8'd7);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert!(detailed.assertions[0].passed);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("a=3 b=7"),);
}

#[test]
fn test_message_less_testbench_assert_uses_default_message() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert(1'b0);
                $finish();
            }
        }
    "#;
    let result = Simulator::builder(code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("assertion failed".to_string()));

    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("assertion failed"),
    );
}

#[test]
fn test_message_less_testbench_assert_continue_uses_default_message() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("assertion failed"),
    );
}

#[test]
fn test_assert_format_args_render_percent_m_and_t_without_args() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "loc=%m time=%t");
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("loc=<hierarchy> time=0"),
    );
}

#[test]
fn test_ff_runtime_events_drain_with_per_tick_time() {
    let code = r#"
        module Top (clk: input clock) {
            always_ff (clk) {
                $assert_continue(1'b0, "ff time=%t");
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst dut: Top (clk);
            initial {
                clk.next(3);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 3);
    assert_eq!(
        detailed
            .assertions
            .iter()
            .map(|a| a.message.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("ff time=1"), Some("ff time=2"), Some("ff time=3")],
    );
}

#[test]
fn test_assert_format_args_render_current_time_for_percent_t() {
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            initial {
                clk.next(3);
                $assert_continue(1'b0, "time=%t");
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("time=3"));
}

#[test]
fn test_assert_format_args_render_const_string_template() {
    let code = r#"
        #[test(t)]
        module t {
            const MSG: string = "x=%d";
            initial {
                $assert_continue(1'b0, MSG, 8'd3);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("x=3"));
}

#[test]
fn test_assert_dynamic_args_follow_display_style_formatting() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, 8'hab, 4'b1010);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("ab a"));
}

#[test]
fn test_assert_format_args_render_char_and_upper_hex() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "char=%c hex=%X", 8'd65, 8'hab);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("char=A hex=AB"),
    );
}

#[test]
fn test_assert_format_args_render_uppercase_aliases_like_lowercase() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "%B %O %D %I %S", 4'b1010, 8'o17, 8'd12, 8'd34, 8'd65);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("1010 17 12 34 A")
    );
}

#[test]
fn test_assert_format_args_preserve_binary_width_and_hex_alias() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "bin=%b hex=%h", 8'd1, 8'h2a);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("bin=00000001 hex=2a")
    );
}

#[test]
fn test_run_test_detailed_stops_on_plain_assert_failure() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert(1'b0, "first");
                $assert(1'b0, "second");
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert!(!detailed.assertions[0].passed);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("first"));
}

#[test]
fn test_run_test_detailed_collects_ff_assert_runtime_events() {
    let code = r#"
        module Top (clk: input clock, a: input logic<8>) {
            always_ff (clk) {
                $assert_continue(a != 8'd0, "ff a=%0d", a);
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var a: logic<8>;
            inst dut: Top (clk, a);
            initial {
                clk.next(1);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert!(!detailed.assertions[0].passed);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("ff a=0"));
}

#[test]
fn test_run_test_stops_on_ff_fatal_runtime_event() {
    let code = r#"
        module Top (clk: input clock) {
            always_ff (clk) {
                $assert(1'b0, "ff fatal");
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst dut: Top (clk);
            initial {
                clk.next(2);
                $assert_continue(1'b0, "after fatal");
                $finish();
            }
        }
    "#;
    let result = Simulator::builder(code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("ff fatal".to_string()));
}

// ── Array and bit select ───────────────────────────────────────────────

#[test]
fn test_unpacked_array_index() {
    let code = r#"
        module ArrayCounter (
            clk: input  clock         ,
            rst: input  reset         ,
            cnt: output logic<8>   [4],
        ) {
            for i in 0..4: g {
                always_ff {
                    if_reset { cnt[i] = 0; }
                    else     { cnt[i] = cnt[i] + i as u8 + 8'd1; }
                }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<8>[4];
            inst dut: ArrayCounter (clk, rst, cnt);
            initial {
                rst.assert();
                clk.next(1);
                $assert(cnt[0] == 8'd1);
                $assert(cnt[1] == 8'd2);
                $assert(cnt[2] == 8'd3);
                $assert(cnt[3] == 8'd4);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_bit_select() {
    let code = r#"
        module BitSel (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<16>,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 16'hABCD; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<16>;
            inst dut: BitSel (clk, rst, val);
            initial {
                rst.assert();
                clk.next(1);
                $assert(val == 16'hABCD);
                $assert(val[7:0] == 8'hCD);
                $assert(val[15:8] == 8'hAB);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Concatenation ──────────────────────────────────────────────────────

#[test]
fn test_concatenation() {
    let code = r#"
        module ConcatDut (
            clk: input  clock    ,
            rst: input  reset    ,
            hi:  output logic<8> ,
            lo:  output logic<8> ,
        ) {
            always_ff {
                if_reset { hi = 0; lo = 0; }
                else     { hi = 8'hAB; lo = 8'hCD; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var hi: logic<8>;
            var lo: logic<8>;
            inst dut: ConcatDut (clk, rst, hi, lo);
            initial {
                rst.assert();
                clk.next(1);
                $assert({hi, lo} == 16'hABCD);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_repeat_concatenation() {
    let code = r#"
        module RepDut (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<4> ,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 4'b1010; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<4>;
            inst dut: RepDut (clk, rst, val);
            initial {
                rst.assert();
                clk.next(1);
                $assert({val repeat 2} == 8'b1010_1010);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Operators in assertions ────────────────────────────────────────────

#[test]
fn test_comparison_operators() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(5);
                $assert(cnt == 32'd5);
                $assert(cnt != 32'd0);
                $assert(cnt >: 32'd4);
                $assert(cnt >= 32'd5);
                $assert(cnt <: 32'd6);
                $assert(cnt <= 32'd5);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_arithmetic_in_assert() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert();
                clk.next(10);
                $assert(cnt + 32'd5 == 32'd15);
                $assert(cnt - 32'd3 == 32'd7);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_runtime_if_in_initial_is_left_for_testbench_runner() {
    let code = r#"
        #[test(t)]
        module t {
            var done: logic;
            initial {
                done = 1;
                if done {
                    $finish();
                }
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}
