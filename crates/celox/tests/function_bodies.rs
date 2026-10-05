#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

// The Veryl reference simulator mis-evaluates an output bound to a
// concatenation and does not model X for a missing return value.
all_backends! {

fn test_dynamic_index_write_to_function_local(sim) {
    @case "function_bodies::test_dynamic_index_write_to_function_local";
}

fn test_out_of_range_dynamic_write_to_function_local_is_ignored(sim) {
    @case "function_bodies::test_out_of_range_dynamic_write_to_function_local_is_ignored";
}

fn test_concatenated_destination_in_function_body(sim) {
    @case "function_bodies::test_concatenated_destination_in_function_body";
}

fn test_nested_output_to_concat_and_dynamic_local(sim) {
    @ignore_on(veryl);
    @case "function_bodies::test_nested_output_to_concat_and_dynamic_local";
}

fn test_path_without_return_yields_unknown(sim) {
    @ignore_on(veryl);
    @case "function_bodies::test_path_without_return_yields_unknown";
}

fn test_runtime_select_of_expression_bound_formal(sim) {
    @case "function_bodies::test_runtime_select_of_expression_bound_formal";
}

fn test_runtime_bounded_loop_in_function(sim) {
    @case "function_bodies::test_runtime_bounded_loop_in_function";
}

fn test_runtime_effects_nested_in_function_statements(sim) {
    @ignore_on(sv);
    @case "function_bodies::test_runtime_effects_nested_in_function_statements";
}

fn test_runtime_bounded_loop_function_reentry(sim) {
    @case "function_bodies::test_runtime_bounded_loop_function_reentry";
}

}

// The analyzer unrolls constant-bound loops by cloning the body; each copy's
// runtime effect must observe that iteration's state.
#[test]
fn test_ff_function_unrolled_loop_effects_observe_each_iteration() {
    let code = r#"
        module Top (clk: input clock, a: input logic<8>, q: output logic<8>) {
            function count (x: input logic<8>) -> logic<8> {
                var r: logic<8>;
                r = 8'd0;
                for i in 0..3 {
                    if x[i] {
                        $display("bit=%0d r=%0d", i, r);
                        r = r + 8'd1;
                    }
                }
                return r;
            }
            always_ff (clk) {
                q = count(a);
            }
        }
    "#;
    let mut sim = celox::Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let q = sim.signal("q");
    sim.modify(|io| io.set(a, 0b101u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 2);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "bit=0 r=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "bit=2 r=1".to_string(),
            },
        ],
    );
}

fn build(code: &str) -> celox::Simulator {
    celox::Simulator::builder(code, "Top").build().unwrap()
}

fn displays(sim: &mut celox::Simulator) -> Vec<String> {
    sim.drain_runtime_events()
        .into_iter()
        .map(|event| match event {
            celox::RuntimeEvent::Display { message } => message,
            other => panic!("unexpected runtime event: {other:?}"),
        })
        .collect()
}

// Unpacked-array inputs are copied when the call evaluates its arguments, so
// a later argument's output copy-out does not change them. On the first edge
// the uninitialized element is still X when `pick` copies it.
#[test]
fn test_ff_unpacked_input_is_snapshotted_before_later_output_effect() {
    let mut sim = celox::Simulator::builder(
        r#"
        module Top (clk: input clock, out_q: output logic<8>) {
            function pick (values: input logic<8>[2], ignored: input logic<8>) -> logic<8> {
                return values[0];
            }
            function update (value: output logic<8>) -> logic<8> {
                value = 8'h00;
                return 8'h00;
            }
            always_ff (clk) {
                var samples: logic<8>[2];
                out_q = pick(samples, update(samples[0]));
            }
        }
    "#,
        "Top",
    )
    .four_state(true)
    .build()
    .unwrap();
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_four_state(out_q).1, 0xffu8.into());
}

