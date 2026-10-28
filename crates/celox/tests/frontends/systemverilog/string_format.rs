use super::*;

sv_backends! {
    fn sformatf_matches_display(sim) { @case "string_format::sformatf_matches_display"; }
    fn sformatf_four_state_values(sim) { @case "string_format::sformatf_four_state_values"; }
    fn sformatf_in_expressions(sim) { @case "string_format::sformatf_in_expressions"; }
    fn sformat_property_output(sim) { @case "string_format::sformat_property_output"; }
    fn sformatf_runtime_formats(sim) { @case "string_format::sformatf_runtime_formats"; }
    fn sformatf_packed_runtime_format(sim) { @case "string_format::sformatf_packed_runtime_format"; }
    fn sformatf_in_conditional_expressions(sim) { @case "string_format::sformatf_in_conditional_expressions"; }
    fn sformatf_time_values(sim) { @case "string_format::sformatf_time_values"; }
    fn sformatf_in_omitted_default(sim) { @case "string_format::sformatf_in_omitted_default"; }
    fn sformatf_runtime_format_bytes(sim) { @case "string_format::sformatf_runtime_format_bytes"; }
    fn sformatf_after_wider_input(sim) { @case "string_format::sformatf_after_wider_input"; }
    fn sformatf_in_container_operands(sim) { @case "string_format::sformatf_in_container_operands"; }
    fn sformatf_with_associative_index(sim) { @case "string_format::sformatf_with_associative_index"; }
    fn sformatf_with_reference_argument(sim) { @case "string_format::sformatf_with_reference_argument"; }
    fn swrite_matches_write(sim) { @case "string_format::swrite_matches_write"; }
    fn display_later_formats(sim) { @case "string_format::display_later_formats"; }
    // External testbenches wrap the design, so the hierarchical name `%m`
    // prints differs between simulators; Celox prints the same for both.
    fn sformatf_scope_matches_display(sim) {
        @setup {
            let source = r#"
                module Child;
                    string s;
                    initial begin
                        $display("[%m]");
                        s = $sformatf("[%m]");
                        $display("%s", s);
                    end
                endmodule
                module Top; Child first(); endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("format_scope.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"[Top.first]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[Top.first]".to_vec() },
        ]);
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn sformatf_native_image() {
    for case in [
        "string_format::sformatf_in_expressions",
        "string_format::sformatf_runtime_formats",
    ] {
        crate::suite::run_case(case, "native-image");
    }
}

/// A property's owner is captured before the operands of a container method
/// run, as without a formatting operand. IEEE 1800-2023 leaves this order to
/// the implementation, so the case is Celox's own contract.
#[test]
fn container_owner_is_captured_before_formatting_operands() {
    let source = r#"
        class Log; string lines[$]; endclass
        module Top(output int first_size, other_size);
            Log log, first, other;
            function automatic string retarget();
                log = other;
                return $sformatf("r%0d", 1);
            endfunction
            initial begin
                first = new;
                other = new;
                log = first;
                log.lines.push_back(retarget());
                first_size = first.lines.size();
                other_size = other.lines.size();
            end
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("owner.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    let first = sim.signal("first_size");
    let other = sim.signal("other_size");
    assert_eq!(sim.get(first), BigUint::from(1u8));
    assert_eq!(sim.get(other), BigUint::from(0u8));
}

/// A value without a conversion of its own keeps its default conversion when a
/// later format literal is folded into the format: block-local strings and
/// reals print as they do without one (IEEE 1800-2023 21.2.1.1).
#[test]
fn later_formats_keep_default_conversions_of_locals() {
    let source = r#"
        module Top;
            initial begin
                string s;
                real r;
                s = "text";
                r = 1.5;
                $write(s, r, "\n");
                $write("", s, r, "", "\n");
            end
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("locals.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    let messages: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .map(|event| match event {
            celox::RuntimeEvent::Write { message } => message,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], messages[1]);
    assert!(
        messages[0].starts_with(b"text"),
        "{:?}",
        String::from_utf8_lossy(&messages[0])
    );
}
