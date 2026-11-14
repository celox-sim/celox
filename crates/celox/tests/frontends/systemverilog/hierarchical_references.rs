use super::*;

pub(super) fn displayed(source: &str) -> Vec<String> {
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.run_until(1_000_000).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect()
}

fn build_error(source: &str) -> String {
    Simulator::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .build_cranelift()
        .expect_err("the design must be rejected")
        .to_string()
        // The report wraps long messages.
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// `$root` names the top instance from anywhere in the hierarchy (IEEE
// 1800-2023 23.3.1), including as an edge event of a flip-flop.
#[test]
fn root_names_start_at_the_top_instance() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf;
            logic [7:0] ticks = 8'd0;
            always @(posedge $root.Top.clk) ticks <= ticks + 8'd1;
        endmodule
        module Top;
            logic clk = 0;
            Leaf l0();
            Leaf l1();
            always #5 clk = ~clk;
            initial begin
                #22 $display("%0d %0d", $root.Top.l0.ticks, l1.ticks);
                $finish;
            end
        endmodule
    "#;
    assert_eq!(displayed(source), ["2 2"]);
}

#[test]
fn rejects_unsupported_hierarchical_drivers_by_name() {
    let child = "module Child; logic [7:0] v; wire [7:0] n = 8'd3; endmodule";
    let error = build_error(&format!(
        "{child} module Top(input logic [7:0] a); Child c(); assign c.v = a; endmodule"
    ));
    assert!(
        error.contains("continuous assignment to a variable through hierarchical name `c.v`"),
        "{error}"
    );
    let error = build_error(&format!(
        "{child} module Top; Child c(); initial c.n = 8'd1; endmodule"
    ));
    assert!(
        error.contains("driver of a net through hierarchical name `c.n`"),
        "{error}"
    );
}

#[test]
fn rejects_names_that_denote_no_signal() {
    let error = build_error(
        "module Child; logic v; endmodule \
         module Top; logic y; Child c(); initial y = c.missing; endmodule",
    );
    assert!(
        error.contains("unresolved hierarchical reference `c.missing`"),
        "{error}"
    );
}

#[test]
fn rejects_hierarchical_writes_next_to_exclusive_drivers() {
    let error = build_error(
        "module C(input logic [7:0] a); logic [7:0] v; assign v = a; endmodule \
         module Top; logic [7:0] a = 1; C c(.a(a)); initial #1 c.v = 8'd5; endmodule",
    );
    assert!(
        error.contains("which another module writes through a hierarchical name: its module drives it continuously"),
        "{error}"
    );
    let error = build_error(
        "module C(input logic clk); logic [7:0] v = 0; always @(posedge clk) v <= v + 1; endmodule \
         module Top; logic clk = 0; C c(.clk(clk)); always_ff @(posedge clk) c.v <= 8'd9; endmodule",
    );
    assert!(error.contains("an always_ff process writes it"), "{error}");
}

// Ordinary `always` processes may write one variable, also through a
// hierarchical name (IEEE 1800-2023 6.5): each write stores the bits it
// assigns, so neither loses the bits of the other.
#[test]
fn ordinary_always_processes_share_a_variable_through_hierarchical_names() {
    let source = r#"
        `timescale 1ns/1ns
        module C(input logic clk, input logic [2:0] idx);
            logic [7:0] q = 0;
            always @(posedge clk) q[idx] <= 1'b1;
        endmodule
        module Top(output logic [7:0] seen);
            logic clk = 0;
            logic [2:0] idx = 7;
            C c(.clk(clk), .idx(idx));
            always @(posedge clk) c.q[0] = 1'b1;
            initial begin
                #1 clk = 1;
                #1 clk = 0; idx = 3;
                #1 clk = 1;
                #1 seen = c.q;
            end
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 0b1000_1001u8.into());
}

// Names in a module with an interface port resolve in the module as
// written, not in the specialization made for the interface, including
// names that start at its own name.
#[test]
fn names_resolve_below_instances_with_interface_ports() {
    let source = r#"
        `timescale 1ns/1ns
        interface Bus; logic [7:0] d; endinterface
        module C; logic [7:0] v = 8'd4; endmodule
        module Leaf(interface bus);
            C c();
            bit q = 1;
            initial #1 $display("%0d %0d", c.v, Leaf.q);
        endmodule
        module Top; Bus b(); Leaf l(.bus(b)); endmodule
    "#;
    assert_eq!(displayed(source), ["4 1"]);
}

