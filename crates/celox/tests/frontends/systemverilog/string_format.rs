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
    // `%m` names the named block or fork, nested ones too, and the generate
    // blocks (an unnamed one by its implicit name, numbered among all the
    // generate constructs of its scope, 27.6) that hold the
    // process or subroutine (IEEE 1800-2023 21.2.1.5). The lines are
    // sorted: processes in one time slot run in no defined order.
    fn scope_name_of_named_blocks_and_generate_scopes(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                module c #(parameter int w = 2) ();
                    if (w == 1) begin : fi
                        initial #1 $display("%m one");
                    end else begin : fi2
                        initial #1 $display("%m many");
                        for (genvar k = 0; k < 2; k++) begin : g
                            initial #1 $display("%m k=%0d", k);
                        end
                    end
                    if (1) begin : gb
                        task automatic t(); $display("%m gen-task"); endtask
                        initial #2 t();
                    end
                    if (1) begin
                        initial #2 $display("%m unnamed");
                    end
                endmodule
                module Top;
                    c #(.w(2)) u ();
                    task t2(); begin : tb $display("%m task-block"); end endtask
                    initial begin : ib
                        #3;
                        begin : inner $display("%m nested"); end
                        fork : fk
                            $display("%m fork");
                        join
                        t2();
                        $display("%m after");
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("block_scope.sv"))], "Top");
        sim.run_until(4).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        lines.sort();
        assert_eq!(lines, [
            "Top.ib after",
            "Top.ib.fk fork",
            "Top.ib.inner nested",
            "Top.t2.tb task-block",
            "Top.u.fi2 many",
            "Top.u.fi2.g[0] k=0",
            "Top.u.fi2.g[1] k=1",
            "Top.u.gb.t gen-task",
            "Top.u.genblk3 unnamed",
        ]);
    }
    // `%m` in the processes of an interface instance, and in the subroutines
    // and named blocks they run, names the instance as its parent does: an
    // element of an instance array, one in a generate block (IEEE 1800-2023
    // 21.2.1.5), in literal and run-time formats alike. The lines are sorted.
    fn scope_name_of_interface_instances(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                interface bfm;
                    string fmt = "%m", s;
                    task w(); $display("task %m"); endtask
                    function automatic string f(); return $sformatf("%m"); endfunction
                    initial begin
                        #1;
                        w();
                        $display("initial %m");
                        s = $sformatf(fmt);
                        $display("runtime %s", s);
                        $display("function %s", f());
                        begin : nb $display("block %m"); end
                    end
                endinterface
                module Top;
                    bfm b0();
                    bfm arr[2] ();
                    if (1) begin : g bfm u(); end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("interface_scope.sv"))], "Top");
        sim.run_until(2).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        lines.sort();
        let mut want = Vec::new();
        for instance in ["Top.arr[0]", "Top.arr[1]", "Top.b0", "Top.g.u"] {
            want.push(format!("block {instance}.nb"));
            want.push(format!("function {instance}.f"));
            want.push(format!("initial {instance}"));
            want.push(format!("runtime {instance}"));
            want.push(format!("task {instance}.w"));
        }
        want.sort();
        assert_eq!(lines, want);
    }
    // `%m` in a subroutine that runs in another instance's code names the
    // subroutine's own instance (IEEE 1800-2023 21.2.1.5): a call through a
    // virtual interface, from a module or from class code, with the
    // interface's own functions it calls, run-time formats, file output,
    // severity tasks and assertion reports; a downward and an upward
    // hierarchical call, into generate blocks, arrays and escaped names; a
    // function imported through an interface port, through a port of the
    // parent too (`$printtimescale` there names the instance as well), and
    // the copies a copied subroutine holds; and an override of a class of a
    // module. Reports name the same scope. The lines are sorted: processes
    // in one time slot run in no defined order.
    fn scope_name_of_subroutines_of_other_instances(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                interface bfm;
                    logic x, y;
                    function automatic string f(); return $sformatf("%m f"); endfunction
                    task automatic w();
                        string fmt;
                        fmt = "%m rt";
                        $display("%m w");
                        $display("%s", f());
                        $display("%s", $sformatf(fmt));
                        $fdisplay(32'h8000_0001, "%m fd");
                        $error("%m error");
                        assert (x);
                        assert (x) else $warning("%m action");
                    endtask
                    function automatic string pf(); $printtimescale; return $sformatf("%m pf"); endfunction
                    modport mp(input x, import pf);
                endinterface
                package p;
                    virtual class B;
                        pure virtual function void f();
                    endclass
                    B obj[$];
                    class h;
                        virtual bfm v;
                        task call(); v.w(); endtask
                    endclass
                endpackage
                module user(bfm.mp port);
                    initial #3 $display("%s", port.pf());
                    task pt(); $display("%s via", port.pf()); endtask
                endmodule
                module mid(bfm.mp q);
                    user u(.port(q));
                endmodule
                module sub;
                    task t(); $display("%m t"); endtask
                endmodule
                module leaf;
                    initial #4 Top.t2();
                endmodule
                module chain_leaf;
                    task u(); $display("%m chain"); endtask
                endmodule
                module chain;
                    chain_leaf c0 ();
                    task t(); c0.u(); endtask
                endmodule
                module m;
                    int k;
                    class C extends p::B;
                        virtual function void f(); $display("%m override %0d", k); endfunction
                    endclass
                    initial begin
                        C c = new;
                        p::obj.push_back(c);
                    end
                endmodule
                module Top;
                    bfm b0 ();
                    bfm arr[2] ();
                    for (genvar i = 0; i < 2; i++) begin : g
                        sub s ();
                    end
                    sub \esc.x ();
                    leaf l ();
                    chain ch ();
                    user u (.port(arr[1]));
                    mid md (.q(b0));
                    for (genvar i = 0; i < 1; i++) begin : gg
                        bfm bus ();
                        user gu (.port(bus));
                    end
                    m m0 ();
                    m m1 ();
                    virtual bfm v;
                    p::h x;
                    task t2(); $display("%m up"); endtask
                    initial begin
                        b0.x = 0;
                        arr[1].x = 0;
                        m0.k = 1;
                        m1.k = 2;
                        #1;
                        v = b0;
                        v.w();
                        // A member access through the handle is no copy:
                        // the value assigned is formatted in this scope.
                        v.y = ($sformatf("%m") == "Top");
                        $display("member %0d", b0.y);
                        x = new;
                        x.v = arr[1];
                        x.call();
                        #1;
                        g[1].s.t();
                        \esc.x .t();
                        ch.t();
                        u.pt();
                        #4;
                        foreach (p::obj[i]) p::obj[i].f();
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("copied_scope.sv"))], "Top");
        sim.run_until(7).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                celox::RuntimeEvent::Report(report) => Some(format!(
                    "{:?} {} [{}]",
                    report.severity,
                    String::from_utf8_lossy(&report.message),
                    report.scope.unwrap_or_default(),
                )),
                _ => None,
            })
            .collect();
        lines.sort();
        assert_eq!(lines, [
            "Error Top.arr[1].w error [Top.arr[1]]",
            "Error Top.b0.w error [Top.b0]",
            "Error assert failed [Top.arr[1]]",
            "Error assert failed [Top.b0]",
            "Time scale of (Top.arr[1]) is 1ns / 1ns",
            "Time scale of (Top.arr[1]) is 1ns / 1ns",
            "Time scale of (Top.b0) is 1ns / 1ns",
            "Time scale of (Top.gg[0].bus) is 1ns / 1ns",
            "Top.\\esc.x.t t",
            "Top.arr[1].f f",
            "Top.arr[1].pf pf",
            "Top.arr[1].pf pf via",
            "Top.arr[1].w fd",
            "Top.arr[1].w rt",
            "Top.arr[1].w w",
            "Top.b0.f f",
            "Top.b0.pf pf",
            "Top.b0.w fd",
            "Top.b0.w rt",
            "Top.b0.w w",
            "Top.ch.c0.u chain",
            "Top.g[1].s.t t",
            "Top.gg[0].bus.pf pf",
            "Top.m0.C::f override 1",
            "Top.m1.C::f override 2",
            "Top.t2 up",
            "Warning Top.arr[1].w action [Top.arr[1]]",
            "Warning Top.b0.w action [Top.b0]",
            "member 1",
        ]);
    }
    // An escaped block name keeps the white space that ends it in `%m`
    // (IEEE 1800-2023 5.6.1), so a `.` in it does not read as a separator.
    fn scope_name_of_escaped_named_blocks(sim) {
        @setup {
            let source = "`timescale 1ns/1ns\nmodule Top;\n    string s;\n    initial begin : \\a.b \n        begin : inner\n            $display(\"%m\");\n            s = $sformatf(\"%m\"); $display(\"%s\", s);\n            $sformat(s, \"%m\"); $display(\"%s\", s);\n            $swrite(s, \"%m\"); $display(\"%s\", s);\n        end\n    end\nendmodule\n";
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("escaped_block.sv"))], "Top");
        sim.run_until(0).unwrap();
        let name = b"Top.\\a.b .inner".to_vec();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: name.clone() },
            celox::RuntimeEvent::Display { message: name.clone() },
            celox::RuntimeEvent::Display { message: name.clone() },
            celox::RuntimeEvent::Display { message: name },
        ]);
    }
    // A format read at run time names the scope of the call in its `%m`,
    // as a literal one does: a named block, a module subroutine under the
    // instance, a package subroutine by its package (21.2.1.5, 21.3.3).
    fn scope_name_of_run_time_formats(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                package pkg;
                    function automatic string where(string format);
                        return $sformatf(format);
                    endfunction
                endpackage
                module Top;
                    string fmt = "%m", s;
                    task t();
                        s = $sformatf(fmt);
                        $display("task %s", s);
                    endtask
                    initial begin : b
                        #1;
                        s = $sformatf(fmt);
                        $display("block %s", s);
                        $sformat(s, fmt);
                        $display("sformat %s", s);
                        // Text of a format is never a directive to the host.
                        s = $sformatf("$scope:A:evil %m");
                        $display("text %s", s);
                        t();
                        $display("package %s", pkg::where(fmt));
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("run_time_scope.sv"))], "Top");
        sim.run_until(1).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"block Top.b".to_vec() },
            celox::RuntimeEvent::Display { message: b"sformat Top.b".to_vec() },
            celox::RuntimeEvent::Display { message: b"text $scope:A:evil Top.b".to_vec() },
            celox::RuntimeEvent::Display { message: b"task Top.t".to_vec() },
            celox::RuntimeEvent::Display { message: b"package pkg::where".to_vec() },
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
        // A member that is a packed array of enum values, which prints as
        // no single name.
        (
            "typedef enum logic [1:0] {A, B} e_t; \
             typedef struct packed { e_t [1:0] a; } s_t; s_t v;",
            "%p of a packed structure with an enum member",
        ),
        // A packed union prints none of its members by name, also with a
        // single member, whose layout equals a one-member structure's.
        (
            "typedef union packed { bit [3:0] a; } u_t; u_t v;",
            "%p of a packed union",
        ),
        (
            "typedef union packed { bit [3:0] a; bit [3:0] b; } u_t; u_t v;",
            "%p of a packed union",
        ),
        (
            "class C; endclass int v[C];",
            "%p of an associative array with class handle keys",
        ),
    ] {
        check_rejected(declarations, "v", expected);
    }
    for (declarations, value) in [
        ("int m[2][3];", "m[1]"),
        ("int a[4];", "a[1:2]"),
        ("typedef struct { int n; string s; } r_t; r_t q[$];", "q[0]"),
        (
            "typedef struct { int n; } r_t; \
             function automatic r_t make(); r_t r; r.n = 1; return r; endfunction",
            "make()",
        ),
    ] {
        check_rejected(declarations, value, "%p of a structure or unpacked array");
    }
    // A packed structure is printed through one select of each member, so
    // a function result, as any call that may have effects, is rejected.
    check_rejected(
        "typedef struct packed { bit [3:0] a; } s_t; \
         function automatic s_t make(); return s_t'(4'h3); endfunction",
        "make()",
        "%p of a packed structure value of a call with effects",
    );
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

