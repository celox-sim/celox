use crate::Design;

cases! { Combinational, "comb_observer";


fn test_comb_function_packed_array_literal_preserves_source_order(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @build Design::new(r#"
module Top (
    q: output logic<2>,
    side: output logic<8>,
) {
    function observe (
        value: input logic<8>,
        written: output logic<8>,
    ) -> logic {
        written = value;
        return value[0];
    }
    function identity (x: input logic<2>) -> logic<2> {
        return x;
    }

    always_comb {
        side = 0;
        q = identity('{default: observe(8'h11, side), observe(8'h22, side)});
    }
}
"#, "Top");

    let side = sim.signal("side");
    assert_eq!(sim.get_as::<u8>(side), 0x22);
}



fn test_comb_function_nested_array_literals_preserve_each_dimension_order(sim) {

    @build Design::new(r#"
module Top (
    q00: output logic<4>,
    q01: output logic<4>,
    q10: output logic<4>,
    q11: output logic<4>,
) {
    function pick (
        x: input logic<4> [2, 2],
        row: input logic,
        col: input logic,
    ) -> logic<4> {
        return x[row][col];
    }

    always_comb {
        q00 = pick('{'{4'h1, 4'h2}, '{4'h3, 4'h4}}, 0, 0);
        q01 = pick('{'{4'h1, 4'h2}, '{4'h3, 4'h4}}, 0, 1);
        q10 = pick('{'{4'h1, 4'h2}, '{4'h3, 4'h4}}, 1, 0);
        q11 = pick('{'{4'h1, 4'h2}, '{4'h3, 4'h4}}, 1, 1);
    }
}
"#, "Top");

    let q00 = sim.signal("q00");
    let q01 = sim.signal("q01");
    let q10 = sim.signal("q10");
    let q11 = sim.signal("q11");
    assert_eq!(sim.get_as::<u8>(q00), 1);
    assert_eq!(sim.get_as::<u8>(q01), 2);
    assert_eq!(sim.get_as::<u8>(q10), 3);
    assert_eq!(sim.get_as::<u8>(q11), 4);
}



fn test_comb_function_array_literal_converts_scalar_items_per_element(sim) {

    @build Design::new(r#"
module Top (
    q0: output signed logic<8>,
    q1: output signed logic<8>,
    q_default: output signed logic<8>,
) {
    function pick (
        x: input signed logic<8> [2],
        index: input logic,
    ) -> signed logic<8> {
        return x[index];
    }

    always_comb {
        q0 = pick('{4'sh8, 4'sh1}, 0);
        q1 = pick('{4'sh8, 4'sh1}, 1);
        q_default = pick('{default: 4'sh8}, 1);
    }
}
"#, "Top");

    let q0 = sim.signal("q0");
    let q1 = sim.signal("q1");
    let q_default = sim.signal("q_default");
    assert_eq!(sim.get_as::<u8>(q0), 0xf8);
    assert_eq!(sim.get_as::<u8>(q1), 0x01);
    assert_eq!(sim.get_as::<u8>(q_default), 0xf8);
}



fn test_comb_function_nested_array_scalar_default_converts_each_element(sim) {

    @build Design::new(r#"
module Top (
    q00: output signed logic<8>,
    q01: output signed logic<8>,
    q10: output signed logic<8>,
    q11: output signed logic<8>,
) {
    function pick (
        x: input signed logic<8> [2, 2],
        row: input logic,
        col: input logic,
    ) -> signed logic<8> {
        return x[row][col];
    }

    always_comb {
        q00 = pick('{default: 4'sh8}, 0, 0);
        q01 = pick('{default: 4'sh8}, 0, 1);
        q10 = pick('{default: 4'sh8}, 1, 0);
        q11 = pick('{default: 4'sh8}, 1, 1);
    }
}
"#, "Top");

    let q00 = sim.signal("q00");
    let q01 = sim.signal("q01");
    let q10 = sim.signal("q10");
    let q11 = sim.signal("q11");
    assert_eq!(sim.get_as::<u8>(q00), 0xf8);
    assert_eq!(sim.get_as::<u8>(q01), 0xf8);
    assert_eq!(sim.get_as::<u8>(q10), 0xf8);
    assert_eq!(sim.get_as::<u8>(q11), 0xf8);
}



fn test_comb_function_array_literal_array_item_preserves_element_type(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @build Design::new(r#"
module Top (
    q: output signed logic<8>,
) {
    function first (x: input signed logic<8> [2, 2]) -> signed logic<8> {
        return x[0][0];
    }
    function pass (
        row0: input signed logic<4> [2],
        row1: input signed logic<4> [2],
    ) -> signed logic<8> {
        return first('{row0, row1});
    }

    always_comb {
        q = pass('{4'h8, 4'h0}, '{4'h1, 4'h2});
    }
}
"#, "Top");

    let q = sim.signal("q");
    assert_eq!(sim.get_as::<u8>(q), 0xf8);
}



fn test_comb_function_array_literal_accepts_array_returning_items(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @build Design::new(r#"
module Top (
    q: output logic<4>,
) {
    type row_t = logic<4> [2];
    type matrix_t = logic<4> [2, 2];

    function make_row (base: input logic<4>) -> row_t {
        var row: row_t;
        row[0] = base;
        row[1] = base + 1;
        return row;
    }
    function pick (x: input matrix_t) -> logic<4> {
        return x[1][0];
    }

    always_comb {
        q = pick('{make_row(1), make_row(3)});
    }
}
"#, "Top");

    let q = sim.signal("q");
    assert_eq!(sim.get_as::<u8>(q), 3);
}



fn test_comb_function_direct_array_argument_converts_each_element(sim) {

    @build Design::new(r#"
module Top (
    q0: output signed logic<8>,
    q1: output signed logic<8>,
) {
    function pick (
        x: input signed logic<8> [2],
        index: input logic,
    ) -> signed logic<8> {
        return x[index];
    }

    var narrow: signed logic<4> [2];
    always_comb {
        narrow[0] = 4'sh8;
        narrow[1] = 4'sh1;
        q0 = pick(narrow, 0);
        q1 = pick(narrow, 1);
    }
}
"#, "Top");

    let q0 = sim.signal("q0");
    let q1 = sim.signal("q1");
    assert_eq!(sim.get_as::<u8>(q0), 0xf8);
    assert_eq!(sim.get_as::<u8>(q1), 0x01);
}



fn test_comb_function_direct_array_return_preserves_all_elements(sim) {

    @build Design::new(r#"
module Top (
    q: output logic<4>,
) {
    type row_t = logic<4> [2];

    function make_row () -> row_t {
        var row: row_t;
        row[0] = 4'd3;
        row[1] = 4'd4;
        return row;
    }
    function pick (x: input row_t) -> logic<4> {
        return x[1];
    }

    always_comb {
        q = pick(make_row());
    }
}
"#, "Top");

    let q = sim.signal("q");
    assert_eq!(sim.get_as::<u8>(q), 4);
}



fn test_comb_statement_function_direct_array_argument_converts_each_element(sim) {

    @build Design::new(r#"
module Top (
    q: output signed logic<8>,
) {
    function capture (
        x: input signed logic<8> [2],
        dst: output signed logic<8>,
    ) {
        dst = x[0];
    }

    var narrow: signed logic<4> [2];
    always_comb {
        narrow[0] = 4'sh8;
        narrow[1] = 4'sh1;
        capture(narrow, q);
    }
}
"#, "Top");

    let q = sim.signal("q");
    assert_eq!(sim.get_as::<u8>(q), 0xf8);
}



fn test_named_function_inputs_evaluate_in_source_order(sim) {

    @build Design::new(r#"
module Top (
    value: input logic<8>,
    tmp: output logic<8>,
    out: output logic<8>,
) {
    function write_tmp (
        x: input logic<8>,
        dst: output logic<8>,
    ) -> logic<8> {
        dst = x;
        return x;
    }

    function add (
        first: input logic<8>,
        second: input logic<8>,
    ) -> logic<8> {
        return first + second;
    }

    always_comb {
        tmp = 8'd0;
        out = add(
            second: write_tmp(value, tmp),
            first: tmp,
        );
    }
}
"#, "Top");

    let value = sim.signal("value");
    let tmp = sim.signal("tmp");
    let out = sim.signal("out");

    sim.modify(|io| io.set(value, 13u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(tmp), 13);
    assert_eq!(sim.get_as::<u8>(out), 26);
}



fn test_named_function_outputs_apply_in_source_order(sim) {
    // The legacy case ID is retained. IEEE 1800-2023 4.9.7 and 13.5 require
    // blocking copy-out, but do not prescribe an order between output formals.

    @build Design::new(r#"
module Top (
    tmp: output logic<8>,
    out: output logic,
    first_value: output logic<8>,
    second_value: output logic<8>,
) {
    function write_outputs (
        first: output logic<8>,
        second: output logic<8>,
    ) -> logic {
        first = 8'd1;
        second = 8'd2;
        return 1'b1;
    }

    always_comb {
        tmp = 8'd0;
        out = write_outputs(
            second: tmp,
            first: tmp,
        );
        write_outputs(second: second_value, first: first_value);
    }
}
"#, "Top");

    let tmp = sim.signal("tmp");
    let out = sim.signal("out");

    let actual = sim.get_as::<u8>(tmp);
    assert!([1, 2].contains(&actual), "copy-out must leave one complete output value, got {actual}");
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(sim.get_as::<u8>(sim.signal("first_value")), 1);
    assert_eq!(sim.get_as::<u8>(sim.signal("second_value")), 2);
}



fn test_comb_function_loop_bounds_apply_output_effects_left_to_right(sim) {
    // Veryl 0.20.3 executes the design but drops the function output effects.

    @build Design::new(r#"
module Top (value: input logic<4>, out: output logic<8>) {
    function start_bound (x: input logic<4>, seen: output logic<8>) -> logic<4> {
        seen = 8'd16 + x;
        return 4'd0;
    }
    function end_bound (seen: input logic<8>) -> logic<4> {
        return seen[3:0];
    }
    function run (x: input logic<4>, seen: output logic<8>) -> logic<8> {
        seen = 8'd0;
        for i in start_bound(x, seen)..end_bound(seen) {}
        return seen;
    }
    var seen: logic<8>;
    always_comb {
        out = run(value, seen);
    }
}
"#, "Top");

    let value = sim.signal("value");
    let out = sim.signal("out");
    sim.modify(|io| io.set(value, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 21);
}



fn test_comb_function_loop_skips_conditions_after_break(sim) {
    // Veryl 0.20.3 executes the design but drops the function output effects.

    @build Design::new(r#"
module Top (
    stop: input logic,
    value: input logic<8>,
    if_out: output logic<8>,
    case_out: output logic<8>,
) {
    function mark (x: input logic<8>, seen: output logic<8>) -> logic {
        seen = x;
        return 1'b0;
    }
    function run_if (
        stop: input logic, x: input logic<8>, seen: output logic<8>,
    ) -> logic<8> {
        seen = 8'd0;
        for i in 0..3 {
            if stop { break; }
            if mark(x, seen) { break; }
        }
        return seen;
    }
    function run_case (
        stop: input logic, x: input logic<8>, seen: output logic<8>,
    ) -> logic<8> {
        seen = 8'd0;
        for i in 0..3 {
            if stop { break; }
            case mark(x, seen) {
                1'b1: { break; }
                default: {}
            }
        }
        return seen;
    }
    var if_seen: logic<8>;
    var case_seen: logic<8>;
    always_comb {
        if_out = run_if(stop, value, if_seen);
        case_out = run_case(stop, value, case_seen);
    }
}
"#, "Top");

    let stop = sim.signal("stop");
    let value = sim.signal("value");
    let if_out = sim.signal("if_out");
    let case_out = sim.signal("case_out");
    sim.modify(|io| {
        io.set(stop, 1u8);
        io.set(value, 29u8);
    }).unwrap();
    assert_eq!(sim.get_as::<u8>(if_out), 0);
    assert_eq!(sim.get_as::<u8>(case_out), 0);
    sim.modify(|io| io.set(stop, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(if_out), 29);
    assert_eq!(sim.get_as::<u8>(case_out), 29);
}
}
