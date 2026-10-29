//! Class objects, properties, and methods (IEEE 1800-2023 clause 8).

use super::*;

sv_backends! {
    fn class_imported_member_type_dimensions(sim) { @case "class::imported_member_type_dimensions"; }
    fn class_imported_member_type_inheritance(sim) { @case "class::imported_member_type_inheritance"; }
    fn class_package_constant_class_defaults(sim) { @case "class::package_constant_class_defaults"; }
    fn class_waits_on_static_properties(sim) { @case "class::waits_on_static_properties"; }
    fn class_package_constant_type_arguments(sim) { @case "class::package_constant_type_arguments"; }
    fn class_named_aggregate_type_arguments(sim) { @case "class::named_aggregate_type_arguments"; }
    fn class_generate_scope_enum_type_arguments(sim) { @case "class::generate_scope_enum_type_arguments"; }
    fn class_method_ranges_of_scope_declarations(sim) { @case "class::method_ranges_of_scope_declarations"; }
    fn class_type_alias_members(sim) { @case "class::type_alias_members"; }
    fn class_type_alias_lexical_scopes(sim) { @case "class::type_alias_lexical_scopes"; }
    fn constraint_mode_ordinary_struct_members(sim) {
        @case "class::constraint_mode_ordinary_struct_members";
    }
    fn class_type_parameter_imports(sim) { @case "class::type_parameter_imports"; }
    fn class_type_parameter_shadowing(sim) { @case "class::type_parameter_shadowing"; }
    fn class_objects_properties_and_methods(sim) { @case "class::objects_properties_and_methods"; }
}

sv_backends! {
    fn class_implicit_constructors_and_this(sim) { @case "class::implicit_constructors_and_this"; }
    fn class_objects_referring_to_objects(sim) { @case "class::objects_referring_to_objects"; }
    fn class_extern_methods_and_tasks(sim) { @case "class::extern_methods_and_tasks"; }
    fn class_objects_survive_heap_collections(sim) { @case "class::objects_survive_heap_collections"; }
    fn class_string_methods_on_properties(sim) { @case "class::string_methods_on_properties"; }
    fn class_derived_properties_hide_base_properties(sim) { @case "class::derived_properties_hide_base_properties"; }
    fn class_inherited_properties_and_methods(sim) { @case "class::inherited_properties_and_methods"; }
    fn class_static_properties_and_methods(sim) { @case "class::static_properties_and_methods"; }
    fn class_constructors_chain_to_base(sim) { @case "class::constructors_chain_to_base"; }
    fn class_virtual_methods_dispatch_on_object_class(sim) { @case "class::virtual_methods_dispatch_on_object_class"; }
    fn class_abstract_classes_and_pure_virtual_methods(sim) { @case "class::abstract_classes_and_pure_virtual_methods"; }
    fn class_methods_of_handle_array_elements(sim) { @case "class::methods_of_handle_array_elements"; }
    fn class_classes_implementing_interface_classes(sim) { @case "class::classes_implementing_interface_classes"; }
    fn class_methods_called_through_interface_class_handles(sim) { @case "class::methods_called_through_interface_class_handles"; }
    fn class_inherited_methods_of_extended_interface_classes(sim) { @case "class::inherited_methods_of_extended_interface_classes"; }
    fn class_casts_of_class_handles(sim) { @case "class::casts_of_class_handles"; }
    fn class_casts_from_interface_class_handles(sim) { @case "class::casts_from_interface_class_handles"; }
    fn class_specializations_of_parameterized_classes(sim) { @case "class::specializations_of_parameterized_classes"; }
    fn class_specializations_are_named_by_parameter_values(sim) { @case "class::specializations_are_named_by_parameter_values"; }
    fn class_specializations_with_their_own_handles(sim) { @case "class::specializations_with_their_own_handles"; }
    fn class_class_scoped_parameters_and_types(sim) { @case "class::class_scoped_parameters_and_types"; }
    fn class_specializations_of_interface_classes(sim) { @case "class::specializations_of_interface_classes"; }
    fn class_named_constructor_arguments(sim) { @case "class::named_constructor_arguments"; }
    fn class_named_method_arguments(sim) { @case "class::named_method_arguments"; }
    fn class_typedefs_of_interface_classes(sim) { @case "class::typedefs_of_interface_classes"; }
    fn class_specializations_in_generate_blocks(sim) { @case "class::specializations_in_generate_blocks"; }
    fn class_specializations_keep_their_parameter_scopes(sim) { @case "class::specializations_keep_their_parameter_scopes"; }
    fn class_untyped_class_parameters_take_the_type_of_their_value(sim) { @case "class::untyped_class_parameters_take_the_type_of_their_value"; }
    fn class_class_declarations_bind_in_their_scopes(sim) { @case "class::class_declarations_bind_in_their_scopes"; }
    fn class_parameter_defaults_bind_where_the_class_is_declared(sim) { @case "class::parameter_defaults_bind_where_the_class_is_declared"; }
    fn class_specialization_keys_follow_the_bound_parameters(sim) { @case "class::specialization_keys_follow_the_bound_parameters"; }
    fn class_derived_classes_keep_their_declaration_scopes(sim) { @case "class::derived_classes_keep_their_declaration_scopes"; }
    fn class_shallow_copies(sim) { @case "class::shallow_copies"; }
    fn class_classes_of_packages(sim) { @case "class::classes_of_packages"; }
    fn class_package_classes_by_their_qualified_names(sim) { @case "class::package_classes_by_their_qualified_names"; }
    fn class_classes_of_the_compilation_unit(sim) { @case "class::classes_of_the_compilation_unit"; }
}

