use super::*;

sv_backends! {
    fn display_arguments_are_sized_as_ieee_specifies(sim) {
        @case "system_functions::display_arguments_are_sized_as_ieee_specifies";
    }

    fn display_field_widths_expand_to_the_value(sim) {
        @case "system_functions::display_field_widths_expand_to_the_value";
    }

    fn countones_preserves_argument_and_return_types(sim) {
        @case "system_functions::countones_preserves_argument_and_return_types";
    }

    fn countones_ignores_unknown_bits_in_comb_and_ff(sim) {
        @case "system_functions::countones_ignores_unknown_bits_in_comb_and_ff";
    }

    fn countones_in_constant_expressions(sim) {
        @case "system_functions::countones_in_constant_expressions";
    }

    fn bit_vector_predicates_preserve_argument_and_return_types(sim) {
        @case "system_functions::bit_vector_predicates_preserve_argument_and_return_types";
    }

    fn bit_vector_predicates_handle_unknown_bits_in_comb_ff_and_ports(sim) {
        @case "system_functions::bit_vector_predicates_handle_unknown_bits_in_comb_ff_and_ports";
    }

    fn bit_vector_predicates_in_constant_expressions(sim) {
        @case "system_functions::bit_vector_predicates_in_constant_expressions";
    }
}

#[test]
fn rejects_bit_vector_functions_with_missing_or_extra_arguments() {
    for name in ["$countones", "$onehot", "$onehot0", "$isunknown"] {
        for args in ["", "a, a", ", a", "a,"] {
            let call = format!("{name}({args})");
            let source =
                format!("module Top(input logic a, output int y); assign y = {call}; endmodule");
            let result = Simulator::from_sv_sources(
                vec![(&source, Path::new("invalid_countones.sv"))],
                "Top",
            )
            .build_cranelift();
            assert!(result.is_err(), "invalid call should be rejected: {call}");
        }
    }
}

fn build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("system_functions.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("design unexpectedly compiled:\n{source}"),
        // The rendered diagnostic wraps long messages behind `│` gutters.
        Err(error) => error
            .to_string()
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[test]
fn rejects_unknown_system_functions_in_every_context() {
    let cases = [
        (
            "always_comb statement",
            "module Top(input logic a, output logic y);
                always_comb begin $foo_bar(a); y = a; end
            endmodule",
        ),
        (
            "always_ff statement",
            "module Top(input logic clk, a, output logic q);
                always_ff @(posedge clk) begin $foo_bar(a); q <= a; end
            endmodule",
        ),
        (
            "initial statement",
            "module Top(output logic y);
                initial $foo_bar(1);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "continuous assignment",
            "module Top(input logic a, output logic y); assign y = $foo_bar(a); endmodule",
        ),
        (
            "unused localparam",
            "module Top(output logic y);
                localparam int P = $foo_bar(5);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "constant function",
            "module Top(output int y);
                function automatic int f(input int x);
                    $foo_bar(x);
                    return x + 1;
                endfunction
                localparam int P = f(5);
                assign y = P;
            endmodule",
        ),
        (
            "generate condition",
            "module Top(output logic y);
                if ($foo_bar(1)) begin : g assign y = 1'b1; end
                else begin : h assign y = 1'b0; end
            endmodule",
        ),
        (
            "packed range",
            "module Top(output logic [$foo_bar(3):0] y); assign y = '0; endmodule",
        ),
    ];
    for (context, source) in cases {
        let error = build_error(source);
        assert!(
            error.contains("unknown system task or function `$foo_bar`"),
            "{context}: {error}"
        );
    }
}

#[test]
fn reports_where_a_known_system_function_is_not_supported() {
    let cases = [
        (
            "system function `$time` in an expression",
            "module Top(output logic [63:0] y); assign y = $time; endmodule",
        ),
        (
            "system function `$sqrt` in an expression",
            "module Top(output logic y);
                localparam int P = $sqrt(4);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "system function `$fopen`",
            r#"module Top(input logic a, output logic y);
                always_comb begin $fopen("x.txt"); y = a; end
            endmodule"#,
        ),
        (
            "system task `$display` inside an initial block",
            r#"module Top(output logic y);
                initial $display("x");
                assign y = 1'b0;
            endmodule"#,
        ),
        (
            "system function `$left`",
            "module Top(input logic [3:0] a, output logic y);
                always_comb begin $left(a); y = a[0]; end
            endmodule",
        ),
        (
            "system function `$left` in an expression",
            "module Top(input logic [3:0] a, output int y); assign y = $left(a); endmodule",
        ),
    ];
    for (expected, source) in cases {
        let error = build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn rejects_system_function_calls_with_invalid_arguments() {
    let cases = [
        (
            "invalid call of `$countones`: expected 1 argument, found 2",
            "module Top(input logic a, output logic y);
                always_comb begin $countones(a, a); y = a; end
            endmodule",
        ),
        (
            "invalid call of `$clog2`: expected 1 argument, found 0",
            "module Top(output int y); assign y = $clog2(); endmodule",
        ),
        (
            "invalid call of `$readmemh`: expected 2 to 4 arguments, found 5",
            r#"module Top(output logic [7:0] y);
                logic [7:0] mem [0:3];
                initial $readmemh("m.hex", mem, 0, 3, 99);
                assign y = mem[0];
            endmodule"#,
        ),
        (
            "invalid call of `$display`: a system task has no value",
            r#"module Top(output logic y); assign y = $display("x"); endmodule"#,
        ),
    ];
    for (expected, source) in cases {
        let error = build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn value_functions_called_as_statements_check_their_operands() {
    for source in [
        "module Top(input logic a, output logic y);
            always_comb begin $countones(zzz); y = a; end
        endmodule",
        "module Top(input logic a, output logic y);
            always_comb begin $bits(no_such_signal); y = a; end
        endmodule",
    ] {
        build_error(source);
    }
}

#[test]
fn value_functions_called_as_statements_evaluate_their_operands_once() {
    let source = r#"
        module Top(input logic [3:0] a, output logic y);
            function automatic logic [3:0] g(input logic [3:0] v);
                $display("g %0d", v);
                return v;
            endfunction
            always_comb begin
                $countones(g(a));
                y = a[0];
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("eval.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    sim.drain_runtime_events();
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "g 5".to_string(),
        }],
    );
}
