use celox::{CompilationWarning, FrontendDiagnostic, ResetType, Simulator, SimulatorErrorKind};

fn expect_mutable_bound_error(code: &str, top: &str) {
    let error = Simulator::builder(code, top)
        .build()
        .expect_err("mutable continuation bound must be rejected");
    assert!(
        matches!(
            error.kind(),
            SimulatorErrorKind::Frontend(diagnostics)
                if diagnostics.iter().any(|diagnostic| matches!(
                    diagnostic,
                    FrontendDiagnostic::MutableForBound { .. }
                ))
        ),
        "unexpected diagnostic: {error}"
    );
}

fn expect_time_advancing_bound_warning(code: &str, top: &str) {
    let simulator = Simulator::builder(code, top)
        .build()
        .expect("time-advancing continuation bounds are warnings, not errors");
    assert!(
        simulator.warnings().iter().any(|warning| matches!(
            warning,
            CompilationWarning::Frontend(FrontendDiagnostic::TimeAdvancingForBound { .. })
        )),
        "expected a time-advancing continuation-bound warning"
    );
}

#[test]
fn direct_and_nested_body_writes_are_errors() {
    expect_mutable_bound_error(
        r#"
module Top {
    var limit: logic<8>;
    var count: logic<8>;

    always_comb {
        limit = 4;
        count = 0;
        for _i in 0..limit {
            for _j in 0..1 {
                limit = 1;
            }
            count += 1;
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn writes_hidden_in_rhs_functions_are_errors() {
    expect_mutable_bound_error(
        r#"
module Top {
    var limit: logic<8>;
    var sink: logic<8>;

    function shrink() -> logic<8> {
        limit = 1;
        return 0;
    }

    always_comb {
        limit = 4;
        sink = 0;
        for _i in 0..limit {
            sink = shrink();
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn function_output_writeback_to_the_bound_is_an_error() {
    expect_mutable_bound_error(
        r#"
module Top {
    var limit: logic<8>;

    function shrink (
        value: output logic<8>,
    ) {
        value = 1;
    }

    always_comb {
        limit = 4;
        for _i in 0..limit {
            shrink(limit);
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn effects_in_the_bound_itself_are_errors() {
    expect_mutable_bound_error(
        r#"
module Top {
    var limit: logic<8>;
    var evaluations: logic<8>;

    function get_limit() -> logic<8> {
        evaluations += 1;
        return limit;
    }

    always_comb {
        limit = 4;
        evaluations = 0;
        for _i in 0..get_limit() {
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn time_advancing_effects_in_the_bound_itself_are_errors() {
    expect_mutable_bound_error(
        r#"
#[test(t)]
module t {
    inst clk: $tb::clock_gen;

    function get_limit() -> logic<8> {
        clk.next();
        return 1;
    }

    initial {
        for _i in 0..get_limit() {
        }
        $finish();
    }
}
"#,
        "t",
    );
}

#[test]
fn file_effects_in_the_bound_itself_are_errors() {
    expect_mutable_bound_error(
        r#"
#[test(t)]
module t {
    var file: $tb::file;

    function get_limit() -> logic<8> {
        file.open("mutable_for_bound.txt");
        file.write("bound");
        file.flush();
        file.close();
        return 1;
    }

    initial {
        for _i in 0..get_limit() {
        }
        $finish();
    }
}
"#,
        "t",
    );
}

#[test]
fn only_the_continuation_side_of_the_range_is_protected() {
    let forward = r#"
module Top (
    out: output logic<8>,
) {
    var start: logic<8>;
    always_comb {
        start = 0;
        out = 0;
        for _i in start..4 {
            start = 3;
            out += 1;
        }
    }
}
"#;
    Simulator::builder(forward, "Top")
        .build()
        .expect("a forward loop does not re-evaluate its start bound");

    expect_mutable_bound_error(
        r#"
module Top {
    var start: logic<8>;
    always_comb {
        start = 0;
        for _i in rev start..4 {
            start = 3;
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn bit_disjoint_and_unrelated_writes_remain_valid() {
    let code = r#"
module Top (
    out: output logic<8>,
) {
    var bits: logic<8>;
    var unrelated: logic<8>;

    always_comb {
        bits = 4;
        unrelated = 0;
        for _i in 0..bits[3:0] {
            bits[7:4] = 1;
            unrelated += 1;
        }
        out = unrelated;
    }
}
"#;
    Simulator::builder(code, "Top")
        .build()
        .expect("bit-disjoint writes must not conflict");
}

#[test]
fn dynamic_indices_are_checked_conservatively() {
    expect_mutable_bound_error(
        r#"
module Top {
    var limits: logic<8>[4];
    var index: logic<2>;

    always_comb {
        limits = '{1, 1, 4, 1};
        index = 2;
        for _i in 0..limits[2] {
            limits[index] = 1;
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn true_ff_writes_are_invisible_but_ff_locals_are_errors() {
    let ff_state = r#"
module Top (
    clk: input clock,
    rst: input reset,
) {
    var limit: logic<8>;
    var count: logic<8>;

    always_ff {
        if_reset {
            limit = 4;
            count = 0;
        } else {
            for _i in 0..limit {
                limit = 1;
                count += 1;
            }
        }
    }
}
"#;
    Simulator::builder(ff_state, "Top")
        .build()
        .expect("FF/NBA writes are not visible to the running loop");

    expect_mutable_bound_error(
        r#"
module Top (
    clk: input clock,
    rst: input reset,
) {
    always_ff {
        if_reset {
        } else {
            var limit: logic<8>;
            limit = 4;
            for _i in 0..limit {
                limit = 1;
            }
        }
    }
}
"#,
        "Top",
    );
}

#[test]
fn time_advancing_body_effects_warn_when_the_bound_is_in_the_event_write_closure() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
    count: output logic<8>,
) {
    always_ff {
        if_reset { count = 0; }
        else       { count += 1; }
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    var count: logic<8>;
    inst dut: Counter (clk, rst, count);

    initial {
        rst.assert();
        clk.next(2);
        for _i in 0..count {
            clk.next();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn hierarchical_bound_warns_when_clock_advances_the_target_state() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
) {
    var count: logic<8>;

    always_ff {
        if_reset { count = 0; }
        else       { count += 1; }
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    inst dut: Counter (clk, rst);

    initial {
        rst.assert();
        clk.next(2);
        for _i in 0..dut.count[3:0] {
            clk.next();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn hierarchical_bound_respects_disjoint_bit_ranges() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
) {
    var count: logic<8>;

    always_ff {
        if_reset { count[3:0] = 0; }
        else       { count[3:0] += 1; }
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    inst dut: Counter (clk, rst);

    initial {
        rst.assert();
        for _i in 0..dut.count[7:4] {
            clk.next();
        }
        $finish();
    }
}
"#;
    let simulator = Simulator::builder(code, "t")
        .build()
        .expect("hierarchical reads of disjoint bits must not conflict");
    assert!(!simulator.warnings().iter().any(|warning| matches!(
        warning,
        CompilationWarning::Frontend(FrontendDiagnostic::TimeAdvancingForBound { .. })
    )));
}

#[test]
fn hierarchical_bound_connected_to_immediately_written_root_is_an_error() {
    let code = r#"
module Limit (
    limit: input logic<8>,
) {}

#[test(t)]
module t {
    var limit: logic<8>;
    inst dut: Limit (limit);

    initial {
        limit = 2;
        for _i in 0..dut.limit {
            limit = 0;
        }
        $finish();
    }
}
"#;
    expect_mutable_bound_error(code, "t");
}

#[test]
fn hierarchical_bound_connected_to_disjoint_root_bits_is_allowed() {
    let code = r#"
module Limit (
    limit: input logic<8>,
) {}

#[test(t)]
module t {
    var upper: logic<4>;
    var lower: logic<4>;
    var limit: logic<8>;
    inst dut: Limit (limit);

    always_comb {
        limit = {upper, lower};
    }

    initial {
        upper = 2;
        lower = 0;
        for _i in 0..dut.limit[7:4] {
            lower = 1;
        }
        $finish();
    }
}
"#;
    Simulator::builder(code, "t")
        .build()
        .expect("writes to disjoint connected bits must not conflict");
}

#[test]
fn time_advancing_body_effects_pass_when_the_bound_is_outside_the_event_write_closure() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
    count: output logic<8>,
) {
    always_ff {
        if_reset { count = 0; }
        else       { count += 1; }
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    var count: logic<8>;
    var limit: logic<8>;
    inst dut: Counter (clk, rst, count);

    initial {
        limit = 2;
        rst.assert();
        for _i in 0..limit {
            clk.next();
        }
        $finish();
    }
}
"#;
    let simulator = Simulator::builder(code, "t")
        .build()
        .expect("an unrelated event write closure must not reject the loop");
    assert!(!simulator.warnings().iter().any(|warning| matches!(
        warning,
        CompilationWarning::Frontend(FrontendDiagnostic::UnknownForBoundEffect { .. })
    )));
}

#[test]
fn time_advancing_body_effects_respect_disjoint_bit_ranges() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
    count: output logic<8>,
) {
    always_ff {
        if_reset { count[3:0] = 0; }
        else       { count[3:0] += 1; }
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    var count: logic<8>;
    inst dut: Counter (clk, rst, count);

    initial {
        rst.assert();
        for _i in 0..count[7:4] {
            clk.next();
        }
        $finish();
    }
}
"#;
    Simulator::builder(code, "t")
        .build()
        .expect("event writes to disjoint bits must not reject the loop");
}

#[test]
fn event_write_closure_warning_propagates_through_comb_logic_and_testbench_functions() {
    let code = r#"
module Counter (
    clk: input clock,
    count: output logic<8>,
    limit: output logic<8>,
) {
    always_ff {
        count += 1;
    }
    always_comb {
        limit = count + 1;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var count: logic<8>;
    var limit: logic<8>;
    inst dut: Counter (clk, count, limit);

    function run() {
        for _i in 0..limit {
            clk.next();
        }
    }

    initial {
        run();
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn event_calls_hidden_in_testbench_helpers_warn() {
    let code = r#"
module Counter (
    clk: input clock,
    count: output logic<8>,
) {
    always_ff {
        count += 1;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var count: logic<8>;
    inst dut: Counter (clk, count);

    function tick() {
        clk.next();
    }

    initial {
        for _i in 0..count {
            tick();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn asynchronous_reset_write_closure_is_a_warning() {
    let code = r#"
module Counter (
    clk: input clock,
    rst: input reset,
    count: output logic<8>,
) {
    always_ff {
        if_reset { count = 0; }
        else       { count += 1; }
    }
}

#[test(t)]
module t {
    inst reset_clk: $tb::clock_gen;
    inst dut_clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk: reset_clk);
    var count: logic<8>;
    inst dut: Counter (clk: dut_clk, rst, count);

    initial {
        for _i in 0..count {
            rst.assert();
        }
        $finish();
    }
}
"#;
    let simulator = Simulator::builder(code, "t")
        .reset_type(ResetType::AsyncHigh)
        .build()
        .expect("an asynchronous reset conflict is a warning");
    assert!(
        simulator.warnings().iter().any(|warning| matches!(
            warning,
            CompilationWarning::Frontend(FrontendDiagnostic::TimeAdvancingForBound { .. })
        )),
        "expected an asynchronous reset continuation-bound warning"
    );
}

#[test]
fn event_write_closure_warning_follows_cascaded_clock_domains() {
    let code = r#"
module Counter (
    clk: input clock,
    enable: input logic,
    count: output logic<8>,
) {
    let gated_clk: '_ clock = clk & enable;

    always_ff (gated_clk) {
        count += 1;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var enable: logic;
    var count: logic<8>;
    inst dut: Counter (clk, enable, count);

    initial {
        enable = 1;
        for _i in 0..count {
            clk.next();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn event_write_closure_warning_tracks_concat_slice_and_mux_bits() {
    let code = r#"
module Counter (
    clk: input clock,
    select: input logic,
    count: output logic<8>,
    limit: output logic<4>,
) {
    var combined: logic<12>;

    always_ff {
        count += 1;
    }
    always_comb {
        combined = {count, 4'h0};
        limit = if select ? combined[7:4] : 4'h0;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var select: logic;
    var count: logic<8>;
    var limit: logic<4>;
    inst dut: Counter (clk, select, count, limit);

    initial {
        select = 1;
        for _i in 0..limit {
            clk.next();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn event_write_closure_preserves_disjoint_bits_through_concat_and_slice() {
    let code = r#"
module Counter (
    clk: input clock,
    count: output logic<8>,
    limit: output logic<4>,
) {
    var combined: logic<12>;

    always_ff {
        count[3:0] += 1;
    }
    always_comb {
        combined = {count, 4'h0};
        limit = combined[11:8];
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var count: logic<8>;
    var limit: logic<4>;
    inst dut: Counter (clk, count, limit);

    initial {
        for _i in 0..limit {
            clk.next();
        }
        $finish();
    }
}
"#;
    Simulator::builder(code, "t")
        .build()
        .expect("comb propagation must preserve disjoint packed bit ranges");
}

#[test]
fn event_write_closure_warns_for_dynamic_unpacked_array_writes_conservatively() {
    let code = r#"
module Counter (
    clk: input clock,
    index: input logic<2>,
    limits: output logic<8>[4],
) {
    always_ff {
        limits[index] += 1;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var index: logic<2>;
    var limits: logic<8>[4];
    inst dut: Counter (clk, index, limits);

    initial {
        index = 0;
        for _i in 0..limits[2] {
            clk.next();
        }
        $finish();
    }
}
"#;
    expect_time_advancing_bound_warning(code, "t");
}

#[test]
fn event_write_closure_preserves_disjoint_unpacked_array_elements() {
    let code = r#"
module Counter (
    clk: input clock,
    limits: output logic<8>[4],
) {
    always_ff {
        limits[0] += 1;
    }
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    var limits: logic<8>[4];
    inst dut: Counter (clk, limits);

    initial {
        for _i in 0..limits[2] {
            clk.next();
        }
        $finish();
    }
}
"#;
    Simulator::builder(code, "t")
        .build()
        .expect("event writes to another unpacked element must remain valid");
}