sv_backends! {
    // Actuals are evaluated in the order they are written, as for other
    // calls with named arguments.
    fn named_constructor_arguments_are_evaluated_in_written_order(sim) {
        @setup {
            let source = r#"module Top(output int x, output int y);
                class Point;
                    int x;
                    int y;
                    function new(int x = 0, int y = 0);
                        this.x = x;
                        this.y = y;
                    endfunction
                endclass
                int count;
                function int next();
                    count++;
                    return count;
                endfunction
                Point point;
                initial begin
                    point = new(.y(next()), .x(next()));
                    x = point.x;
                    y = point.y;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("x")), 2u32.into());
        assert_eq!(sim.get(sim.signal("y")), 1u32.into());
    }

    fn named_method_arguments_are_evaluated_in_written_order(sim) {
        @setup {
            let source = r#"module Top(output int x, output int y);
                class Point;
                    int x;
                    int y;
                    function void set(int x = 0, int y = 0);
                        this.x = x;
                        this.y = y;
                    endfunction
                endclass
                int count;
                function int next();
                    count++;
                    return count;
                endfunction
                Point point;
                initial begin
                    point = new;
                    point.set(.y(next()), .x(next()));
                    x = point.x;
                    y = point.y;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("x")), 2u32.into());
        assert_eq!(sim.get(sim.signal("y")), 1u32.into());
    }

    // The shape of uvm_factory's `m_matches_type_pair` calls: an
    // out-of-block method bound by name, with a `null` actual. Out of the
    // shared cases because Icarus does not support out-of-block methods and
    // xezim evaluates them wrongly in an instantiated module.
    fn named_arguments_of_out_of_block_methods(sim) {
        @setup {
            let source = r#"module Top(output int hits);
                class Pair;
                    string name;
                    function new(string name);
                        this.name = name;
                    endfunction
                endclass
                class Factory;
                    extern protected function bit matches_type_pair(Pair match_type_pair,
                        Pair requested_type, string requested_type_name);
                    function int find(Pair pair, string name);
                        int found = 0;
                        if (matches_type_pair(.match_type_pair(pair),
                                              .requested_type(null),
                                              .requested_type_name(name)))
                            found += 1;
                        if (matches_type_pair(.requested_type_name("other"),
                                              .requested_type(pair),
                                              .match_type_pair(pair)))
                            found += 10;
                        return found;
                    endfunction
                endclass
                function bit Factory::matches_type_pair(Pair match_type_pair,
                    Pair requested_type, string requested_type_name);
                    return match_type_pair == requested_type
                        || match_type_pair.name == requested_type_name;
                endfunction
                Factory factory;
                Pair pair;
                initial begin
                    factory = new;
                    pair = new("x");
                    hits = factory.find(pair, "x") * 100 + factory.find(pair, "y");
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("factory.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("hits")), 1110u32.into());
    }

    fn null_dereferences_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output int value, output bit done);
                class C;
                    int x;
                endclass
                C c;
                initial begin
                    value = c.x;
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn shallow_copies_of_null_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output bit done);
                class C;
                    int x;
                endclass
                C a;
                C b;
                initial begin
                    b = new a;
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn virtual_calls_through_null_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output int value, output bit done);
                class C;
                    virtual function int f();
                        return 1;
                    endfunction
                endclass
                C c;
                initial begin
                    value = c.f();
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn checkpoints_keep_objects_that_state_files_cannot_record(sim) {
        @setup {
            let source = r#"module Top(output int value);
                timeunit 1ps / 1ps;
                class C;
                    int x;
                endclass
                C c;
                initial begin
                    c = new;
                    c.x = 7;
                    #10 value = c.x;
                    #10 c.x = 8;
                    #10 value = c.x;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("checkpoint.sv"))], "Top");
        sim.run_until(5).unwrap();
        let error = sim.save_state().unwrap_err().to_string();
        assert!(error.contains("class objects"), "{error}");
        let checkpoint = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(8u8));
        sim.restore(&checkpoint).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get(value), BigUint::from(7u8));
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(8u8));
    }
}

sv_backends! {
    fn class_four_state_properties(sim) { @case "class::four_state_properties"; }

    fn four_state_properties_are_two_state_in_two_state_mode(sim) {
        @setup {
            let source = r#"module Top(output longint t, output bit [3:0] l, output bit [3:0] k);
                class C;
                    time t;
                    logic [3:0] l;
                endclass
                C c;
                initial begin
                    c = new;
                    t = c.t;
                    l = c.l;
                    c.l = 4'b1z0x;
                    k = c.l;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
            .four_state(false);
        sim.run_until(0).unwrap();
        for (name, value) in [("t", 0u8), ("l", 0), ("k", 0b1000)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }
}

/// A write of a class property does not wake a process waiting on it, so a
/// wait whose condition reads one is rejected instead of never waking. The
/// condition reads it in a method through `this`, through a handle, through
/// a queue held in a property and through a function.
#[test]
fn waits_on_class_properties_are_unsupported() {
    let waits = [
        "wait (m_state >= s);",
        "wait (other.m_state >= s);",
        "wait (q.size() != 0);",
        "wait (reached(s));",
    ];
    for wait in waits {
        let source = format!(
            "class C;
                int m_state;
                int q[$];
                C other;
                function bit reached(int s); return m_state >= s; endfunction
                task wait_for(int s); {wait} $display(\"reached %0d at %0d\", s, $time); endtask
            endclass
            module Top;
                initial begin
                    automatic C c = new;
                    c.other = c;
                    fork
                        c.wait_for(2);
                        begin #1 c.m_state = 1; #1 c.m_state = 2; c.q.push_back(1); end
                    join
                end
            endmodule"
        );
        for four_state in [false, true] {
            let builder = || {
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("class.sv"))],
                    "Top",
                )
                .four_state(four_state)
            };
            let errors = [
                builder().build_native().err(),
                builder().build_cranelift().err(),
                builder().build_wasm().err(),
                builder().build_interpreter().err(),
            ];
            for error in errors {
                let error = error
                    .unwrap_or_else(|| panic!("{wait} must be rejected"))
                    .to_string();
                assert!(error.contains("in a wait condition"), "{wait}: {error}");
            }
        }
    }
}