// A member of an interface instance of another instance, also of an
// element of an interface instance array, is named through the instance
// (IEEE 1800-2023 23.6, 25.3), as Caliptra's bus monitor reads
// `caliptra_top_dut.responder_inst[sl_i].hreadyout`.
#[test]
fn members_of_interface_instances_of_other_instances() {
    let source = r#"
        `timescale 1ns/1ns
        interface Bus #(parameter W = 8); logic [W-1:0] d; logic r; endinterface
        module Dut(input logic [7:0] base);
            Bus #(.W(8)) one();
            Bus #(.W(8)) many[0:2]();
            assign one.d = base;
            for (genvar k = 0; k < 3; k++) begin : drive
                assign many[k].d = base + 8'(k);
            end
        endmodule
        module Top;
            logic [7:0] base = 8'h10;
            Dut dut(base);
            logic [7:0] seen [0:2];
            for (genvar i = 0; i < 3; i++) begin : mon
                always @* seen[i] = dut.many[i].d;
            end
            initial begin
                #1 $display("%h %h %h %h %h", dut.one.d, dut.many[0].d, dut.many[2].d, seen[1], seen[2]);
                base = 8'h40;
                #1 $display("%h %h %h %h %h", dut.one.d, dut.many[0].d, dut.many[2].d, seen[1], seen[2]);
            end
        endmodule
    "#;
    // Icarus and Verilator print the same.
    assert_eq!(displayed(source), ["10 10 12 11 12", "40 40 42 41 42"]);
}

// An upward name finds its scope from instances at different depths (IEEE
// 1800-2023 23.8): when that scope is one instance of the design, every
// instance of the module denotes the same object.
#[test]
fn upward_names_from_instances_at_different_depths_denote_one_object() {
    let design = |cores: &str| {
        format!(
            r#"
            `timescale 1ns/1ns
            module Probe(output logic [7:0] value, output logic odd);
                assign value = Core.counter;
                assign odd = Core.counter[0];
            endmodule
            module Leaf(output logic [7:0] value);
                logic odd;
                Probe probe(value, odd);
            endmodule
            module Mid(output logic [7:0] near, far);
                logic odd;
                Probe probe(near, odd);
                Leaf leaf(far);
            endmodule
            module Core(input logic clk, output logic [23:0] seen);
                logic [7:0] counter = 8'd5;
                logic [7:0] direct, near, far, below;
                logic odd;
                always @(posedge clk) counter <= counter + 8'd3;
                Probe probe(direct, odd);
                Mid mid(near, far);
                Leaf leaf(below);
                assign seen = {{direct, near, far}} ^ {{16'd0, below}} ^ {{23'd0, odd}};
            endmodule
            module Top;
                logic clk = 0;
                logic [23:0] seen{cores};
                initial begin
                    #1 $display("%h", seen);
                    clk = 1;
                    #1 $display("%h", seen);
                end
            endmodule
        "#
        )
    };
    // 5 and then 8 in each of the three bytes; the fourth probe and the
    // parity cancel in the low byte but for the parity bit.
    assert_eq!(
        displayed(&design("; Core core(clk, seen)")),
        ["050501", "080800"]
    );
    // From two cores the name denotes two objects, one for each.
    let error = build_error(&design(", other; Core a(clk, seen), b(clk, other)"));
    assert!(
        error.contains(
            "upward hierarchical reference `Core.counter` that denotes different objects in \
             instances of module"
        ),
        "{error}"
    );
}

