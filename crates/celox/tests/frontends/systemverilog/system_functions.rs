use super::*;

sv_backends! {
    fn dimensions_of_nonarray_handle_types(sim) {
        @case "system_functions::dimensions_of_nonarray_handle_types";
    }

    fn dimensions_is_independent_of_unknown_values(sim) {
        @case "system_functions::dimensions_is_independent_of_unknown_values";
    }

    fn dimensions_counts_declared_arrays(sim) {
        @case "system_functions::dimensions_counts_declared_arrays";
    }

    fn dimensions_preserves_selected_and_expression_types(sim) {
        @case "system_functions::dimensions_preserves_selected_and_expression_types";
    }

    fn dimensions_resolves_types_and_constant_scopes(sim) {
        @case "system_functions::dimensions_resolves_types_and_constant_scopes";
    }

    fn array_query_evaluates_dimension_function_once(sim) {
        @setup {
            let source = r#"
                module Top(input int d, output int y);
                    logic [7:4] mem [2:5];
                    function automatic int dimension();
                        $display("dimension");
                        return d;
                    endfunction
                    assign y = $left(mem, dimension());
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("array_query.sv"))], "Top");
        let d = sim.signal("d");
        let y = sim.signal("y");
        for (dimension, expected) in [(1u32, 2u32), (2, 7), (3, 0), (0x0800_0001, 0)] {
            sim.set(d, dimension);
            assert_eq!(sim.get(y), expected.into());
            assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::Display {
                message: "dimension".to_string(),
            }]);
        }
    }
    fn readmem_descending_ranges_and_absolute_addresses(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let hex = dir.path().join("data.hex");
            let bin = dir.path().join("data.bin");
            std::fs::write(&hex, "aa @2 bb cc").unwrap();
            std::fs::write(&bin, "1010 1011 1100").unwrap();
            let source = format!(r#"
                module Top(input logic clk, output logic [31:0] initial_words, registered);
                    logic [7:0] a [3:0];
                    logic [7:0] b [1:4];
                    initial $readmemh("{}", a, 3, 1);
                    assign initial_words = {{a[3], a[2], a[1], 8'h00}};
                    always_ff @(posedge clk) begin
                        $readmemb("{}", b, 4, 2);
                        registered <= {{b[4], b[3], b[2], 8'h00}};
                    end
                endmodule
            "#, hex.display(), bin.display());
        }
        @build Simulator::from_sv_sources(vec![(&source, Path::new("readmem.sv"))], "Top");
        let initial_words = sim.signal("initial_words");
        let registered = sim.signal("registered");
        let clk = sim.event("clk");
        assert_eq!(sim.get(initial_words), 0xaabbcc00u32.into());
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(registered), 0x0a0b0c00u32.into());
    }

    fn readmem_count_mismatch_preserves_unwritten_words(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let short = dir.path().join("short.hex");
            let long = dir.path().join("long.hex");
            std::fs::write(&short, "aa").unwrap();
            std::fs::write(&long, "bb cc dd").unwrap();
            let source = format!(r#"
                module Top(output logic [31:0] y, short_y);
                    logic [7:0] mem [-1:2];
                    logic [7:0] short_mem [0:3];
                    always_comb begin
                        mem[-1] = 8'h11; mem[0] = 8'h22;
                        mem[1] = 8'h33; mem[2] = 8'h44;
                        short_mem[0] = 8'h11; short_mem[1] = 8'h22;
                        short_mem[2] = 8'h33; short_mem[3] = 8'h44;
                        $readmemh("{}", short_mem, 3, 2);
                        $readmemh("{}", mem, -1, 0);
                        y = {{mem[-1], mem[0], mem[1], mem[2]}};
                        short_y = {{short_mem[0], short_mem[1], short_mem[2], short_mem[3]}};
                    end
                endmodule
            "#, short.display(), long.display());
        }
        @build Simulator::from_sv_sources(vec![(&source, Path::new("readmem.sv"))], "Top");
        let y = sim.signal("y");
        let short_y = sim.signal("short_y");
        assert_eq!(sim.get(y), 0xbbcc3344u32.into());
        assert_eq!(sim.get(short_y), 0x112233aau32.into());
    }
    fn signed_casts_in_parameters(sim) { @case "system_functions::signed_casts_in_parameters"; }
    fn array_queries_preserve_bounds_and_dimensions(sim) { @case "system_functions::array_queries_preserve_bounds_and_dimensions"; }
    fn array_queries_handle_runtime_dimensions(sim) { @case "system_functions::array_queries_handle_runtime_dimensions"; }
    fn immediate_cover_handles_unknown_conditions(sim) { @case "system_functions::immediate_cover_handles_unknown_conditions"; }
    fn immediate_cover_executes_pass_statement(sim) { @case "system_functions::immediate_cover_executes_pass_statement"; }

    fn drain_runtime_events_returns_always_comb_fatal(sim) {
        @setup {
            let source = r#"
                module Top(input logic fail, output logic y);
                    always_comb begin
                        y = fail;
                        $display("before");
                        if (fail) $fatal(1, "boom %0d", fail);
                        $display("after");
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("fatal.sv"))], "Top");

        let fail = sim.signal("fail");
        sim.drain_runtime_events();
        sim.set(fail, 1u8);
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: "before".to_string() },
            celox::RuntimeEvent::AssertFatal { message: "boom 1".to_string() },
        ]);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn display_arguments_are_sized_as_ieee_specifies(sim) {
        @case "system_functions::display_arguments_are_sized_as_ieee_specifies";
    }

    fn display_field_widths_expand_to_the_value(sim) {
        @case "system_functions::display_field_widths_expand_to_the_value";
    }

    fn display_unknown_bits_as_ieee_specifies(sim) {
        @case "system_functions::display_unknown_bits_as_ieee_specifies";
    }

    fn display_tasks_default_to_their_radix(sim) {
        @case "system_functions::display_tasks_default_to_their_radix";
    }

    fn countbits_in_parameter_specializations_and_generate_scopes(sim) {
        @case "system_functions::countbits_in_parameter_specializations_and_generate_scopes";
    }

    fn countbits_preserves_argument_and_return_types(sim) {
        @case "system_functions::countbits_preserves_argument_and_return_types";
    }
    fn countbits_in_constant_expressions(sim) {
        @case "system_functions::countbits_in_constant_expressions";
    }
    fn countbits_matches_four_states_and_variable_controls(sim) {
        @case "system_functions::countbits_matches_four_states_and_variable_controls";
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
    for name in [
        "$countones",
        "$onehot",
        "$onehot0",
        "$isunknown",
        "$dimensions",
    ] {
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
fn readmem_diagnostics_identify_task_and_requested_range() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("invalid.mem");
    for name in ["$readmemh", "$readmemb"] {
        for (content, expected) in [
            ("@4", "requested range [3:2]"),
            ("g", "invalid data token g"),
        ] {
            std::fs::write(&file, content).unwrap();
            let source = format!(
                r#"module Top(output logic [7:0] y);
                logic [7:0] mem [0:3]; initial {name}("{}", mem, 3, 2);
                assign y = mem[2]; endmodule"#,
                file.display()
            );
            let error = build_error(&source);
            assert!(error.contains(name), "{error}");
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("destination depth"), "{error}");
        }
    }
}