/// The default of an argument is read only where a call omits the argument,
/// so a property-reading default does not reject a wait that supplies it.
#[test]
fn waits_read_property_defaults_only_when_omitted() {
    let source = |call: &str| {
        format!(
            "`timescale 1ns/1ns
            class C; int state; endclass
            module Top(output int seen);
                C c;
                int signal;
                function automatic bit ready(int x = c.state); return x != 0; endfunction
                initial begin
                    c = new;
                    fork
                        begin wait ({call}); seen = $time; end
                        begin #2 signal = 1; end
                    join
                end
            endmodule"
        )
    };
    let supplied = source("ready(signal)");
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(supplied.as_str(), Path::new("class.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    sim.run_until(5).unwrap();
    let seen = sim.signal("seen");
    assert_eq!(sim.get(seen), BigUint::from(2u8));
    let omitted = source("ready()");
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(omitted.as_str(), Path::new("class.sv"))], "Top")
            .build_interpreter()
    else {
        panic!("a wait reading a property through a default is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("in a wait condition"), "{error}");
}

#[test]
fn randomizing_four_state_properties_is_rejected() {
    for (body, want) in [
        (
            "class C; rand logic [3:0] x; endclass C c; initial begin c = new; void'(c.randomize()); end",
            "randomization of a 4-state class property",
        ),
        (
            "class C; rand bit [3:0] x; logic [3:0] y; constraint k { x < y; } endclass C c; initial begin c = new; void'(c.randomize()); end",
            "4-state class property in a constraint",
        ),
        (
            "class C; logic [3:0] x; rand bit [3:0] y; constraint k { y < 5; } endclass C c; initial begin c = new; void'(c.randomize(x)); end",
            "randomization of a 4-state class property",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        for four_state in [false, true] {
            let Err(error) =
                Simulator::from_sv_sources(vec![(&source, Path::new("class.sv"))], "Top")
                    .four_state(four_state)
                    .build_cranelift()
            else {
                panic!("{want} is unsupported");
            };
            let error = error.to_string();
            assert!(error.contains(want), "{error}");
        }
    }
}

#[test]
fn misplaced_super_new_static_misuse_and_unimplemented_methods_are_rejected() {
    for (body, want) in [
        (
            "class B; endclass class C extends B; int x; function new(); x = 1; super.new(); endfunction endclass",
            "`super.new` that is not the first statement",
        ),
        (
            "class C; int x; static function int f(); return x; endfunction endclass",
            "non-static property `x` in a static method",
        ),
        (
            "class C; function int g(); return 1; endfunction static function int f(); return g(); endfunction endclass",
            "non-static method `g` called from a static method",
        ),
        (
            "virtual class C; pure virtual function int f(); endclass class D extends C; endclass",
            "does not implement pure virtual method `f`",
        ),
        (
            "virtual class B; pure virtual function int f(); endclass class C extends B; function int f(); return super.f(); endfunction endclass",
            "pure virtual method `f` called through `super`",
        ),
        ("virtual class C; endclass", "object of abstract class `C`"),
        (
            "interface class I; pure virtual function int f(); endclass virtual class B implements I; endclass class C extends B; endclass",
            "does not implement interface class method `f`",
        ),
        (
            "interface class I; pure virtual function int f(); endclass interface class J; pure virtual function void f(); endclass class C implements I, J; function int f(); return 1; endfunction endclass",
            "whose return type differs from its interface class",
        ),
    ] {
        let source = format!("module Top; {body} C c; initial c = new; endmodule");
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{body}` is rejected");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{error}");
    }
}

#[test]
fn incompatible_class_handle_assignments_are_rejected() {
    for (statement, want) in [
        ("d = b;", ["handle of class `B`", "class `D`"]),
        ("d = u;", ["handle of class `U`", "class `D`"]),
        ("i = b;", ["handle of class `B`", "class `I`"]),
        ("b = U::new;", ["handle of class `U`", "class `B`"]),
        ("d.next = b;", ["handle of class `B`", "class `D`"]),
        ("d = new b;", ["handle of class `B`", "class `D`"]),
        ("u = new d;", ["handle of class `D`", "class `U`"]),
        ("$cast(d.next, b);", ["`$cast` to a class property", ""]),
    ] {
        let source = format!(
            "module Top;
                interface class I; endclass
                class B; endclass
                class D extends B implements I; D next; endclass
                class U; endclass
                B b; D d; U u; I i;
                initial begin d = new; {statement} end
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{statement}` is rejected");
        };
        let error = error.to_string();
        assert!(want.iter().all(|want| error.contains(want)), "{error}");
    }
}

#[test]
fn misused_parameterized_classes_are_rejected() {
    for (items, want) in [
        (
            "Box #(2) two; Box #(3) three; initial begin three = new; two = three; end",
            ["handle of class `Box#(N=3)`", "class `Box#(N=2)`"],
        ),
        (
            "initial $display(Box::N);",
            ["`Box::` outside class `Box`", "`Box#()::`"],
        ),
        (
            "Box #(.M(1)) other;",
            ["class `Box` has no parameter `M`", ""],
        ),
        (
            "interface class Get #(type T = int); pure virtual function T get(); endclass
             interface class Both extends Get#(int), Get#(bit); endclass",
            ["interface class `Both`", "inherits `T` from both"],
        ),
        (
            "class Self #(int N = 0) extends Self#(N + 1); endclass Self self;",
            ["nested more than 64 deep", "class `Self`"],
        ),
        (
            "class Sized #(parameter V = 1); endclass Sized #(8'h1) a; Sized #(32'h1) b;
             initial begin b = new; a = b; end",
            ["class `Sized#(V=32'd1)`", "class `Sized#(V=8'd1)`"],
        ),
        (
            "Box #(.N(3), 4) mixed;",
            ["positional parameter value after a named one", ""],
        ),
        (
            "class Point; function new(int x = 0); endfunction endclass Point p;
             initial p = new(.x(), .x(1));",
            ["formal `x` is bound twice", "13.5.4"],
        ),
        (
            "Box #(.N(), .N(2)) twice;",
            ["duplicate parameter assignment `N`", ""],
        ),
        (
            "class Plain; typedef int T; endclass Plain#(123)::T value;",
            ["class `Plain`, which has no parameters", ""],
        ),
        (
            "class Typed #(type T = int); endclass Typed#(enum {A, B}) typed;",
            ["enumeration type as a class type parameter value", ""],
        ),
        (
            "class Typed #(type T = int); endclass Typed#(struct packed { int x; }) typed;",
            ["struct or union type as a class type parameter value", ""],
        ),
        (
            // Each iteration would declare a class of its own (#2064).
            "for (genvar i = 0; i < 2; i++) begin : g
                 localparam int K = i + 1;
                 class C #(int N = K); endclass
                 initial $display(\"%0d\", C#()::N);
             end",
            ["class `C` declared in a loop generate block", ""],
        ),
        (
            // The class parameter hides the module's `N`, whose value must
            // not name `Four#(1)` (#1964 tracks four-state actuals).
            "localparam int N = 1; class Four #(logic [1:0] N = 0); endclass
             class Shadow #(logic [1:0] N = 2'bxx); function void make(); Four#(N) four; endfunction endclass
             Shadow#() shadow;",
            ["class parameter value that is not constant", ""],
        ),
    ] {
        let source = format!(
            "module Top;
                class Box #(int N = 4); endclass
                {items}
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{items}` is rejected");
        };
        let error = error.to_string();
        assert!(want.iter().all(|want| error.contains(want)), "{error}");
    }
}

/// Calls whose actuals cannot be bound to the formals are compile errors
/// (IEEE 1800-2023 13.5.3, 13.5.4), for methods as for other subroutines.
#[test]
fn unbindable_call_arguments_are_rejected() {
    for (call, want) in [
        ("c.f(.a(1), .a(2))", "formal `a` is bound twice"),
        ("c.f(1, .a(2))", "formal `a` is bound twice"),
        ("c.f(.q(1))", "no formal named `q`"),
        ("c.f(.b(1), 3)", "a positional actual after a named one"),
        ("c.f(1, 2, 3)", "3 actuals for 2 formals"),
        ("c.f(.b(1))", "formal `a` has no default"),
        ("c.f(, 1)", "formal `a` has no default"),
        ("c.f(.a(), .b(1))", "formal `a` has no default"),
        ("C::s(.z(1))", "no formal named `z`"),
        ("f(.a(1), .a(2))", "formal `a` is bound twice"),
        ("f(.z(1))", "no formal named `z`"),
        ("f(.b(1), 2)", "a positional actual after a named one"),
        ("f(.b(1))", "formal `a` has no default"),
        ("f(1, 2, 3)", "3 actuals for 2 formals"),
    ] {
        let source = format!(
            "module Top;
                class C;
                    function int f(int a, int b = 2); return a + b; endfunction
                    static function int s(int a = 1); return a; endfunction
                endclass
                function automatic int f(int a, int b = 2); return a + b; endfunction
                C c = new;
                initial $display(\"%0d\", {call});
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("call.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{call}` is accepted");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{call}: {error}");
    }
}

sv_backends! {
    fn class_unconstrained_randomize(sim) { @case "class::unconstrained_randomize"; }
    fn class_randomize_without_rand_members(sim) { @case "class::randomize_without_rand_members"; }
    fn class_randomize_null_receiver(sim) {
        @setup {
            let source = r#"module Top(output int hooks, result);
                class Empty;
                    static int calls;
                    function void pre_randomize(); calls++; endfunction
                    function void post_randomize(); calls++; endfunction
                endclass
                Empty object;
                initial begin
                    result = object.randomize();
                    hooks = Empty::calls;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null_randomize.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
        assert_eq!(sim.get(sim.signal("hooks")), 0u8.into());
        assert_eq!(sim.get(sim.signal("result")), 0u8.into());
    }
}

sv_backends! {
    fn class_randomize_rejects_unsupported_dynamic_constraints(sim) {
        @setup {
            let source = r#"module Top(output int value, hooks, result);
                class Base;
                    rand int x = 77;
                    static int calls;
                    function void pre_randomize(); calls++; endfunction
                    function void post_randomize(); calls++; endfunction
                endclass
                class Derived extends Base;
                    constraint c { x dist {1 := 2}; }
                endclass
                Base b;
                Derived d;
                initial begin
                    d = new;
                    b = d;
                    result = b.randomize();
                    value = b.x;
                    hooks = Base::calls;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("base_randomize.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("randomization through a base handle"), "{error}");
        assert_eq!(sim.get(sim.signal("value")), 77u8.into());
        assert_eq!(sim.get(sim.signal("hooks")), 0u8.into());
        assert_eq!(sim.get(sim.signal("result")), 0u8.into());
    }
}

sv_backends! {
    fn class_randomize_checkpoint_four_state(sim) {
        @setup {
            let source = r#"module Top(output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; rand bit [63:0] x; endclass
                C c;
                initial begin
                    c = new;
                    c.srandom(44);
                    void'(c.randomize());
                    value = c.x;
                    #10;
                    void'(c.randomize());
                    value = c.x;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("random_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(15).unwrap();
        let expected = sim.get_four_state(value);
        sim.restore(&checkpoint).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get_four_state(value), expected);
    }
}

sv_backends! {
    fn class_constructor_checkpoint_restores_process_rng(sim) {
        @setup { let source = r#"
            module Top(input bit clk, output bit [63:0] value);
                class C; rand bit [63:0] x; endclass
                C c;
                always @(posedge clk) begin
                    c = new;
                    void'(c.randomize());
                    value = c.x;
                end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constructor_rng.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        let value = sim.signal("value");
        sim.tick(clk).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let mut expected = Vec::new();
        for _ in 0..4 {
            sim.tick(clk).unwrap();
            expected.push(sim.get_four_state(value));
        }
        assert!(expected.windows(2).any(|pair| pair[0] != pair[1]));
        sim.restore(&checkpoint).unwrap();
        for expected in expected {
            sim.tick(clk).unwrap();
            assert_eq!(sim.get_four_state(value), expected);
        }
    }
}

sv_backends! {
    fn class_randomization_mode_bit_argument(sim) { @case "class::randomization_mode_bit_argument"; }
    fn class_randomization_inheritance(sim) { @case "class::randomization_inheritance"; }
    fn class_randomization_modes_and_selection(sim) { @case "class::randomization_modes_and_selection"; }
    fn class_randomization_pre_hook_controls_modes(sim) { @case "class::randomization_pre_hook_controls_modes"; }
}

sv_backends! {
    fn class_random_modes_checkpoint_four_state(sim) {
        @setup { let source = r#"
            module Top(input bit enable, output int mode, output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; rand bit [63:0] x = 17; endclass
                C c;
                initial begin
                    c = new;
                    c.x.rand_mode(0);
                    #10 if (enable) c.x.rand_mode(1);
                    #10;
                    void'(c.randomize());
                    value = c.x;
                    mode = c.x.rand_mode();
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("mode_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let enable = sim.signal("enable");
        sim.modify(|io| io.set(enable, 1u8)).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 1u8.into());
        sim.restore(&saved).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 0u8.into());
        assert_eq!(sim.get_four_state(sim.signal("value")), (17u8.into(), 0u8.into()));
    }

    fn class_random_selection_spans_multiple_words(sim) {
        @setup {
            let properties = (0..70).map(|i| format!("p{i}")).collect::<Vec<_>>().join(", ");
            let source = format!(r#"
                module Top(output bit done, output int errors);
                    class C;
                        bit [63:0] {properties};
                    endclass
                    C c, reference;
                    initial begin
                        c = new; reference = new;
                        c.srandom(71); reference.srandom(71);
                        if (!c.randomize(p69) || !reference.randomize(p0)) errors++;
                        if (c.p69 != reference.p0 || c.p0 != 0 || c.p68 != 0) errors++;
                        done = 1;
                    end
                endmodule
            "#);
        }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("wide_selection.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("done")), 1u8.into());
        assert_eq!(sim.get(sim.signal("errors")), 0u8.into());
    }
}

#[test]
fn classes_of_packages_in_other_sources_are_used_through_their_layouts() {
    let package = "package shapes;
        class Shape;
            int id;
            static int made;
            function new(int id); this.id = id; made++; endfunction
            virtual function int area(); return 0; endfunction
        endclass
        class Square extends Shape;
            int side;
            function new(int side); super.new(side + 1); this.side = side; endfunction
            virtual function int area(); return side * side; endfunction
        endclass
    endpackage";
    let module = "module Top(output int area, output int id, output int made);
        import shapes::*;
        Shape shape;
        initial begin
            shape = Square::new(4);
            area = shape.area();
            id = shape.id;
            made = shapes::Shape::made;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("shapes.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("area")), 16u32.into());
    assert_eq!(sim.get(sim.signal("id")), 5u32.into());
    assert_eq!(sim.get(sim.signal("made")), 1u32.into());
}

#[test]
fn compilation_unit_types_of_separate_files_specialize_package_classes_separately() {
    // Celox makes each source file a compilation unit of its own (IEEE
    // 1800-2023 3.12.1 lets a tool choose this), so the same typedef in two
    // files names two types and two specializations of `p::Counter`.
    let package = "`timescale 1ns/1ns
    package p;
        class Counter #(type T = int);
            static int count;
            function new(); count++; endfunction
        endclass
    endpackage";
    let a = "`timescale 1ns/1ns
    typedef enum {A0, A1} E;
    module A(output int n);
        initial begin
            automatic p::Counter#(E) c = new();
            #1 n = p::Counter#(E)::count;
        end
    endmodule";
    let b = "`timescale 1ns/1ns
    typedef enum {B0, B1} E;
    module B(output int n);
        initial begin
            automatic p::Counter#(E) c = new();
            automatic p::Counter#(E) d = new();
            #1 n = p::Counter#(E)::count;
        end
    endmodule";
    let top = "`timescale 1ns/1ns
    module Top(output int a, output int b);
        A ua(.n(a));
        B ub(.n(b));
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("package.sv")),
            (a, Path::new("a.sv")),
            (b, Path::new("b.sv")),
            (top, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("a")), 1u32.into());
    assert_eq!(sim.get(sim.signal("b")), 2u32.into());
}

#[test]
fn a_class_of_another_module_does_not_hide_a_package() {
    let source = "package P; typedef bit [2:0] T; endpackage
        module Other; class P; typedef int T; endclass endmodule
        module Top(output int bits); P::T value; assign bits = $bits(value); endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("bits")), 3u32.into());
}

#[test]
fn processes_of_several_instances_share_the_static_properties_of_a_package_class() {
    let source = "`timescale 1ns/1ns
        package p;
            class C;
                static int made;
                function new(); made++; endfunction
            endclass
        endpackage
        module W; p::C c; initial c = new; endmodule
        module Top(output int made);
            W first();
            W second();
            initial #1 made = p::C::made;
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("made")), 2u32.into());
}

#[test]
fn type_arguments_keep_their_dimensions_scopes_and_class_scoped_types() {
    let source = "module Top(output int defs, output int narrow, output int wide);
        class Defs; typedef bit [7:0] T; endclass
        class Box #(type T = int);
            T value;
            function int bits(); return $bits(value); endfunction
        endclass
        Box#(Defs::T) box;
        for (genvar i = 2; i < 4; i++) begin : g
            Box#(bit [i:0]) sized;
            initial begin
                sized = new;
                if (i == 2) narrow = sized.bits(); else wide = sized.bits();
            end
        end
        initial begin box = new; defs = box.bits(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("defs")), 8u32.into());
    assert_eq!(sim.get(sim.signal("narrow")), 3u32.into());
    assert_eq!(sim.get(sim.signal("wide")), 4u32.into());
    // Arrays of strings of two sizes are two types (IEEE 1800-2023 6.22).
    let source = "module Top;
        class Box #(type T = int); endclass
        typedef string A [2];
        typedef string B [3];
        Box#(A) a;
        Box#(B) b;
        initial begin b = new; a = b; end
    endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build_cranelift()
    else {
        panic!("handles of two specializations are not assignment compatible");
    };
    assert!(error.to_string().contains("a handle of class"), "{error}");
}

#[test]
fn classes_of_packages_in_other_sources_keep_their_behaviour() {
    let package = r#"package p;
  class B; virtual function int f(); return 1; endfunction endclass
  interface class I; pure virtual function int id(); endclass
  class R; rand int x; int hooked; function void pre_randomize(); hooked = 5; endfunction endclass
  class P #(int N = 1);
    static function int f(); return P::N; endfunction
  endclass
  typedef P#(2) P2;
  class Plain; function int v(); return 9; endfunction endclass
  typedef Plain Alias;
  interface class Getter #(type T = int); pure virtual function T get(); endclass
endpackage
package q;
  class C; extern function int f(); endclass
  function int C::f(); return 2; endfunction
endpackage
package r;
  class C; extern function int f(); endclass
  function int C::f(); return 3; endfunction
endpackage"#;
    let module = r#"package w1; int p; endpackage
package w2; int p; endpackage
module Top(output int cast, output int over, output int iface, output int hooked, output int special, output int alias_v, output int qf, output int rf, output int got);
  import w1::*;
  import w2::*;
  class D extends p::B; virtual function int f(); return 4; endfunction endclass
  class M implements p::I; virtual function int id(); return 6; endfunction endclass
  interface class Getter #(type T = int); pure virtual function T get(); endclass
  class G implements Getter#(int); virtual function int get(); return 8; endfunction endclass
  p::B b;
  D d;
  M m;
  p::R rr;
  p::Alias al;
  q::C qc;
  r::C rc;
  G g;
  initial begin
    d = new;
    m = new;
    rr = new;
    void'(rr.randomize());
    al = p::Alias::new;
    qc = new; rc = new; g = new;
    cast = $cast(b, d);
    over = b.f();
    iface = m.id();
    hooked = rr.hooked;
    special = p::P2::f();
    alias_v = al.v();
    qf = qc.f();
    rf = rc.f();
    got = g.get();
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("packages.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    for (signal, value) in [
        ("cast", 1u32),
        ("over", 4),
        ("iface", 6),
        ("hooked", 5),
        ("special", 2),
        ("alias_v", 9),
        ("qf", 2),
        ("rf", 3),
        ("got", 8),
    ] {
        assert_eq!(sim.get(sim.signal(signal)), value.into(), "{signal}");
    }
}

#[test]
fn specializations_tell_types_apart_and_keep_their_bases() {
    let source = r#"module Top(output int packed_made, output int unpacked_made, output int width, output int base_n);
  localparam int K = 2;
  class Box #(type T = int); static int made; function new(); made++; endfunction endclass
  typedef bit [1:0] A;
  typedef bit B [1:0];
  class Inner #(int N = 1); typedef bit [N:0] T; endclass
  class Outer #(int N = 1);
    function int w(); Inner#(N)::T v; return $bits(v); endfunction
  endclass
  class Base #(int N = 0); localparam int K = 1; function int n(); return N; endfunction endclass
  class Derived extends Base#(K); endclass
  Box#(A) a; Box#(B) b;
  Outer#(2) o;
  Derived d;
  initial begin
    a = new; b = new; o = new; d = new;
    packed_made = Box#(A)::made;
    unpacked_made = Box#(B)::made;
    width = o.w();
    base_n = d.n();
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    // `bit [1:0]` and `bit [1:0]` unpacked are two types (IEEE 1800-2023 6.22).
    assert_eq!(sim.get(sim.signal("packed_made")), 1u32.into());
    assert_eq!(sim.get(sim.signal("unpacked_made")), 1u32.into());
    assert_eq!(sim.get(sim.signal("width")), 3u32.into());
    // `extends Base#(K)` names the module's `K`, not the one `Base` declares.
    assert_eq!(sim.get(sim.signal("base_n")), 2u32.into());
}

#[test]
fn classes_keep_the_identity_and_signatures_of_their_scopes() {
    let first = r#"`timescale 1ns/1ns
class C; bit [7:0] x; endclass
module A(output int a);
  C c;
  initial begin c = new; c.x = 300; a = c.x; end
endmodule"#;
    let second = r#"`timescale 1ns/1ns
package p;
  parameter int N = 8;
  class B; virtual function int f(); return 1; endfunction static function bit [N-1:0] g(); return '1; endfunction endclass
endpackage
package q;
  class D extends p::B; virtual function int f(); return 5; endfunction endclass
endpackage
class C; bit [15:0] x; endclass
module Top(output int narrow, output int wide, output int over, output int bits);
  parameter int N = 16;
  int a;
  A ua(.a(a));
  C c;
  p::B b;
  q::D d;
  initial begin
    c = new; c.x = 300; d = new; b = d;
    #1;
    narrow = a;
    wide = c.x;
    over = b.f();
    bits = $bits(p::B::g());
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (first, Path::new("first.sv")),
            (second, Path::new("second.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    // Each source's compilation-unit class `C` is its own (3.12.1).
    assert_eq!(sim.get(sim.signal("narrow")), 44u32.into());
    assert_eq!(sim.get(sim.signal("wide")), 300u32.into());
    // A subclass of another package reaches its override.
    assert_eq!(sim.get(sim.signal("over")), 5u32.into());
    // A package method keeps the width of its package's parameter.
    assert_eq!(sim.get(sim.signal("bits")), 8u32.into());
}

sv_backends! {
    fn class_randomization_enum_inheritance(sim) { @case "class::randomization_enum_inheritance"; }
    fn class_randomization_enum_alias(sim) { @case "class::randomization_enum_alias"; }
    fn class_randomization_enum_domain(sim) { @case "class::randomization_enum_domain"; }
}

sv_backends! {
    fn class_randomization_enum_fill_literals(sim) { @case "class::randomization_enum_fill_literals"; }
}

// The methods and static properties a module class inherits from a class of
// a package in another source keep what they are there: a source-local
// method identifier does not make a method of the module static, and a
// static property keeps the width the package's constants give it.
#[test]
fn inherited_members_of_package_classes_keep_their_declarations() {
    let package = r#"package p;
  localparam int N = 8;
  class B;
    static bit [N-1:0] x;
    static function int g(); return 1; endfunction
  endclass
endpackage"#;
    let module = r#"module Top(output int value, output int width);
  localparam int N = 16;
  class D extends p::B;
    int y;
    function int f(); y = 3; return y + $bits(x); endfunction
  endclass
  D d;
  initial begin
    d = new;
    value = d.f();
    width = $bits(p::B::x);
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("packages.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("value")), 11u32.into());
    assert_eq!(sim.get(sim.signal("width")), 8u32.into());
}

sv_backends! {
    fn class_randomization_cyclic_enum_alias(sim) { @case "class::randomization_cyclic_enum_alias"; }
    fn class_randomization_cyclic_domains(sim) { @case "class::randomization_cyclic_domains"; }
    fn class_randomization_cyclic_controls(sim) { @case "class::randomization_cyclic_controls"; }
    fn class_randomization_cyclic_checkpoint(sim) {
        @setup { let source = r#"
            module Top(output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; randc bit [2:0] x; endclass
                C c;
                initial begin
                    c = new;
                    c.srandom(41);
                    void'(c.randomize());
                    #10;
                    repeat (16) begin
                        void'(c.randomize());
                        value = (value << 3) | c.x;
                    end
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("cyclic_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(15).unwrap();
        let expected = sim.get_four_state(value);
        sim.restore(&saved).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get_four_state(value), expected);
    }
}

sv_backends! {
    fn constrained_randomize_scalar(sim) { @case "class::constrained_randomize_scalar"; }
    fn constrained_randomize_width_and_inheritance(sim) { @case "class::constrained_randomize_width_and_inheritance"; }
}

sv_backends! {
    fn constrained_randomize_hooks(sim) { @case "class::constrained_randomize_hooks"; }
}

sv_backends! {
    fn constrained_randomize_inside(sim) { @case "class::constrained_randomize_inside"; }
    fn constrained_randomize_inside_widths(sim) { @case "class::constrained_randomize_inside_widths"; }
}

sv_backends! {
    fn constrained_randomize_conditionals(sim) { @case "class::constrained_randomize_conditionals"; }
    fn constrained_randomize_nested_guards(sim) { @case "class::constrained_randomize_nested_guards"; }
}

sv_backends! {
    fn constrained_randomize_inline(sim) { @case "class::constrained_randomize_inline"; }
    fn constrained_randomize_inline_inheritance(sim) { @case "class::constrained_randomize_inline_inheritance"; }
}

sv_backends! {
    fn constrained_randomize_inline_basic(sim) { @case "class::constrained_randomize_inline_basic"; }
}

sv_backends! {
    fn constrained_randomize_inline_package_instances(sim) { @case "class::constrained_randomize_inline_package_instances"; }
}

sv_backends! {
    fn constrained_randomize_inline_inputs(sim) { @case "class::constrained_randomize_inline_inputs"; }
    fn constrained_randomize_inline_input_methods(sim) { @case "class::constrained_randomize_inline_input_methods"; }
}

sv_backends! {
    fn constrained_randomize_inline_input_hooks(sim) { @case "class::constrained_randomize_inline_input_hooks"; }
}

sv_backends! {
    fn constraint_mode_basic(sim) { @case "class::constraint_mode_basic"; }
    fn constraint_mode_inheritance(sim) { @case "class::constraint_mode_inheritance"; }
    fn constraint_mode_hooks(sim) { @case "class::constraint_mode_hooks"; }
}

sv_backends! {
    fn constraint_mode_checkpoint(sim) {
        @setup { let source = r#"
            module Top(input bit enable, output int mode, output bit [3:0] value);
                timeunit 1ps / 1ps;
                class C;
                    rand bit [3:0] x;
                    constraint c { x == 2; }
                endclass
                C p;
                initial begin
                    p = new;
                    p.constraint_mode(0);
                    #10 if (enable) p.c.constraint_mode(1);
                    #10;
                    mode = p.c.constraint_mode();
                    if (mode) begin
                        if (!p.randomize()) $fatal(1, "enabled solve");
                    end else begin
                        if (!(p.randomize() with { x == 7; })) $fatal(1, "restored solve");
                    end
                    value = p.x;
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("constraint_mode_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let enable = sim.signal("enable");
        sim.modify(|io| io.set(enable, 1u8)).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 1u8.into());
        assert_eq!(sim.get(sim.signal("value")), 2u8.into());
        sim.restore(&saved).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 0u8.into());
        assert_eq!(sim.get(sim.signal("value")), 7u8.into());
    }
}

sv_backends! {
    fn constraint_mode_base_dispatch(sim) { @case "class::constraint_mode_base_dispatch"; }
}

sv_backends! {
    fn class_foreign_generic_static_state_across_modules(sim) { @case "class::foreign_generic_static_state_across_modules"; }
    fn class_foreign_generic_unit_type_constructor(sim) { @case "class::foreign_generic_unit_type_constructor"; }
    fn class_foreign_generic_static_handle_identity(sim) { @case "class::foreign_generic_static_handle_identity"; }
}

sv_backends! {
    fn class_foreign_generic_declaration_scope_and_equivalent_defaults(sim) { @case "class::foreign_generic_declaration_scope_and_equivalent_defaults"; }
}

sv_backends! {
    fn class_foreign_generic_multiple_user_classes(sim) { @case "class::foreign_generic_multiple_user_classes"; }
}

sv_backends! {
    fn class_foreign_generic_nominal_and_nested_types(sim) { @case "class::foreign_generic_nominal_and_nested_types"; }
}

#[test]
fn foreign_generic_native_images_and_checkpoints_preserve_shared_handles() {
    let source = r#"`timescale 1ps/1ps
        package p;
            class Slot #(type T);
                static T saved;
                static function void put(T object); saved=object; endfunction
            endclass
        endpackage
        class Item;
            int value;
            function new(); value=7; endfunction
        endclass
        module Writer;
            Item object;
            initial begin
                object=new(); p::Slot#(Item)::put(object);
                #10; object.value=21;
                #10; object.value++;
            end
        endmodule
        module Reader(output int value);
            initial begin
                #15; value=p::Slot#(Item)::saved.value;
                #10; value=p::Slot#(Item)::saved.value;
            end
        endmodule
        module Top(output int value);
            Writer writer(); Reader reader(value);
        endmodule
    "#;
    for four_state in [false, true] {
        let builder = || {
            celox::Simulation::from_sv_sources(
                vec![(source, Path::new("foreign_checkpoint.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let mut interpreted = builder().build_interpreter().unwrap();
        interpreted.run_until(30).unwrap();
        assert_eq!(
            interpreted.get(interpreted.signal("value")),
            BigUint::from(22u8)
        );
        let image = builder().compile_native().unwrap().into_program_image();
        let image =
            celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let mut sim = celox::Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap();
        sim.run_until(15).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(21u8));
        let bytes = sim.checkpoint().unwrap().to_bytes().unwrap();
        // SAFETY: bytes were created by this simulator.
        let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        sim.run_until(30).unwrap();
        assert_eq!(sim.get(value), BigUint::from(22u8));
        sim.restore(&checkpoint).unwrap();
        sim.run_until(30).unwrap();
        assert_eq!(sim.get(value), BigUint::from(22u8));
    }
}

// This API chooses one compilation unit per source file (3.12.1).
sv_backends! {
    fn foreign_generic_distinct_unit_classes(sim) {
        @setup { let sources = vec![
            (r#"
package p;
 class Registry #(type T);
  static int count=0;
  static function T create(); T object; object=new(); count++; return object; endfunction
 endclass
endpackage"#, Path::new("registry.sv")),
            (r#"
import p::*;
class User;
 int value;
 function new(); value=42; endfunction
 typedef Registry#(User) type_id;
endclass
module First(output int result, count);
 User object;
 initial begin object=User::type_id::create(); result=object.value; count=User::type_id::count; end
endmodule"#, Path::new("first.sv")),
            (r#"
import p::*;
class User;
 int padding;
 int value;
 function new(); padding=99; value=73; endfunction
 typedef Registry#(User) type_id;
endclass
module Second(output int result, count);
 User object;
 initial begin object=User::type_id::create(); result=object.value; count=User::type_id::count; end
endmodule"#, Path::new("second.sv")),
            (r#"
module Top(output int first, second, first_count, second_count);
 First a(first, first_count); Second b(second, second_count);
endmodule"#, Path::new("top.sv")),
        ]; }
        @build celox::Simulation::from_sv_sources(sources, "Top");
        sim.run_until(0).unwrap();
        for (name, expected) in [("first", 42u32), ("second", 73), ("first_count", 1), ("second_count", 1)] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(expected), "{name}");
        }
    }
}

sv_backends! {
    fn class_foreign_generic_inherited_constants(sim) { @case "class::foreign_generic_inherited_constants"; }
}

sv_backends! {
    fn class_foreign_generic_constructor_lexical_scope(sim) { @case "class::foreign_generic_constructor_lexical_scope"; }
    fn class_foreign_generic_package_type_static_storage(sim) { @case "class::foreign_generic_package_type_static_storage"; }
    fn class_foreign_generic_template_of_the_named_package(sim) { @case "class::foreign_generic_template_of_the_named_package"; }
    fn class_foreign_generic_in_a_module_with_interface_instances(sim) { @case "class::foreign_generic_in_a_module_with_interface_instances"; }
}
