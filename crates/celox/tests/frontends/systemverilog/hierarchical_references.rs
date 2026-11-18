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

// A continuous assignment may drive a variable of another instance through a
// hierarchical name when nothing else writes it (IEEE 1800-2023 6.5, 23.6).
#[test]
fn continuous_assignments_drive_variables_of_other_instances() {
    let source = r#"
        `timescale 1ns/1ns
        module Child; logic [7:0] v; logic [7:0] w; assign w = v + 8'd1; endmodule
        module Top;
            logic [7:0] a = 8'h41;
            Child c();
            assign c.v = a;
            initial begin
                #1 $display("%h", c.w);
                a = 8'h7f;
                #1 $display("%h", c.w);
            end
        endmodule
    "#;
    // Verilator 5.054 prints the same.
    assert_eq!(displayed(source), ["42", "80"]);
    // The variable's own module may not drive it as well.
    for driver in ["assign v = 8'd5;", "always_comb v = 8'd5;"] {
        let error = build_error(&format!(
            "module Child; logic [7:0] v; {driver} endmodule \
             module Top(input logic [7:0] a); Child c(); assign c.v = a; endmodule"
        ));
        assert!(
            error.contains("multiple variable drivers for `v`, which another module writes"),
            "{driver}: {error}"
        );
    }
}

#[test]
fn rejects_unsupported_hierarchical_drivers_by_name() {
    let child = "module Child; logic [7:0] v; wire [7:0] n = 8'd3; endmodule";
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

/// A name the body of an interface does not declare denotes an object
/// relative to where the instance is (IEEE 1800-2023 23.8). A module that
/// receives the instance through an interface port, or an interface whose
/// body instantiates it, is not that scope; the name is rejected there.
#[test]
fn outward_names_of_interfaces_through_ports_and_nested_instances_are_unsupported() {
    let interfaces = "interface Ctl; logic clk; endinterface
        interface Counter; int n = 0; always @(posedge CONTROL.clk) n++; endinterface";
    for (design, expected) in [
        (
            "module User(Counter p); endmodule
             module Top; Ctl CONTROL(); Counter c(); User u(c); endmodule",
            "hierarchical name `CONTROL.clk` in the body of an interface that interface port `p` connects",
        ),
        (
            "interface Outer; Counter inner(); endinterface
             module Top; Ctl CONTROL(); Outer o(); endmodule",
            "hierarchical name `CONTROL.clk` in the body of an interface instance nested in interface instance `o`",
        ),
    ] {
        let source = format!("{interfaces}\n{design}");
        let error =
            match Simulator::from_sv_sources(vec![(source.as_str(), Path::new("up.sv"))], "Top")
                .build_interpreter()
            {
                Ok(_) => panic!("{design}: an outward name of this body is unsupported"),
                Err(error) => error.to_string(),
            };
        assert!(error.contains(expected), "{design}: {error}");
    }
}

/// A task called through a virtual interface reads and writes, in each
/// instance, the object its name denotes from that instance's parent (IEEE
/// 1800-2023 23.8, 25.9). One subroutine serves every instance, so those
/// objects must have one type; instances whose objects differ are rejected.
#[test]
fn outward_names_of_virtual_interface_calls_need_one_type() {
    let source = "interface Drv; task put(logic [7:0] v); CONTROL.data = v; endtask endinterface
        interface C8; logic [7:0] data; endinterface
        interface C16; logic [15:0] data; endinterface
        package cfg; virtual Drv vs[2]; endpackage
        module A; C8 CONTROL(); Drv d(); initial cfg::vs[0] = d; endmodule
        module B; C16 CONTROL(); Drv d(); initial cfg::vs[1] = d; endmodule
        module Top; A a(); B b(); initial cfg::vs[0].put(8'h11); endmodule";
    let error = match Simulator::from_sv_sources(vec![(source, Path::new("up.sv"))], "Top")
        .build_interpreter()
    {
        Ok(_) => panic!("objects of different types must be rejected"),
        Err(error) => error.to_string(),
    };
    let error = error
        .split_whitespace()
        .filter(|word| *word != "│")
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains("which denotes objects of different types in different instances"),
        "{error}"
    );
}

// A hierarchical name may select a member of an element of an array of
// packed structures of another instance, as `ctrl.kv_read[0].read_entry`
// (IEEE 1800-2023 7.4.5, 23.6): from above and below, inside the index of
// another hierarchical name, by a genvar, of an unpacked array, and as the
// target of a write. The values are those Verilator 5.054 prints.
#[test]
fn members_of_elements_of_arrays_of_other_instances() {
    let source = r#"
        `timescale 1ns/1ns
        typedef struct packed { logic [4:0] read_entry; logic [3:0] read_offset; } kv_read_t;
        typedef struct packed { logic [3:0] last; logic [3:0] first; } ctrl_t;
        module Ctrl(output kv_read_t [2:0] kv_read);
            assign kv_read[0] = {5'd7, 4'd1};
            assign kv_read[1] = {5'd2, 4'd2};
            assign kv_read[2] = {5'd3, 4'd5};
        endmodule
        module Vault;
            ctrl_t [7:0] key_ctrl;
            kv_read_t u_arr [3];
            kv_read_t [1:0][1:0] two;
            initial begin
                for (int i = 0; i < 8; i++) key_ctrl[i] = {4'(i + 8), 4'(i)};
                u_arr[0] = {5'd11, 4'd3};
                u_arr[1] = {5'd12, 4'd4};
                u_arr[2] = '0;
                two = '0;
            end
        endmodule
        module Dut;
            Ctrl c();
            Vault v();
        endmodule
        module Checker;
            initial #3 $display("up %0d", Top.d.c.kv_read[2].read_offset);
        endmodule
        module Top;
            Dut d();
            Checker chk();
            for (genvar g = 0; g < 3; g++) begin : gen
                initial #1 $display("gen%0d %0d", g, d.c.kv_read[g].read_entry);
            end
            initial begin
                #1 $display("nested %0d", Top.d.v.key_ctrl[Top.d.c.kv_read[1].read_entry].last);
                $display("element %0d", d.c.kv_read[0].read_entry);
                $display("unpacked %0d %0d", Top.d.v.u_arr[1].read_entry, d.v.u_arr[0].read_offset);
                d.v.u_arr[2].read_entry = 5'd21;
                Top.d.v.two[1][0].read_offset = 4'd9;
                #1 $display("written %0d %0d %0d", d.v.u_arr[2].read_entry,
                    Top.d.v.two[1][0].read_offset, Top.d.v.two[0][1].read_offset);
                #2 $finish;
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        // The generate blocks print at the same time as the initial block.
        lines.sort();
        assert_eq!(
            lines,
            [
                "element 7",
                "gen0 7",
                "gen1 2",
                "gen2 3",
                "nested 10",
                "unpacked 12 3",
                "up 5",
                "written 21 9 0",
            ],
            "four_state={four_state}"
        );
    }
}