// A forced signal that is the target of a hierarchical name stays forceable
// in the specialization the name resolution makes.
#[test]
fn forced_targets_of_hierarchical_names_stay_forceable() {
    let source = r#"
        `timescale 1ns/1ns
        module C; logic [7:0] v = 8'd4; logic [7:0] q = 8'd1; endmodule
        module Top;
            C c();
            initial #1 $display("%0d %0d", c.v, c.q);
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .native_force_support(true)
        .signal_access(celox::SignalAccessOptions {
            force_signals: vec!["Top.c.q".into()],
            ..Default::default()
        })
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
    assert_eq!(displayed, ["4 1"]);
}

#[test]
fn local_interface_scopes_block_upward_fallback_even_when_empty() {
    for body in ["", "logic other;"] {
        let source = format!(
            r#"
            interface Bus; logic v; endinterface
            interface Empty; {body} endinterface
            module Child(output wire q);
                Empty ancestor_bus();
                assign q = ancestor_bus.v;
            endmodule
            module Top(input logic d, output wire q);
                Bus ancestor_bus();
                assign ancestor_bus.v = d;
                Child child(q);
            endmodule
        "#
        );
        let error = build_error(&source);
        assert!(
            error.contains("unresolved hierarchical reference `ancestor_bus.v`"),
            "{error}"
        );
    }
    let source = r#"
        interface Bus; logic v; endinterface
        interface Empty; endinterface
        module Child(output wire q);
            Empty ancestor_bus[2]();
            assign q = ancestor_bus[0].v;
        endmodule
        module Top(input logic d, output wire q);
            Bus ancestor_bus[2]();
            assign ancestor_bus[0].v = d;
            Child child(q);
        endmodule
    "#;
    let error = build_error(source);
    assert!(
        error.contains("unresolved hierarchical reference `ancestor_bus[0].v`"),
        "{error}"
    );
}

// Each instance of a module declares its own types (IEEE 1800-2023 6.22),
// so an enum variable of one instance takes the value of another's only by
// a cast (6.19.3). Instances of one module with the same parameters share
// one analysis and so, here, one type: the assignment is accepted. This is
// a known deviation, which the test pins until types tell instances apart.
#[test]
fn enum_assignments_between_instances_of_one_module_are_accepted() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf(input logic set);
            typedef enum logic [1:0] {X, Y, Z} e_t;
            typedef struct packed { logic [3:0] a; e_t e; } p_t;
            e_t v;
            p_t p;
            initial begin
                #1;
                if (set) begin
                    v = Y;
                    p = '{a: 4'd5, e: Z};
                end
            end
        endmodule
        module Top;
            Leaf a(1'b1);
            Leaf b(1'b0);
            initial begin
                #2;
                b.v = a.v;
                b.p = a.p;
                #1;
                $display("v=%0d p.a=%0d p.e=%0d", b.v, b.p.a, b.p.e);
            end
        endmodule
    "#;
    assert_eq!(displayed(source), ["v=1 p.a=5 p.e=2"]);
}

#[test]
fn static_variables_of_subroutines_are_named_through_their_subroutines() {
    // `f.v` names the static variable `v` of subroutine `f` (IEEE 1800-2023
    // 23.6, 6.21): of a method of the class or a base, also when only the
    // method writes it, per specialization, and of a function or task of the
    // module; it is read and written through the name (uvm-1.2's
    // `visit.compiled_regex`).
    let source = "package p;
    class Base;
        virtual function void visit(int n);
            static int count;
            static int last;
            static chandle h;
            count += n;
            last = n;
            if (h == null) h = null;
        endfunction
        function int peek(); return visit.count * 100 + visit.last + (visit.h == null); endfunction
    endclass
    class Derived extends Base;
        function void reset(); visit.count = 0; endfunction
    endclass
    class G #(int W = 1);
        function int bump(); static int k; k += W; return k; endfunction
        function int seen(); return bump.k; endfunction
    endclass
    endpackage
    module Top;
        import p::*;
        typedef G#(1) g1_t;
        typedef G#(10) g10_t;
        function automatic int tally(); static int calls; calls++; return calls; endfunction
        task automatic tick(); static int n; n++; endtask
        initial begin
            Derived d = new;
            g1_t a = new;
            g10_t b = new;
            d.visit(2);
            d.visit(3);
            $display(\"%0d\", d.peek());
            d.reset();
            d.visit(7);
            $display(\"%0d\", d.peek());
            void'(a.bump());
            void'(a.bump());
            void'(b.bump());
            $display(\"%0d %0d\", a.seen(), b.seen());
            void'(tally());
            tick();
            tally.calls = 10;
            $display(\"%0d %0d %0d\", tally(), tally.calls, tick.n);
        end
    endmodule";
    assert_eq!(displayed(source), ["504", "708", "2 10", "11 11 1"]);
}

#[test]
fn automatic_variables_of_subroutines_have_no_hierarchical_names() {
    // An automatic variable is no object a hierarchical name reaches
    // (IEEE 1800-2023 6.21).
    let source = "class V;
        function void visit(); int count; count++; endfunction
        function int peek(); return visit.count; endfunction
    endclass
    module Top; initial begin V v = new; $display(\"%0d\", v.peek()); end endmodule";
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("auto.sv"))], "Top")
            .build_cranelift()
    else {
        panic!("an automatic variable was named hierarchically");
    };
    assert!(error.to_string().contains("visit.count"), "{error}");
}

#[test]
fn static_variable_references_resolve_where_they_are_written() {
    // `f.v` names the static `v` of the `f` visible at the reference (IEEE
    // 1800-2023 23.6): a module's own function, not another module's; a
    // method of the class before a function of the module (8.5); a
    // protected method of a base (8.18). `v` has the type `f` declares it
    // with, a local typedef included, and its initial value before `f` is
    // ever called (6.21); a formal of a static function is a static
    // variable of it too (13.3.1).
    for (body, expected) in [
        (
            "module M1; function int f(); static int v = 7; v++; return v; endfunction
                initial void'(f()); endmodule
             module Top; M1 m1();
                function int f(); static int v = 100; return v; endfunction
                initial #1 $display(\"%0d\", f.v); endmodule",
            "100",
        ),
        (
            "module Top;
                function int f(); static int v = 1; return v; endfunction
                class C;
                    function int f(); static int v = 2; return v; endfunction
                    function int g(); return f.v; endfunction
                endclass
                initial begin C c = new; $display(\"%0d %0d\", c.g(), f.v); end
             endmodule",
            "2 1",
        ),
        (
            "class B; protected function int h(); static int w = 4; return w; endfunction endclass
             class D extends B; function int g(); return h.w; endfunction endclass
             module Top; initial begin D d = new; $display(\"%0d\", d.g()); end endmodule",
            "4",
        ),
        (
            "module Top;
                function int f(); typedef bit [11:0] w_t; static w_t v; return v; endfunction
                initial begin f.v = 13'h1fff; $display(\"%h %0d\", f.v, $bits(f.v)); end
             endmodule",
            "fff 12",
        ),
        (
            "module Top; function int f(int arg); return arg; endfunction
                initial begin void'(f(5)); $display(\"%0d\", f.arg); end endmodule",
            "5",
        ),
    ] {
        assert_eq!(displayed(body), [expected], "{body}");
    }
    // Rejected: a function of another module, an automatic variable by the
    // unit's default lifetime (6.21), a local method of a base (8.18).
    for body in [
        "module M1; function int f(); static int v = 7; return v; endfunction endmodule
         module Top; M1 m1(); initial $display(\"%0d\", f.v); endmodule",
        "module automatic Top; function int f(); int v = 1; return v; endfunction
            initial $display(\"%0d\", f.v); endmodule",
        "class B; local function int f(); static int v = 3; return v; endfunction endclass
         class D extends B; function int g(); return f.v; endfunction endclass
         module Top; initial begin D d = new; $display(\"%0d\", d.g()); end endmodule",
    ] {
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(body, Path::new("refs.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("accepted: {body}");
        };
        assert!(error.to_string().contains("f.v"), "{body}: {error}");
    }
}
