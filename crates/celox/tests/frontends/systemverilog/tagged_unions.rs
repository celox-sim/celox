use super::*;

sv_backends! {
    fn default_arguments(sim) {
        @case "tagged_unions::default_arguments";
    }
    fn factory_and_union_arguments(sim) {
        @case "tagged_unions::factory_and_union_arguments";
    }
    fn writable_task_arguments(sim) {
        @case "tagged_unions::writable_task_arguments";
    }
    fn writable_system_arguments(sim) {
        @case "tagged_unions::writable_system_arguments";
    }
    fn conditional_struct_payload(sim) {
        @case "tagged_unions::conditional_struct_payload";
    }
    fn constructors_and_checked_members(sim) {
        @case "tagged_unions::constructors_and_checked_members";
    }
    fn nested_and_void_constructors(sim) {
        @case "tagged_unions::nested_and_void_constructors";
    }
}

#[test]
fn inactive_member_read_and_write_report_runtime_errors() {
    for statement in ["y = item.Full;", "item.Full = 8'hff;"] {
        let source = format!(
            r#"
            module Top(output logic [7:0] y);
                union tagged packed {{ void Empty; logic [7:0] Full; }} item;
                initial begin
                    item = tagged Empty;
                    {statement}
                end
            endmodule
        "#
        );
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(&source, std::path::Path::new("tagged_error.sv"))],
            "Top",
        )
        .build()
        .unwrap();
        let error = sim.step().unwrap_err();
        assert!(error.to_string().contains("tagged union member"), "{error}");
    }
}

#[test]
fn rejects_invalid_tagged_constructors() {
    for rhs in [
        "tagged Missing 1",
        "tagged Empty 1",
        "tagged Full",
        "8'hff",
        "other",
    ] {
        let source = format!(
            r#"
            module Top;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} first_t;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} second_t;
                first_t item;
                second_t other;
                initial item = {rhs};
            endmodule
        "#
        );
        assert!(
            celox_sv_analyzer::analyze_source(&source, std::path::Path::new("tagged_invalid.sv"))
                .is_err(),
            "{rhs}"
        );
    }
}

sv_backends! {
    fn casts_ports_and_registers(sim) {
        @case "tagged_unions::casts_ports_and_registers";
    }
    fn wide_four_state_payload(sim) {
        @case "tagged_unions::wide_four_state_payload";
    }
    fn inactive_read_fails_on_clock(sim) {
        @setup {
            let source = r#"
                module Top(input bit clk, output logic [7:0] y);
                    union tagged packed { void Empty; logic [7:0] Full; } item;
                    always_ff @(posedge clk) begin
                        item = tagged Empty;
                        y <= item.Full;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("tagged_read.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        assert!(sim.tick(clk).is_err());
        assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::AssertFatal {
            message: b"tagged union member does not match the active tag".to_vec(),
        }]);
    }
    fn inactive_write_fails_on_clock(sim) {
        @setup {
            let source = r#"
                module Top(input bit clk, output logic [7:0] y);
                    union tagged packed { void Empty; logic [7:0] Full; } item;
                    always_ff @(posedge clk) begin
                        item = tagged Empty;
                        item.Full = 8'hff;
                        y <= 0;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("tagged_write.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        assert!(sim.tick(clk).is_err());
        assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::AssertFatal {
            message: b"tagged union member does not match the active tag".to_vec(),
        }]);
    }
    fn uninitialized_tag_fails_on_clock(sim) {
        @setup {
            let source = r#"
                module Top(input bit clk, output logic [7:0] y);
                    union tagged packed { void Empty; logic [7:0] Full; } item;
                    always_ff @(posedge clk) y <= item.Full;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("tagged_unknown.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        assert!(sim.tick(clk).is_err());
        assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::AssertFatal {
            message: b"tagged union member does not match the active tag".to_vec(),
        }]);
    }
}

#[test]
fn rejects_writes_that_bypass_the_tag() {
    for statement in [
        "item[8] = 1;",
        "item += 1;",
        "item++;",
        "{item} = 9'h100;",
        "{item.Full} = 8'hff;",
        "assign item.Full = 8'hff;",
    ] {
        let source = format!(
            "module Top; union tagged packed {{ void Empty; logic [7:0] Full; }} item; {} endmodule",
            if statement.starts_with("assign") {
                statement.to_owned()
            } else {
                format!("initial begin {statement} end")
            }
        );
        assert!(
            celox_sv_analyzer::analyze_source(&source, Path::new("tagged_write.sv")).is_err(),
            "{statement}"
        );
    }
}

#[test]
fn ports_reject_fabricated_tags_and_different_union_types() {
    for (direction, actual_type, expr) in [
        ("input", "logic [8:0]", "value"),
        ("output", "logic [8:0]", "value"),
        ("input", "Types::second_t", "value"),
        ("input", "Types::first_t", "9'h100"),
    ] {
        let source = format!(
            r#"
            package Types;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} first_t;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} second_t;
            endpackage
            module Child({direction} Types::first_t item); endmodule
            module Top;
                {actual_type} value;
                Child child(.item({expr}));
            endmodule
        "#
        );
        let result = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("tagged_ports_invalid.sv"))],
            "Top",
        )
        .build();
        let error = match result {
            Ok(_) => panic!("incompatible tagged union port must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires a signal"), "{error}");
    }
}