// The selected row is fixed by the index value when the call starts; the
// callee's write to the index is copied out only afterwards.
#[test]
fn test_ff_selected_unpacked_input_uses_index_at_call_time() {
    let mut sim = build(
        r#"
        module Top (clk: input clock, rows: input logic<8>[2, 2], out_q: output logic<8>) {
            function pick (values: input logic<8>[2], index: output logic) -> logic<8> {
                index = 1'b1;
                return values[1];
            }
            always_ff (clk) {
                var index: logic;
                out_q = pick(rows[index], index);
            }
        }
    "#,
    );
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");
    let rows = sim.signal("rows");
    // rows[r][c] occupies bits 16r+8c+7:16r+8c of the flattened array.
    sim.modify(|io| io.set(rows, 0x4433_2211u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out_q), 0x22);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out_q), 0x44);
}

#[test]
fn test_ff_unpacked_literal_input_is_snapshotted_before_output_index_effect() {
    let mut sim = build(
        r#"
        module Top (clk: input clock, d: input logic<8>, out_q: output logic<8>) {
            function pick (values: input logic<8>[2], result: output logic<8>) -> logic<8> {
                result = 8'h00;
                return values[0];
            }
            function update (value: output logic<8>) -> logic {
                value = 8'h00;
                return 1'b0;
            }
            always_ff (clk) {
                var changing: logic<8>;
                var sink: logic<8>[2];
                changing = d;
                out_q = pick('{changing, default: 8'h00}, sink[update(changing)]);
            }
        }
    "#,
    );
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let out_q = sim.signal("out_q");
    for value in [0x5au8, 0x80] {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(out_q), value);
    }
}

#[test]
fn test_ff_function_runtime_effect_in_for_bound_runs_once() {
    let mut sim = build(
        r#"
        module Top (clk: input clock, count: input logic<3>) {
            function observed (x: input logic<3>) -> logic<3> {
                $display("bound=%0d", x);
                return x;
            }
            function consume (n: input logic<3>) {
                for i in observed(n)..n {}
            }
            always_ff (clk) {
                consume(count);
            }
        }
    "#,
    );
    let clk = sim.event("clk");
    let count = sim.signal("count");
    sim.modify(|io| io.set(count, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(displays(&mut sim), ["bound=5"]);
}

#[test]
fn test_ff_function_runtime_effects_in_destinations_run_once() {
    let mut sim = build(
        r#"
        module Top (clk: input clock, index: input logic<3>, q: output logic<8>, r: output logic<8>) {
            function observed (x: input logic<3>) -> logic<3> {
                $display("index=%0d", x);
                return x;
            }
            function set (value: output logic) {
                value = 1'b1;
            }
            function assign_bit (i: input logic<3>) -> logic<8> {
                var tmp: logic<8>;
                tmp = 8'd0;
                tmp[observed(i)] = 1'b1;
                return tmp;
            }
            function output_bit (i: input logic<3>) -> logic<8> {
                var tmp: logic<8>;
                tmp = 8'd0;
                set(tmp[observed(i)]);
                return tmp;
            }
            always_ff (clk) {
                q = assign_bit(index);
                r = output_bit(index);
            }
        }
    "#,
    );
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let q = sim.signal("q");
    let r = sim.signal("r");
    sim.modify(|io| io.set(index, 6u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 0x40);
    assert_eq!(sim.get_as::<u8>(r), 0x40);
    assert_eq!(displays(&mut sim), ["index=6", "index=6"]);
}

// Writing a packed array through an out-of-range index performs no operation
// (IEEE 1800-2023 7.4.6). Kept out of the shared suite because Verilator 5.052
// truncates the index and writes another element.
#[test]
fn test_ff_out_of_range_packed_write_to_function_local_is_ignored() {
    let mut sim = build(
        r#"
        module Top (clk: input clock, i: input logic<2>, q: output logic<8>) {
            function put (idx: input logic<2>) -> logic<8> {
                var m: logic<2, 4>;
                m = 8'h21;
                m[idx] = 4'ha;
                return m;
            }
            always_ff (clk) {
                q = put(i);
            }
        }
    "#,
    );
    let clk = sim.event("clk");
    let i = sim.signal("i");
    let q = sim.signal("q");
    for (index, expected) in [(0u8, 0x2au8), (1, 0xa1), (2, 0x21), (3, 0x21)] {
        sim.modify(|io| io.set(i, index)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(q), expected, "index {index}");
    }
}