#[test]
fn rejects_stop_and_deferred_cover_instead_of_discarding_their_semantics() {
    for body in [
        "initial $stop;",
        "always_comb begin $stop(0); y = 0; end",
        "always_ff @(posedge clk) begin $stop; y <= 0; end",
        "function automatic int f(); $stop; return 1; endfunction initial y = f();",
    ] {
        let source = format!("module Top(input logic clk, output logic y); {body} endmodule");
        assert!(build_error(&source).contains("system task `$stop`"));
    }
    for timing in ["#0", "final"] {
        let source =
            format!("module Top(output logic y); initial cover {timing} (1) y = 1; endmodule");
        assert!(build_error(&source).contains("deferred immediate cover"));
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
            "system task `$assert` inside an initial block",
            "module Top(output logic y);
                initial $assert(1'b1);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "system task `$stop`",
            "module Top(output logic y); initial $stop; assign y = 0; endmodule",
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
        "module Top(input logic a, output logic y);
            always_comb begin $dimensions(no_such_signal); y = a; end
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

fn initial_simulation(source: &str) -> celox::Simulation {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("initial.sv"))], "Top")
        .build()
        .unwrap()
}

#[test]
fn initial_blocks_run_their_system_tasks_at_time_zero() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic [15:0] a = 16'h1234;
            logic [7:0] b;
            initial begin
                b = a[0+:8];
                $display("b=%h", b);
                for (int i = 0; i < 2; i++) $write("%0d,", i);
                if (b == 8'h35) $display("unreachable");
                $error("e%0d", b - 8'h30);
            end
            assign y = b;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let y = sim.signal("y");
    // The block runs as a process at time zero, not before it.
    assert_eq!(sim.get(y), 0u8.into());
    assert_eq!(sim.drain_runtime_events(), Vec::new());
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(y), 0x34u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "b=34".to_string(),
            },
            celox::RuntimeEvent::Write {
                message: "0,".to_string(),
            },
            celox::RuntimeEvent::Write {
                message: "1,".to_string(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: "e4".to_string(),
            },
        ],
    );
    assert!(!sim.is_finished());
    assert_eq!(sim.step().unwrap(), None);
}