#[test]
fn writable_arguments_check_active_tags_and_recheck_copyout() {
    for (setup, body, call) in [
        (
            "item = tagged Empty;",
            "value = 42;",
            "set_value(item.Full);",
        ),
        (
            "item = tagged Empty;",
            "value += 1;",
            "increment(item.Full);",
        ),
        (
            "item = tagged Full 0;",
            "item = tagged Empty; value = 42;",
            "set_value(item.Full);",
        ),
        (
            "item = tagged Empty;",
            "value = 42;",
            "count = $sscanf(\"42\", \"%d\", item.Full);",
        ),
        (
            "item = tagged Full 123;",
            "value = 42;",
            "count = $dist_uniform(item.Full, change_tag(), 10);",
        ),
        (
            "item = tagged Full 0; other = tagged Empty;",
            "value = 42;",
            "set_value({item.Full[3:0], other.Full[3:0]});",
        ),
    ] {
        let source = format!(
            r#"
            module Top(output logic [7:0] y);
                union tagged packed {{ void Empty; logic [7:0] Full; }} item, other;
                int count;
                function automatic int change_tag(); item = tagged Empty; return 1; endfunction
                task automatic set_value(output logic [7:0] value); {body} endtask
                task automatic increment(inout logic [7:0] value); {body} endtask
                initial begin {setup} {call} y = 0; end
            endmodule
        "#
        );
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("tagged_argument_error.sv"))],
            "Top",
        )
        .build()
        .unwrap();
        let error = sim.step().unwrap_err();
        assert!(error.to_string().contains("tagged union member"), "{error}");
    }
}

#[test]
fn copyout_rejects_fabricated_union_tags() {
    for direction in ["output", "inout"] {
        for actual in ["item", "{item}"] {
            let source = format!(
                r#"
                module Top;
                    union tagged packed {{ void Empty; logic [7:0] Full; }} item;
                    task automatic fill({direction} logic [8:0] value); value = 9'h100; endtask
                    initial begin item = tagged Empty; fill({actual}); end
                endmodule
            "#
            );
            assert!(
                celox_sv_analyzer::analyze_source(&source, Path::new("tagged_copyout.sv")).is_err(),
                "{direction} {actual}"
            );
        }
    }
}

#[test]
fn unused_index_argument_preserves_tag_check() {
    let source = r#"
        module Top(output logic y);
            union tagged packed { void Empty; logic [7:0] Full; } item;
            logic [7:0] data;
            function automatic int zero(input logic [7:0] value); return 0; endfunction
            initial begin
                item = tagged Empty;
                data = 8'hff;
                y = data[zero(item.Full)];
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("tagged_index.sv"))], "Top")
            .build()
            .unwrap();
    let error = sim.step().unwrap_err();
    assert!(error.to_string().contains("tagged union member"), "{error}");
}

#[test]
fn rejects_integral_system_copyout_and_invalid_factory_returns() {
    for statement in [
        "count = $sscanf(\"256\", \"%d\", item);",
        "count = $random(item);",
        "count = $fread(item, 0);",
    ] {
        let source = format!(
            "module Top; union tagged packed {{ void Empty; logic [7:0] Full; }} item; int count; initial begin {statement} end endmodule"
        );
        assert!(
            celox_sv_analyzer::analyze_source(&source, Path::new("tagged_system_copyout.sv"))
                .is_err(),
            "{statement}"
        );
    }
    for value in ["9'h100", "other"] {
        let source = format!(
            r#"
            module Top;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} item_t;
                union tagged packed {{ void Empty; logic [7:0] Full; }} other;
                item_t item;
                function automatic item_t make(); return {value}; endfunction
                initial item = make();
            endmodule
        "#
        );
        assert!(
            celox_sv_analyzer::analyze_source(&source, Path::new("tagged_invalid_return.sv"))
                .is_err(),
            "{value}"
        );
    }
}

#[test]
fn output_port_concatenations_cannot_fabricate_tags() {
    for actual in ["{item}", "{{item}}", "{pad, item}"] {
        let source = format!(
            r#"
            module Child(output logic [9:0] value); assign value = 10'h100; endmodule
            module Top;
                union tagged packed {{ void Empty; logic [7:0] Full; }} item;
                logic pad;
                Child child(.value({actual}));
            endmodule
        "#
        );
        let result = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("tagged_concat_port.sv"))],
            "Top",
        )
        .build();
        assert!(result.is_err(), "{actual}");
    }
}

#[test]
fn rejects_invalid_tagged_argument_defaults() {
    for default in ["9'h100", "tagged Missing 1", "tagged Full"] {
        let source = format!(
            r#"
            module Top;
                typedef union tagged packed {{ void Empty; logic [7:0] Full; }} item_t;
                function automatic logic [7:0] get(input item_t value = {default}); return value.Full; endfunction
                logic [7:0] result;
                initial result = get();
            endmodule
        "#
        );
        assert!(
            celox_sv_analyzer::analyze_source(&source, Path::new("tagged_invalid_default.sv"))
                .is_err(),
            "{default}"
        );
    }
}

#[test]
fn inactive_constant_member_cannot_supply_packed_bound() {
    let source = r#"
module Top(output logic [N-1:0] data);
    typedef union tagged packed { logic [7:0] Empty; logic [7:0] Full; } item_t;
    function automatic int size();
        item_t item;
        item = tagged Empty 8'd4;
        return item.Full;
    endfunction
    localparam int N = size();
    assign data = '1;
endmodule
"#;
    assert!(
        celox::Simulation::from_sv_sources(
            vec![(source, Path::new("tagged_inactive_constant.sv"))],
            "Top",
        )
        .build()
        .is_err()
    );
}