// `$psprintf`, which simulators take as `$sformatf` (IEEE 1800-2023
// 21.3.3), formats as it does, at run time and as a constant (a documented
// deviation).
#[test]
fn psprintf_is_sformatf() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            localparam string NAME = $psprintf("unit%0d", 3);
            string s;
            int n = 5;
            initial begin
                s = $psprintf("%0d-%s", n, "x");
                $display("%s %s", s, NAME);
            end
        endmodule
    "#;
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["5-x unit3"]
    );
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

sv_backends! {
    // A packed structure prints as an assignment pattern with named members
    // (IEEE 1800-2023 21.2.1.6): a packed structure member as a pattern, an
    // enum member by name, others unformatted; also an element of a packed
    // structure array, a member that is a structure and a conditional of
    // two structures, as BlackParrot prints its configuration. An operator
    // makes a vector of a structure, which prints as one (7.2.1, 11.4).
    fn pattern_format_packed_structures(sim) {
        @setup {
            let source = r#"
                module Top;
                    typedef enum logic [1:0] {OFF, ON, IDLE} mode_e;
                    typedef struct packed { logic [3:0] c; logic signed [3:0] d; } inner_t;
                    typedef struct packed { logic [7:0] a; inner_t b; mode_e m; } outer_t;
                    outer_t v;
                    outer_t arr [2];
                    string s;
                    logic sel;
                    struct packed { logic [3:0] \%s ; } esc;
                    initial begin
                        v = '{a: 8'd200, b: '{c: 4'd5, d: -4'sd3}, m: ON};
                        arr[1] = v;
                        arr[1].m = IDLE;
                        $display("%p", v);
                        s = $sformatf("<%p>", arr[1]);
                        $display("%s", s);
                        $display("%p", v.b);
                        $display("%p", inner_t'(8'h5d));
                        sel = 1;
                        esc = 4'd6;
                        $display("%p", sel ? v.b : inner_t'(8'h00));
                        $display("%p", ~v.b);
                        $display("%p", v.b << 1);
                        $display("%p", esc);
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("pattern_packed.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"'{a:200, b:'{c:5, d:-3}, m:ON}".to_vec() },
            celox::RuntimeEvent::Display { message: b"<'{a:200, b:'{c:5, d:-3}, m:IDLE}>".to_vec() },
            celox::RuntimeEvent::Display { message: b"'{c:5, d:-3}".to_vec() },
            celox::RuntimeEvent::Display { message: b"'{c:5, d:-3}".to_vec() },
            celox::RuntimeEvent::Display { message: b"'{c:5, d:-3}".to_vec() },
            celox::RuntimeEvent::Display { message: b"162".to_vec() },
            celox::RuntimeEvent::Display { message: b"186".to_vec() },
            celox::RuntimeEvent::Display { message: br"'{\%s:6}".to_vec() },
        ]);
    }
}
