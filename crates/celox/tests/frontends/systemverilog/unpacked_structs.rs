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
    fn unpacked_struct_module_variables(sim) {
        @case "unpacked_structs::module_variables";
    }
    fn unpacked_struct_handle_members_survive_heap_collections(sim) {
        @case "unpacked_structs::handle_members_survive_heap_collections";
    }
    fn unpacked_struct_four_state_members(sim) {
        @case "unpacked_structs::four_state_members";
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
        ("i_t q[$];", "dynamic container of unpacked structures"),
        (
            "class C; i_t v; endclass",
            "class property of unpacked structure type",
        ),
        (
            "function automatic void f(ref i_t r); r.a++; endfunction i_t v; initial f(v);",
            "ref argument of `f` of unpacked structure type",
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
