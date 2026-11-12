use super::*;

sv_backends! {
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
    // `%m` names the subroutine that calls the task (IEEE 1800-2023
    // 21.2.1.5), wherever it is called from: a package subroutine or class
    // method by its package and class (a specialization with its parameter
    // values, as `$typename` names it), one of the compilation unit by
    // `$unit`, a module subroutine under the instance that declares it.
    fn scope_name_of_subroutines(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                package pkg;
                    function automatic string where();
                        string s;
                        $swrite(s, "%m");
                        return s;
                    endfunction
                    function automatic void show();
                        $display("[%m] %s", $sformatf("<%m>"));
                    endfunction
                    class C;
                        function string where();
                            string s;
                            $swrite(s, "%m");
                            return s;
                        endfunction
                        static task show();
                            $display("[%M %%m]");
                        endtask
                    endclass
                    class P #(int N = 1);
                        function string where();
                            return $sformatf("%m");
                        endfunction
                    endclass
                endpackage
                class U;
                    function string where();
                        return $sformatf("%m");
                    endfunction
                endclass
                function automatic string unit_where();
                    return $sformatf("%m");
                endfunction
                module Child;
                    task t();
                        string s;
                        $swrite(s, "%m");
                        $display("[%s]", s);
                    endtask
                    initial t();
                endmodule
                module Top;
                    pkg::C c;
                    pkg::P #(3) p;
                    U u;
                    Child first();
                    initial begin
                        #1;
                        $display("[%s]", pkg::where());
                        pkg::show();
                        c = new;
                        $display("[%s]", c.where());
                        pkg::C::show();
                        $display("[%s]", unit_where());
                        p = new;
                        $display("[%s]", p.where());
                        u = new;
                        $display("[%s]", u.where());
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("subroutine_scope.sv"))], "Top");
        sim.run_until(1).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"[Top.first.t]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[pkg::where]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[pkg::show] <pkg::show>".to_vec() },
            celox::RuntimeEvent::Display { message: b"[pkg::C::where]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[pkg::C::show %m]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[$unit::unit_where]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[pkg::P#(3)::where]".to_vec() },
            celox::RuntimeEvent::Display { message: b"[$unit::U::where]".to_vec() },
        ]);
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
        (
            "typedef enum bit {A, B} e_t; e_t [1:0] v;",
            "%p of an enum value",
        ),
        (
            "typedef struct packed { bit [3:0] a; } s_t; s_t v;",
            "%p of a structure or unpacked array",
        ),
        (
            "class C; endclass int v[C];",
            "%p of an associative array with class handle keys",
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
        ("typedef struct { int n; string s; } r_t; r_t q[$];", "q[0]"),
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
    // An enum value is printed by name through more than one evaluation.
    for (declarations, value) in [
        ("typedef enum {A, B} e_t; e_t q[$];", "q.pop_front()"),
        (
            "typedef enum {A, B} e_t; function automatic e_t pick(); return B; endfunction",
            "pick()",
        ),
        (
            "typedef enum {A, B} e_t; typedef struct { e_t e; } s_t; \
             function automatic e_t pick(); return B; endfunction",
            "s_t'{pick()}",
        ),
    ] {
        check_rejected(
            declarations,
            value,
            "%p of an enum value of a call with effects",
        );
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

// `$sformatf` of constant expressions is a constant system function call
// (IEEE 1800-2023 11.2.1): it gives parameters and parameter overrides
// their values, here the instance names a design derives level by level.
#[test]
fn sformatf_of_constants_is_a_constant_expression() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf #(parameter string ID = "", parameter int N = 0) ();
            localparam string TAG = $sformatf("%s#%0d", ID, N);
            initial #1 $display("%s|%s", ID, TAG);
        endmodule
        module Mid #(parameter string ID = "") ();
            Leaf #(.ID($sformatf("%s-alu", ID)), .N(1)) alu();
            for (genvar i = 0; i < 2; ++i) begin : g
                Leaf #(.ID($sformatf("%s-socket%0d", ID, i)), .N(i + 10)) socket();
            end
        endmodule
        module Top;
            localparam int WIDTH = 12;
            localparam string NAME = $sformatf("gbar%0d", WIDTH);
            localparam string RADIX = $sformatf(
                "%h/%0h/%d/%0d/%b/%o/%c%%", 12'h02a, 12'h02a, 8'd7, -8'sd7, 4'd5, 6'd9, 8'h41);
            // A value wider than its characters is right-justified in its
            // width; a string holds no NUL byte.
            localparam string PADDED = $sformatf("[%s][%0s][%c]", 24'h004142, 24'h004142, 8'd0);
            Mid #(.ID("core0")) core();
            Mid #(.ID(NAME)) bar();
            string padded, unpadded;
            logic [23:0] characters = 24'h004142;
            initial begin
                $display("%s", RADIX);
                // The same calls at run time.
                padded = $sformatf("[%s][%0s][%c]", characters, characters, 8'd0);
                unpadded = PADDED;
                $display("%s %0d", PADDED, padded == unpadded);
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let mut displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed.remove(0), "02a/2a/  7/-7/0101/11/A%");
    assert_eq!(displayed.remove(0), "[ AB][AB][] 1");
    // The instances display in no specified order.
    displayed.sort();
    assert_eq!(
        displayed,
        [
            "core0-alu|core0-alu#1",
            "core0-socket0|core0-socket0#10",
            "core0-socket1|core0-socket1#11",
            "gbar12-alu|gbar12-alu#1",
            "gbar12-socket0|gbar12-socket0#10",
            "gbar12-socket1|gbar12-socket1#11",
        ]
    );
}

// A string parameter longer than an `i128` (16 bytes) still gives a
// constant `$sformatf` override its value.
#[test]
fn sformatf_overrides_use_long_string_parameters() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf #(parameter string ID = "") ();
            initial #1 $display("%s", ID);
        endmodule
        module Mid #(parameter string ID = "") ();
            Leaf #(.ID($sformatf("%s-alu-unit", ID))) alu();
        endmodule
        module Top;
            Mid #(.ID("a-very-long-instance-name")) core();
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["a-very-long-instance-name-alu-unit"]);
}

// A NUL byte in the format value is removed before the directives are
// parsed, as the runtime formatter removes it: `%\0d` is `%d`, with its
// default field width, not `%0d`.
#[test]
fn sformatf_of_constants_ignores_nul_bytes_in_the_format() {
    let source = r#"
        module Leaf #(parameter string ID = "") ();
            initial $display("[%s]", ID);
        endmodule
        module Top;
            Leaf #(.ID($sformatf(24'h250064, 8'd7))) leaf();
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["[  7]"]);
}