#[test]
fn finish_in_an_initial_block_ends_the_simulation() {
    let source = r#"
        module Top(output logic y);
            initial begin
                $display("before");
                $finish;
                $display("after");
            end
            assign y = 1'b0;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    assert!(sim.is_finished());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "before".to_string(),
        }],
    );
}

#[test]
fn fatal_in_an_initial_block_fails_the_simulation() {
    let source = r#"
        module Top(output logic y);
            initial begin
                $display("before");
                $fatal(1, "boom %0d", 7);
                $display("after");
            end
            assign y = 1'b0;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let error = sim.step().unwrap_err();
    assert!(error.to_string().contains("boom"), "{error}");
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "before".to_string(),
            },
            celox::RuntimeEvent::AssertFatal {
                message: "boom 7".to_string(),
            },
        ],
    );
}

#[test]
fn initial_blocks_skip_the_operands_short_circuits_skip() {
    let source = r#"
        module Top(input logic a, output logic y);
            logic d;
            function automatic logic f(input logic v);
                $display("called");
                return v;
            endfunction
            initial begin
                d = 1'b1 || f(a);
                $display("%0d", d);
            end
            assign y = d;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "1".to_string(),
        }],
    );
}

#[test]
fn initial_blocks_that_read_design_state_run_at_time_zero() {
    let source = r#"
        module Top(input logic [3:0] a, output logic [3:0] y);
            logic [3:0] value = a + 4'd1;
            initial begin
                $display("value=%0d", value);
                if (a == 4'd0) $display("zero");
            end
            assign y = value;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let y = sim.signal("y");
    sim.step().unwrap();
    // The initializer runs before the initial block.
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "value=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "zero".to_string(),
            },
        ],
    );
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn rejects_nonblocking_assignments_in_initial_processes() {
    let error = build_error(
        r#"module Top(output logic y);
            logic v;
            initial begin v <= 1'b1; $display("%0d", v); end
            assign y = v;
        endmodule"#,
    );
    assert!(
        error.contains("nonblocking assignment in an initial block that runs as a process"),
        "{error}"
    );
}

#[test]
fn rejects_countbits_with_missing_or_omitted_arguments() {
    for args in ["", "a", "a,", ", a", "a, '1,", "a,, '1"] {
        let source = format!(
            "module Top(input logic a, output int y); assign y = $countbits({args}); endmodule"
        );
        let error = build_error(&source);
        assert!(error.contains("$countbits"), "{args}: {error}");
    }
}

#[test]
fn countbits_called_as_a_statement_evaluates_all_arguments_once() {
    let source = r#"
        module Top(input logic [3:0] a, output logic y);
            function automatic logic [3:0] value(input logic [3:0] v);
                $display("value %0d", v);
                return v;
            endfunction
            function automatic logic control(input logic v);
                $display("control %0d", v);
                return v;
            endfunction
            always_comb begin
                $countbits(value(a), control(a[0]), control(a[1]));
                y = a[0];
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("countbits_statement.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    sim.drain_runtime_events();
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "value 5".to_string()
            },
            celox::RuntimeEvent::Display {
                message: "control 1".to_string()
            },
            celox::RuntimeEvent::Display {
                message: "control 0".to_string()
            },
        ]
    );
}

#[test]
fn dimensions_rejects_undefined_constant_operands() {
    for source in [
        "module Top(output int y); localparam N = $dimensions(missing); assign y = 0; endmodule",
        "module Top(output int y); if ($dimensions(missing) == 1) assign y = 1; else assign y = 0; endmodule",
    ] {
        build_error(source);
    }
}
