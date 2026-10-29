//! Unpacked structures (IEEE 1800-2023 7.2).

use super::*;

sv_backends! {
    fn unpacked_struct_members_whole_copies_and_equality(sim) {
        @case "unpacked_structs::members_whole_copies_and_equality";
    }
    fn unpacked_struct_assignment_patterns(sim) {
        @case "unpacked_structs::assignment_patterns";
    }
    fn unpacked_struct_subroutine_arguments_and_results(sim) {
        @case "unpacked_structs::subroutine_arguments_and_results";
    }
    fn unpacked_struct_initializers_results_and_defaults(sim) {
        @case "unpacked_structs::initializers_results_and_defaults";
    }
    fn unpacked_struct_results_and_outputs(sim) {
        @case "unpacked_structs::results_and_outputs";
    }

    fn structure_properties_hide_module_variables(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int a; int b; } pair_t;
                int p;
                class C;
                    pair_t p;
                    function int first();
                        p.a = 7;
                        return p.a;
                    endfunction
                endclass
                initial begin
                    C c;
                    c = new;
                    p = 3;
                    value = c.first() * 10 + p;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(73u8));
    }

    fn structure_properties_in_for_headers_and_method_precedence(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int a; int b; } pair_t;
                function int get();
                    return 5;
                endfunction
                class H;
                    pair_t p;
                    function pair_t get();
                        return '{4, 2};
                    endfunction
                    function int run();
                        int n;
                        for (p = '{0, 0}; p.a < 3; p = '{p.a + 1, p.b + 2}) n++;
                        p = get();
                        return n * 100 + p.a * 10 + p.b;
                    endfunction
                endclass
                initial begin
                    H h;
                    h = new;
                    value = h.run();
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(342u16));
    }

    fn container_elements_with_packed_and_process_members(sim) {
        @setup {
            let source = r#"module Top(output bit [3:0] code, output bit [7:0] header,
                    output bit null_process, output int changes);
                timeunit 1ps / 1ps;
                typedef struct packed { bit [3:0] code; bit [3:0] kind; } header_t;
                typedef struct { header_t header; string s; } element_t;
                typedef struct { process p; logic flag; } waiter_t;
                typedef struct { string name; bit flag; } entry_t;
                entry_t q[$];
                initial begin
                    element_t r[$];
                    waiter_t w[];
                    r.push_back('{8'hA5, "a"});
                    code = r[0].header.code;
                    header = r[0].header;
                    w = new[1];
                    null_process = w[0].p == null;
                    q.push_back('{"x", 0});
                    #1 q[0] = '{"x", 1};
                end
                initial begin
                    @(q[0].flag);
                    changes = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top")
            .four_state(true);
        sim.run_until(5).unwrap();
        for (name, value) in [("code", 0xau8), ("header", 0xa5), ("null_process", 1), ("changes", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    fn constructors_in_structure_patterns(sim) {
        @setup {
            let source = r#"module Top(output bit ok);
                class Node;
                    string n;
                    function new();
                        n = "made";
                    endfunction
                endclass
                typedef struct { Node n; int k; } handle_t;
                initial begin
                    handle_t h;
                    h = '{n: new(), k: 1};
                    ok = h.n.n == "made" && h.k == 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }
    fn static_task_structure_arguments_are_shared(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output int first, output int second);
                typedef struct { int a; string s; } row_t;
                task show(input row_t r, output int seen);
                    #2 seen = r.a * 10 + r.s.len();
                endtask
                initial show('{1, "x"}, first);
                initial #1 show('{2, "yy"}, second);
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(4).unwrap();
        let (first, second) = (sim.signal("first"), sim.signal("second"));
        assert_eq!(sim.get(first), BigUint::from(22u8));
        assert_eq!(sim.get(second), BigUint::from(22u8));
    }
    fn unknown_record_members_and_properties_are_not_folded(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { string name; logic flag; } row_t;
                class H;
                    logic f;
                endclass
                initial begin
                    row_t q[$];
                    row_t d[];
                    H h;
                    h = new;
                    q.push_back('{name: "a", flag: 'x});
                    value = 0;
                    if (q[0].flag || !q[0].flag) value += 1;
                    else value += 10;
                    if (h.f || !h.f) value += 100;
                    else value += 1000;
                    d = new[2];
                    if ((d[0] == d[1]) || (d[0] != d[1])) value += 10000;
                    else value += 100000;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top")
            .four_state(true);
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(101010u32));
    }
    fn equal_record_updates_preserve_event_waits(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output bit done, output int changes);
                typedef struct { string name; int value; } row_t;
                row_t q[$], d[];
                int count, before_count;
                initial begin
                    q.push_back('{"a", 1});
                    d = new[1];
                    #1 before_count = count;
                    d[0] = d[0];
                    d[0] = '{default: 0, name: ""};
                    q[0] = q[0];
                    q[0] = '{{"a"}, 1};
                    #1 q = '{'{"a", 1}};
                    #1 done = count == before_count;
                    q[0] = '{"b", 1};
                    #1 changes = count - before_count;
                end
                always @(q or d) count = count + 1;
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(4).unwrap();
        let (done, changes) = (sim.signal("done"), sim.signal("changes"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(changes), BigUint::from(1u8));
    }
    fn static_nested_string_initializers(sim) {
        @setup {
            let source = r#"module Top(output int n);
                typedef struct { string s; } in_t;
                typedef struct { in_t i; } out_t;
                initial repeat (2) begin
                    static out_t o = '{'{"abc"}};
                    n = n * 10 + o.i.s.len();
                    o.i.s = {o.i.s, "d"};
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let n = sim.signal("n");
        assert_eq!(sim.get(n), BigUint::from(34u8));
    }
    fn explicit_static_structures_of_repeating_processes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output int out);
                typedef struct { int a; string s; } row_t;
                int source = 5;
                logic clk = 0;
                always #1 clk = ~clk;
                always @(posedge clk) begin
                    static row_t r = '{a: source, s: "x"};
                    r.a++;
                    out = r.a * 10 + r.s.len();
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(4).unwrap();
        let out = sim.signal("out");
        assert_eq!(sim.get(out), BigUint::from(71u8));
    }
    fn enumeration_methods_of_container_element_members(sim) {
        @setup {
            let source = r#"module Top(output bit done);
                typedef enum { STARTED, FINISHED } state_e;
                typedef struct { state_e state; string tag; } rec_t;
                typedef struct { state_e state; int n; } flat_t;
                initial begin
                    rec_t m[int];
                    flat_t q[$];
                    string s;
                    m[2] = '{STARTED, "x"};
                    q.push_back('{FINISHED, 3});
                    s = {m[2].state.name(), " ", q[0].state.name, " ", m[2].state.next().name()};
                    done = s == "STARTED FINISHED FINISHED" && q[0].state.prev() == STARTED;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
    }
    fn property_bound_structure_references_forward_and_capture(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int a; string s; } pair_t;
                class H;
                    pair_t p;
                endclass
                int arr[2];
                function automatic void set_pair(output pair_t p);
                    p = '{7, "s"};
                endfunction
                function automatic void bump_pair(inout pair_t p);
                    p.a++;
                    p.s = {p.s, "!"};
                endfunction
                function automatic void forward(ref pair_t r);
                    set_pair(r);
                    bump_pair(r);
                endfunction
                function automatic void place(ref pair_t r);
                    {r.a, arr[r.a]} = {32'd1, 32'd7};
                endfunction
                initial begin
                    H h;
                    pair_t v;
                    h = new;
                    forward(h.p);
                    forward(v);
                    value = h.p.a * 1000 + v.a * 100 + h.p.s.len() * 10 + v.s.len();
                    h.p.a = 0;
                    place(h.p);
                    value = value * 100 + h.p.a * 10 + arr[0] + arr[1];
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(882217u32));
    }
    fn redeclared_structure_properties_hide_their_members(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int a; int b; } pair_t;
                typedef struct { int a; } one_t;
                class Base;
                    pair_t p;
                endclass
                class Derived extends Base;
                    one_t p;
                    function int sum();
                        return p.a * 100 + super.p.a * 10 + super.p.b;
                    endfunction
                endclass
                initial begin
                    Derived d;
                    Base b;
                    d = new;
                    b = d;
                    d.p.a = 1;
                    b.p = '{2, 3};
                    value = d.sum() * 10 + d.p.a;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(1231u16));
    }
    fn structure_members_of_class_typedefs(sim) {
        @setup {
            let source = r#"module Top(output int value);
                class Base;
                    typedef enum { NEVER, STARTED, FINISHED } rs_e;
                endclass
                class C extends Base;
                    typedef bit [3:0] nibble_t;
                    typedef struct { rs_e state; nibble_t n; bit ret_val; } st_t;
                    st_t m[string];
                    function int f();
                        st_t s;
                        s.state = rs_e'(2);
                        s.n = 4'h9;
                        s.ret_val = 1;
                        m["a"] = s;
                        return m["a"].state * 100 + m["a"].n * 10 + m["a"].ret_val;
                    endfunction
                endclass
                initial begin
                    C c;
                    c = new;
                    value = c.f();
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(291u16));
    }
    fn structure_defaults_descend_to_members_of_their_type(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int n; string s; } A;
                typedef struct { A x; A z; } B;
                typedef struct { B b; A y; } C;
                initial begin
                    A a;
                    C c;
                    a = '{3, "abc"};
                    c = '{default: a};
                    value = c.b.x.n * 1000 + c.b.z.s.len() * 100 + c.y.n * 10;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(3330u16));
    }
    fn queue_pops_without_parentheses_are_whole_elements(sim) {
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { int a; string s; } row_t;
                initial begin
                    row_t q[$];
                    row_t r;
                    q = '{'{1, "a"}, '{2, "bb"}, '{3, "ccc"}};
                    r = q.pop_front;
                    value = r.a * 10 + r.s.len();
                    r = q.pop_back;
                    value = value * 100 + r.a * 10 + r.s.len() + q.size() * 1000;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(2133u16));
    }
    fn equal_four_state_record_updates_preserve_event_waits(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output bit done);
                typedef struct { string name; logic flag; } row_t;
                row_t d[];
                int count, before_count;
                initial begin
                    d = new[1];
                    #1 before_count = count;
                    d[0] = d[0];
                    #1 done = count == before_count && $isunknown(d[0].flag);
                end
                always @(d) count = count + 1;
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top")
            .four_state(true);
        sim.run_until(3).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
    }
    fn unpacked_struct_module_variables(sim) {
        @case "unpacked_structs::module_variables";
    }
    fn unpacked_struct_handle_members_survive_heap_collections(sim) {
        @case "unpacked_structs::handle_members_survive_heap_collections";
    }
    fn unpacked_struct_four_state_members(sim) {
        @case "unpacked_structs::four_state_members";
    }
    fn unpacked_struct_reference_formals(sim) {
        @case "unpacked_structs::reference_formals";
    }
    fn unpacked_struct_reference_formals_of_virtual_methods(sim) {
        @case "unpacked_structs::reference_formals_of_virtual_methods";
    }
    fn unpacked_struct_reference_formals_across_time(sim) {
        @case "unpacked_structs::reference_formals_across_time";
    }
    fn unpacked_struct_reference_formals_four_state(sim) {
        @case "unpacked_structs::reference_formals_four_state";
    }
    fn unpacked_struct_reference_formal_handles_survive_heap_collections(sim) {
        @case "unpacked_structs::reference_formal_handles_survive_heap_collections";
    }
    fn unpacked_struct_class_properties(sim) {
        @case "unpacked_structs::class_properties";
    }
    fn unpacked_struct_class_property_assignment_order(sim) {
        @case "unpacked_structs::class_property_assignment_order";
    }
    fn unpacked_struct_class_property_four_state_members(sim) {
        @case "unpacked_structs::class_property_four_state_members";
    }
    fn unpacked_struct_class_properties_inheritance_and_collections(sim) {
        @case "unpacked_structs::class_properties_inheritance_and_collections";
    }
    fn unpacked_struct_container_elements(sim) {
        @case "unpacked_structs::container_elements";
    }
    fn unpacked_struct_container_element_member_selects(sim) {
        @case "unpacked_structs::container_element_member_selects";
    }
    fn unpacked_struct_container_element_four_state_defaults(sim) {
        @case "unpacked_structs::container_element_four_state_defaults";
    }
    fn unpacked_struct_container_records_survive_heap_collections(sim) {
        @case "unpacked_structs::container_records_survive_heap_collections";
    }
}

