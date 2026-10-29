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
    fn pattern_format(sim) { @case "string_format::pattern_format"; }
    fn pattern_format_keys_and_handles(sim) { @case "string_format::pattern_format_keys_and_handles"; }
    // A string literal operand leaves a conditional integral (5.9): `%p`
    // prints it unformatted, not in quotes.
    fn pattern_format_conditional_string_literal(sim) {
        @setup {
            let source = r#"
                module Top;
                    bit f;
                    string s;
                    initial begin
                        f = 1;
                        $display("%p", f ? "A" : 8'h42);
                        s = $sformatf("%p", f ? "A" : 8'h42);
                        $display("%s", s);
                        f = 0;
                        $display("%p", f ? "A" : 8'h42);
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("pattern_literal.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"65".to_vec() },
            celox::RuntimeEvent::Display { message: b"65".to_vec() },
            celox::RuntimeEvent::Display { message: b"66".to_vec() },
        ]);
    }
    // An enum operand leaves a conditional integral unless the other operand
    // has the same enumerated type (11.4.11).
    fn pattern_format_mixed_enum_conditional(sim) {
        @setup {
            let source = r#"
                module Top;
                    typedef enum {A, B} e_t;
                    bit f;
                    string s;
                    initial begin
                        f = 1;
                        $display("%p", f ? B : 0);
                        s = $sformatf("%p", f ? B : 7);
                        $display("%s", s);
                        f = 0;
                        $display("%p", f ? B : 7);
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("pattern_enum.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"1".to_vec() },
            celox::RuntimeEvent::Display { message: b"1".to_vec() },
            celox::RuntimeEvent::Display { message: b"7".to_vec() },
        ]);
    }
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

/// `%p` of a type whose pattern Celox does not print yet is rejected, not
/// printed as a plain integral value (IEEE 1800-2023 21.2.1.6).
#[test]
fn pattern_format_rejects_unsupported_types() {
    for (declarations, expected) in [
        ("typedef enum {A, B} e_t; e_t v;", "%p of an enum value"),
        (
            "typedef struct packed { bit [3:0] a; } s_t; s_t v;",
            "%p of a structure or unpacked array",
        ),
        (
            "typedef enum {A, B} e_t; e_t v[$];",
            "%p of a container of enum values",
        ),
        (
            "class C; endclass int v[C];",
            "%p of an associative array with class handle keys",
        ),
        (
            "typedef struct { int n; string s; } r_t; r_t v[$];",
            "%p of a container of aggregate elements",
        ),
    ] {
        check_rejected(declarations, "v", expected);
    }
    for (declarations, value) in [
        ("typedef struct packed { bit [3:0] a; } s_t;", "s_t'(4'h3)"),
        (
            "typedef struct packed { bit [3:0] a; } s_t; s_t q[2];",
            "q[0]",
        ),
        ("int m[2][3];", "m[1]"),
        ("int a[4];", "a[1:2]"),
        ("typedef struct { int n; string s; } r_t; r_t v;", "v"),
        ("typedef struct { int n; string s; } r_t; r_t q[$];", "q[0]"),
        (
            "typedef struct { int n; } r_t; class C; r_t p; endclass C c;",
            "c.p",
        ),
        (
            "typedef struct { int n; } i_t; typedef struct { i_t i; int m; } r_t; r_t v;",
            "v.i",
        ),
        (
            "typedef struct { int n; } i_t; typedef struct { i_t i; int m; } r_t; \
             class C; r_t p; endclass C c;",
            "c.p.i",
        ),
        (
            "typedef struct { int n; } r_t; \
             function automatic r_t make(); r_t r; r.n = 1; return r; endfunction",
            "make()",
        ),
        (
            "typedef struct packed { bit [3:0] a; } s_t; \
             function automatic s_t make(); return s_t'(4'h3); endfunction",
            "make()",
        ),
    ] {
        check_rejected(declarations, value, "%p of a structure or unpacked array");
    }
    for (declarations, value) in [
        ("typedef enum {A, B} e_t; e_t q[$];", "q[0]"),
        ("typedef enum {A, B} e_t; e_t q[$];", "q.pop_front()"),
        ("typedef enum {A, B} e_t; e_t a[2];", "a[1]"),
        ("typedef enum {A, B} e_t; e_t a, b; bit f;", "f ? a : b"),
        ("typedef enum {A, B} e_t;", "A"),
        ("typedef enum {A, B} e_t;", "e_t'(1)"),
        ("typedef enum {A, B} e_t; e_t v;", "v.next()"),
        (
            "typedef enum {A, B} e_t; function automatic e_t pick(); return B; endfunction",
            "pick()",
        ),
    ] {
        check_rejected(declarations, value, "%p of an enum value");
    }
}

/// Building `%p` of `value` fails with `expected`.
fn check_rejected(declarations: &str, value: &str, expected: &str) {
    let source = format!(
        "module Top; {declarations} string s; \
         initial begin $display(\"%p\", {value}); s = $sformatf(\"%p\", {value}); end endmodule"
    );
    let Err(error) =
        Simulator::from_sv_sources(vec![(source.as_str(), Path::new("pattern.sv"))], "Top")
            .build_cranelift()
    else {
        panic!("{declarations}: %p of {value} is unsupported");
    };
    let error = error.to_string();
    assert!(error.contains(expected), "{declarations} {value}: {error}");
}