// A string parameter that takes its value from an earlier wider string
// parameter still gives a constant `$sformatf` override its value.
#[test]
fn sformatf_overrides_use_derived_long_string_parameters() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf #(parameter string ID = "") ();
            initial #1 $display("%s", ID);
        endmodule
        module Mid #(parameter string ID = "",
                     parameter string TAG = ID) ();
            Leaf #(.ID($sformatf("%s-alu", TAG))) alu();
        endmodule
        module Top;
            Mid #(.ID("a-very-long-instance-name")) core();
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["a-very-long-instance-name-alu"]);
}

// A constant string-valued query is a constant `%s` operand: the packed
// bytes of `$typename(...)` format as the string it names.
#[test]
fn sformatf_of_constants_formats_a_typename_operand() {
    let source = r#"
        module Leaf #(parameter string ID = "") ();
            initial $display("%s", ID);
        endmodule
        module Top;
            Leaf #(.ID($sformatf("%s", $typename(byte unsigned)))) leaf();
            initial $display("%s", $sformatf("%s", $typename(byte unsigned)));
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed.len(), 2);
    assert_eq!(displayed[0], displayed[1]);
    assert!(displayed[0].contains("byte"), "{displayed:?}");
}

// A conversion of an operand too wide to fit the result is not folded;
// it must decline without formatting the whole width first.
#[test]
fn sformatf_of_a_huge_operand_is_not_folded() {
    let source = r#"
        module Top;
            localparam string NAME = $sformatf("%b", 100000000'b0);
            initial #1 $display("%s", NAME);
        endmodule
    "#;
    assert!(
        celox::Simulation::from_sv_sources(vec![(source, Path::new("constant.sv"))], "Top")
            .build_cranelift()
            .is_err()
    );
}