#[test]
fn unsupported_and_illegal_uses_are_rejected() {
    let types = "typedef struct { int a; bit b; } i_t; typedef struct { int a; string s; } s_t;";
    for (body, want) in [
        ("i_t v; initial v = 5;", "not of its type"),
        ("i_t v; int x; initial x = v;", "not of its type"),
        (
            "function int f(int x); return x; endfunction i_t v; initial $display(\"%0d\", f(v));",
            "not of its type",
        ),
        (
            "typedef struct { int a; bit b; } j_t; typedef struct { i_t m; } o_t; o_t o; initial o = '{m: j_t'{1, 0}};",
            "not of its type",
        ),
        ("i_t v; s_t w; initial v = w;", "not of its type"),
        (
            "class A; endclass class B; endclass typedef struct { A node; } r_t; r_t r; B b; initial r = '{node: b};",
            "assignment pattern member",
        ),
        (
            "typedef struct { int a; bit b; } j_t; i_t v; initial v = j_t'{1, 0};",
            "not of its type",
        ),
        (
            "typedef struct { int a; bit b; } j_t; function int f(i_t r); return r.a; endfunction j_t w; initial $display(\"%0d\", f(w));",
            "not of its type",
        ),
        (
            "function int f(i_t r); return r.a; endfunction initial $display(\"%0d\", f(5));",
            "not of its type",
        ),
        ("function i_t f(); return 5; endfunction", "not of its type"),
        (
            "i_t v; int x; initial x = v + 1;",
            "applied to an unpacked structure",
        ),
        (
            "i_t v; initial $display(\"%0d\", v[3]);",
            "applied to an unpacked structure",
        ),
        (
            "i_t v; s_t w; bit x; initial x = v == w;",
            "comparing an unpacked structure",
        ),
        (
            "i_t v; initial v = '{1, 2, 3};",
            "3 items for 2 structure members",
        ),
        ("i_t v; initial v.nope = 1;", "structure member `v.nope`"),
        (
            "i_t v; initial v += v;",
            "operator `+=` on an unpacked structure",
        ),
        (
            "parameter i_t P = '{1, 0};",
            "parameter of unpacked structure type",
        ),
        (
            "s_t q[$], r[$]; bit x; initial x = q == r;",
            "comparison of dynamic containers of unpacked structures",
        ),
        (
            "i_t q[$]; initial q.sort();",
            "sorting unpacked structures requires a with expression",
        ),
        (
            "s_t q[$]; initial begin q.push_back('{1, \"x\"}); q[0].a = 5; end",
            "assignment to a member of a dynamic container element",
        ),
        (
            "s_t q[$]; initial begin q.push_back('{1, \"x\"}); q[0].a++; end",
            "assignment to a member of a dynamic container element",
        ),
        (
            "typedef struct { bit [15:8] hi; string s; } h_t; h_t q[$]; bit x; initial x = q[0].hi[8];",
            "select of a container element member that is not declared [N-1:0]",
        ),
        (
            "typedef struct { s_t inner; } n_t; n_t q[$]; s_t i; initial i = q.pop_front().inner;",
            "structure member with several members of an element read with a side effect",
        ),
        (
            "typedef struct { s_t inner; } n_t; n_t q[$]; s_t i; initial i = q[$urandom_range(0, 1)].inner;",
            "structure member with several members of an element read with a side effect",
        ),
        (
            "class C; static i_t v; endclass",
            "static class property of unpacked structure type",
        ),
        (
            "class C; rand i_t v; endclass",
            "random class property of unpacked structure type",
        ),
        (
            "class C; i_t v; endclass C c; initial begin c = new; c.v += c.v; end",
            "operator `+=` on an unpacked structure",
        ),
        (
            "class C; i_t v; endclass C c; i_t w; initial begin c = new; c.v <= w; end",
            "nonblocking assignment to a class property",
        ),
        (
            "class C; i_t v; endclass C cs[2]; int k; i_t w; function int f(); k = k + 1; return k; endfunction initial w = cs[f()].v;",
            "whole unpacked structure property read through a receiver with a side effect",
        ),
        (
            "class C; i_t v; endclass C cs[$]; int k[$]; i_t w; initial w = cs[k.pop_front()].v;",
            "whole unpacked structure property read through a receiver with a side effect",
        ),
        (
            "typedef struct { int a; bit b; } j_t; function automatic void f(ref i_t r); r.a++; endfunction j_t v; initial f(v);",
            "not equivalent",
        ),
        (
            "typedef struct { int a; bit b; } j_t; function automatic void f(ref i_t q[$]); endfunction j_t v[$]; initial f(v);",
            "not equivalent",
        ),
        (
            "function automatic void f(const ref i_t r); r = '{1, 0}; endfunction i_t v; initial f(v);",
            "const ref argument cannot be",
        ),
        (
            "function automatic void f(const ref i_t r); r.a = 1; endfunction i_t v; initial f(v);",
            "const ref argument cannot be",
        ),
        (
            "function automatic void g(ref int x); endfunction function automatic void f(const ref i_t r); g(r.a); endfunction i_t v; initial f(v);",
            "const ref argument cannot be",
        ),
        (
            "function automatic void f(ref i_t r); endfunction i_t q[$]; initial f(q[0]);",
            "ref argument of `f` bound to an element of a dynamic array",
        ),
        (
            "class C; i_t v; endclass C cs[$]; function automatic void f(ref i_t r); endfunction initial f(cs[$urandom_range(0, 1)].v);",
            "whole unpacked structure property read through a receiver with a side effect",
        ),
        (
            "class B; i_t p; endclass class D extends B; int p; endclass D d; int x; initial begin d = new; x = d.p.a; end",
            "structure member `d.p.a`",
        ),
        (
            "class B; i_t p; endclass typedef struct { int a; } one_t; class D extends B; one_t p; endclass D d; bit x; initial begin d = new; x = d.p.b; end",
            "structure member `d.p.b`",
        ),
        ("i_t v[2];", "array of unpacked structures"),
        (
            "typedef struct packed { i_t x; } p_t; p_t v;",
            "unsupported packed struct member",
        ),
        (
            "typedef union { int a; bit b; } u_t; u_t u;",
            "unpacked struct, union",
        ),
        (
            "function i_t g(); return '{1, 0}; endfunction function void f(); static i_t v = g(); endfunction",
            "initializer of a static variable of unpacked structure type",
        ),
    ] {
        let source = format!("module Top; {types} {body} endmodule");
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("struct.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("`{body}` must be rejected");
        };
        let error = error.to_string();
        assert!(error.contains(want), "`{body}`: {error}");
    }
    let source = "module Top(output i_t o); typedef struct { int a; } i_t; endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("struct.sv"))], "Top").build_cranelift()
    else {
        panic!("an unpacked structure port is unsupported");
    };
    let error = error.to_string();
    assert!(error.contains("port of unpacked structure type"), "{error}");
}
