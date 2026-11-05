//! Class objects, properties, and methods (IEEE 1800-2023 clause 8).

use super::*;

sv_backends! {
    fn class_imported_member_type_dimensions(sim) { @case "class::imported_member_type_dimensions"; }
    fn class_members_hide_module_names(sim) { @case "class::members_hide_module_names"; }
    fn class_imported_member_type_inheritance(sim) { @case "class::imported_member_type_inheritance"; }
    fn class_package_constant_class_defaults(sim) { @case "class::package_constant_class_defaults"; }
    fn class_waits_on_static_properties(sim) { @case "class::waits_on_static_properties"; }
    fn class_package_constant_type_arguments(sim) { @case "class::package_constant_type_arguments"; }
    fn class_named_aggregate_type_arguments(sim) { @case "class::named_aggregate_type_arguments"; }
    fn class_enumeration_constants_of_classes(sim) { @case "class::enumeration_constants_of_classes"; }
    fn class_container_type_aliases_of_classes(sim) { @case "class::container_type_aliases_of_classes"; }
    fn class_members_of_call_results(sim) { @case "class::members_of_call_results"; }
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
    fn class_casts_to_class_properties(sim) { @case "class::casts_to_class_properties"; }
    fn class_void_casts_of_class_handles(sim) { @case "class::void_casts_of_class_handles"; }
    fn class_failing_casts_evaluate_their_sources(sim) { @case "class::failing_casts_evaluate_their_sources"; }
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
    fn class_specializations_of_procedural_constants(sim) { @case "class::specializations_of_procedural_constants"; }
    fn class_specializations_of_procedural_aggregate_types(sim) { @case "class::specializations_of_procedural_aggregate_types"; }
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

/// A static method called without its empty parentheses may be the receiver
/// of a member, and a method that returns a chandle so called gives a chandle
/// (IEEE 1800-2023 13.5.5).
#[test]
fn parenless_static_factories_and_chandle_results() {
    let source = r#"
    module Top(output int latest_id, output int latest_value, output bit empty);
        class Node;
            static Node last;
            int id;
            chandle ch;
            function new(int id); this.id = id; last = this; endfunction
            function int get_id(); return id; endfunction
            function chandle get_handle(); return ch; endfunction
            static function Node latest(); return last; endfunction
        endclass
        Node n;
        chandle h;
        initial begin
            n = new(3);
            n = new(5);
            latest_id = Node::latest.get_id();
            latest_value = Node::latest.id;
            h = n.get_handle;
            empty = h == null;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let latest_id = sim.signal("latest_id");
    let latest_value = sim.signal("latest_value");
    let empty = sim.signal("empty");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(latest_id), BigUint::from(5u8));
    assert_eq!(sim.get(latest_value), BigUint::from(5u8));
    assert_eq!(sim.get(empty), BigUint::from(1u8));
}

/// A method that returns an unpacked structure or a virtual interface and is
/// called without its empty parentheses gives a value of that type, to an
/// assignment and to an argument (IEEE 1800-2023 13.5.5).
#[test]
fn parenless_method_results_of_structure_and_interface_types() {
    let source = r#"
    interface Bus; int data; endinterface
    module Top(output int sum, output int passed, output int read);
        typedef struct { int a; int b; } pair_t;
        class Holder;
            pair_t p;
            virtual Bus bus;
            function pair_t get(); return p; endfunction
            function virtual Bus get_bus(); return bus; endfunction
        endclass
        function automatic int total(pair_t q); return q.a + q.b; endfunction
        Bus b();
        Holder h;
        pair_t g;
        virtual Bus v;
        initial begin
            h = new;
            h.p.a = 2; h.p.b = 5;
            h.bus = b;
            b.data = 7;
            g = h.get;
            sum = g.a + g.b;
            passed = total(h.get);
            v = h.get_bus;
            read = v.data;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let sum = sim.signal("sum");
    let passed = sim.signal("passed");
    let read = sim.signal("read");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sum), BigUint::from(7u8));
    assert_eq!(sim.get(passed), BigUint::from(7u8));
    assert_eq!(sim.get(read), BigUint::from(7u8));
}

/// A select or a member of a property is the target of a for-loop's
/// initialization and step, and a tagged expression assigned to a tagged union property takes
/// the property's type (IEEE 1800-2023 12.7.1, 11.9). Verilator 5.052
/// rejects a select as a for-loop target, and Icarus and xezim tagged
/// unions, so this is not a shared case.
#[test]
fn property_targets_of_loop_headers_and_tagged_values() {
    let source = r#"
    module Top(output int steps, output bit [8:0] full);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { bit [3:0] x; bit [3:0] y; } pair_t;
        class Info; bit [7:0] bits; item_t item; pair_t pair; endclass
        Info info;
        initial begin
            info = new;
            steps = 0;
            for (info.bits[3:0] = 4'h1; info.bits[3:0] < 4'h4; info.bits[3:0] += 1) steps++;
            for (info.pair.x = 4'h1; info.pair.x < 4'h3; info.pair.x++) steps++;
            info.item = tagged Full 8'h5a;
            full = 9'(info.item);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let steps = sim.signal("steps");
    let full = sim.signal("full");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(steps), BigUint::from(5u8));
    assert_eq!(sim.get(full), BigUint::from(0x15au16));
}

/// An assignment operator evaluates the address of its target once
/// (IEEE 1800-2023 11.4.1), before it reads the target, so it writes the
/// object it read even when its value changes what the target names. The
/// standard does not say when the address is evaluated, and Verilator
/// 5.052 evaluates it after the value, so this is not a shared case.
#[test]
fn assignment_operators_write_the_place_they_read() {
    let source = r#"
    module Top(output bit [7:0] bits, output bit [7:0] element, output bit [7:0] other_bits,
               output bit [7:0] other_element);
        class Info; bit [7:0] bits; bit [7:0] q[$]; endclass
        Info info, other, saved;
        function automatic bit [3:0] rebind(); info = other; return 4'h1; endfunction
        initial begin
            info = new;
            other = new;
            saved = info;
            info.bits = 8'h10;
            info.q.push_back(8'h10);
            other.q.push_back(8'h20);
            info.bits[3:0] += rebind();
            info = saved;
            info.q[0][3:0] += rebind();
            bits = saved.bits;
            element = saved.q[0];
            other_bits = other.bits;
            other_element = other.q[0];
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let signals = ["bits", "element", "other_bits", "other_element"].map(|name| sim.signal(name));
    sim.run_until(0).unwrap();
    let values = signals.map(|signal| sim.get(signal));
    assert_eq!(values, [0x11u8, 0x11, 0x00, 0x20].map(BigUint::from));
}

/// A const class property is written only by its initializer or, without
/// one, in its class's constructor (IEEE 1800-2023 8.19), whatever form
/// the write takes.
#[test]
fn const_property_writes_are_rejected() {
    let writes = [
        "c.k = 8'h1;",
        "c.k[3:0] = 4'h1;",
        "c.k += 8'h1;",
        "c.k++;",
        "{c.k, c.v} = 16'h1;",
        "c.s = \"x\";",
        "c.s.putc(0, \"y\");",
        "c.h = null;",
        "c.fixed = 8'h1;",
        "c.p.x = 4'hf;",
        "void'($sscanf(\"5\", \"%d\", c.k));",
    ];
    for write in writes {
        let source = format!(
            "module Top;
                typedef struct packed {{ bit [3:0] x; bit [3:0] y; }} pair_t;
                class C;
                    const pair_t p;
                    const bit [7:0] k;
                    const bit [7:0] fixed = 8'h3;
                    bit [7:0] v;
                    const string s;
                    const C h;
                    function new();
                        k = 8'h2;
                        s = \"a\";
                        h = null;
                    endfunction
                endclass
                initial begin
                    automatic C c = new;
                    {write}
                end
            endmodule"
        );
        let error =
            celox::Simulation::from_sv_sources(vec![(&*source, Path::new("class.sv"))], "Top")
                .build()
                .err()
                .unwrap_or_else(|| panic!("`{write}` was accepted"))
                .to_string();
        assert!(error.contains("const class property"), "`{write}`: {error}");
    }
}

/// A member of a tagged union nested in a packed structure property is
/// read and written with the check of its active member (IEEE 1800-2023
/// 7.3.2), as one of a variable is: an inactive member stops the run.
#[test]
fn inactive_tagged_members_of_properties_stop_the_run() {
    for access in [
        "r = info.record.item.Full;",
        "info.record.item.Full = 8'h42;",
        // The check of the value is the value's, whatever it is assigned to.
        "info.record.n = info.record.item.Full[3:0];",
        "info.record.n = value.item.Full[3:0];",
    ] {
        let source = format!(
            "module Top(output bit [7:0] r);
                typedef union tagged packed {{ void Empty; bit [7:0] Full; }} item_t;
                typedef struct packed {{ item_t item; bit [3:0] n; }} record_t;
                class Info; record_t record; endclass
                Info info;
                record_t value;
                initial begin info = new; value.item = tagged Empty; info.record = value; {access} end
            endmodule"
        );
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .unwrap();
        let error = sim.run_until(0).expect_err(access).to_string();
        assert!(error.contains("active tag"), "{access}: {error}");
    }
}

/// The check of a tagged member that an assignment writes reads the tag
/// when the member is written, after the value is evaluated (IEEE 1800-2023
/// 7.3.2, 10.4.1). Icarus and xezim reject tagged unions, so this is not a
/// shared case.
#[test]
fn tagged_member_targets_are_checked_when_written() {
    let source = r#"
    module Top(output bit [8:0] full);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; } record_t;
        class Info; record_t record; endclass
        Info info;
        function automatic bit [7:0] fill();
            info.record.item = tagged Full 8'h00;
            return 8'h5a;
        endfunction
        initial begin
            info = new;
            info.record.item = tagged Empty;
            info.record.item.Full = fill();
            full = 9'(info.record.item);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let full = sim.signal("full");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(full), BigUint::from(0x15au16));
}

/// An integral update of a whole tagged union would change its tag with
/// its bits (IEEE 1800-2023 7.3.2), so it is rejected whatever form the
/// target takes: a variable, a member of a variable, a class property, a
/// member of a property, or an element of an array of unions.
#[test]
fn integral_updates_of_tagged_union_targets_are_rejected() {
    for update in [
        "item += 1;",
        "record.item += 1;",
        "items[1] += 1;",
        "info.item += 1;",
        "info.record.item += 1;",
        "info.record.item++;",
        "unpacked[0] += 1;",
        "info.items[0]++;",
    ] {
        let source = format!(
            "module Top;
                typedef union tagged packed {{ void Empty; bit [7:0] Full; }} item_t;
                typedef struct packed {{ item_t item; bit [3:0] n; }} record_t;
                class Info; item_t item; record_t record; item_t items[$]; endclass
                item_t item;
                item_t [1:0] items;
                item_t unpacked[2];
                record_t record;
                initial begin
                    automatic Info info = new;
                    {update}
                end
            endmodule"
        );
        let error =
            celox::Simulation::from_sv_sources(vec![(&*source, Path::new("class.sv"))], "Top")
                .build()
                .err()
                .unwrap_or_else(|| panic!("`{update}` was accepted"))
                .to_string();
        assert!(error.contains("tagged union"), "`{update}`: {error}");
    }
}

/// A select of the bits of an element of an unpacked array of structures
/// that hold a tagged union is an ordinary vector, which an integral update
/// may write: the target's type is the type the selection of the whole
/// array gives (IEEE 1800-2023 7.4.6, 11.5.1).
#[test]
fn bit_selects_of_elements_holding_tagged_unions_are_vectors() {
    let source = r#"
    module Top(output bit [12:0] whole);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        record_t records[2];
        initial begin
            records[0].item = tagged Full 8'h41;
            records[0].n = 4'h2;
            records[0][3:0] += 1;
            whole = records[0];
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let whole = sim.signal("whole");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(whole), BigUint::from(0x1413u16));
}

/// An assignment operator reads its target, with the target's checks,
/// before it evaluates its value (IEEE 1800-2023 11.4.1): a value that
/// makes an inactive member active does not hide the invalid read.
#[test]
fn assignment_operators_check_their_target_before_the_value() {
    let source = r#"
    module Top;
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        class Info; record_t record; endclass
        Info info;
        function automatic bit [7:0] activate();
            info.record.item = tagged Full 8'h00;
            return 8'h01;
        endfunction
        initial begin
            info = new;
            info.record.item = tagged Empty;
            info.record.item.Full += activate();
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(0).unwrap_err().to_string();
    assert!(error.contains("active tag"), "{error}");
}

/// A two-state member of a four-state packed structure property takes no
/// unknown bits (IEEE 1800-2023 7.2.1), assigned as a member.
#[test]
fn two_state_members_of_packed_structure_properties_drop_unknown_bits() {
    let source = r#"
    module Top(output logic [1:0] whole);
        typedef struct packed { bit b; logic l; } mixed_t;
        class Info; mixed_t p; endclass
        Info info;
        initial begin info = new; info.p.b = 1'bx; info.p.l = 1'b1; whole = info.p; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .four_state(true)
        .build()
        .unwrap();
    let whole = sim.signal("whole");
    sim.run_until(0).unwrap();
    let (value, mask) = sim.get_four_state(whole);
    assert_eq!((value, mask), (BigUint::from(1u8), BigUint::from(0u8)));
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

/// The defaults of a method use the object it is called on, as the call
/// evaluated its receiver, also when an argument then assigns the variable
/// the receiver names (IEEE 1800-2023 13.5.3). Verilator 5.052 reads the
/// variable again, so this is not a shared case.
#[test]
fn method_defaults_use_the_receiver_the_call_evaluated() {
    let source = r#"
    module Top(output int r);
        class Pair;
            int n;
            function int f(int x, int y = n); return x + y; endfunction
        endclass
        Pair a, b, cur;
        function automatic int swap(); cur = b; return 0; endfunction
        initial begin
            a = new;
            a.n = 1;
            b = new;
            b.n = 2;
            cur = a;
            r = cur.f(swap());
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("receiver.sv"))], "Top")
            .build()
            .unwrap();
    let r = sim.signal("r");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(r), BigUint::from(1u8));
}

/// A constraint may not name a property that a constraint program does not
/// hold, and such a property may not be random; the class is otherwise
/// randomized with it as state.
#[test]
fn constraints_on_handle_and_string_properties_are_rejected() {
    let wide = "class C; rand bit [3:0] x; bit [127:0] w; constraint k { x < 5; } endclass";
    for (body, call, want) in [
        (
            "class C; rand bit [3:0] x; string s; constraint k { x < s.len(); } endclass",
            "c.randomize()",
            "constraint",
        ),
        (
            "class N; endclass class C; rand bit [3:0] x; N n; constraint k { x == n; } endclass",
            "c.randomize()",
            "a handle, string, container or property wider than 64 bits in a constraint",
        ),
        (
            "class C; rand bit [3:0] x; bit [127:0] w; constraint k { x < w; } endclass",
            "c.randomize()",
            "a handle, string, container or property wider than 64 bits in a constraint",
        ),
        (
            "class C; rand bit [3:0] x; rand string q[$]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "rand dynamic arrays and queues currently require 2-state integral elements",
        ),
        (
            "class C; rand bit [3:0] x; rand logic [3:0] d[]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "rand dynamic arrays and queues currently require 2-state integral elements",
        ),
        (
            "class C; rand bit [3:0] x; randc bit [3:0] d[]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "a randc dynamic array or queue",
        ),
        // A loop over a rand array would constrain its elements, which the
        // solver does not take yet.
        (
            "class C; rand int x; rand int a[]; constraint k { foreach (a[i]) x != a[i]; } endclass",
            "c.randomize()",
            "constraint foreach over a rand array",
        ),
        (
            "class C; rand int x; int a[]; constraint k { foreach (a[i]) x != a[i + 1]; } endclass",
            "c.randomize()",
            "constraint array element other than its loop's",
        ),
        // A member of a loop element is subject to its visibility (8.18).
        (
            "class R; local int m; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].m; } endclass",
            "c.randomize()",
            "local member `m` of class",
        ),
        (
            "class R; protected int m; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].m; } endclass",
            "c.randomize()",
            "protected member `m` of class",
        ),
        // An inline constraint accesses members as the caller does (18.7).
        (
            "class R; local int m; endclass \
             class C; rand int x; R rs[$]; endclass",
            "c.randomize() with { foreach (rs[i]) x != rs[i].m; }",
            "local member `m` of class",
        ),
        // A chandle has no value a constraint takes (6.14).
        (
            "class R; chandle h; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].h; } endclass",
            "c.randomize()",
            "a chandle in a constraint",
        ),
        // A process reference is no value either (9.7).
        (
            "class C; rand int x; process ps[$]; constraint k { foreach (ps[i]) x != ps[i]; } endclass",
            "c.randomize()",
            "a constraint array element other than 2-state integral",
        ),
        (
            "class C; rand int x; chandle hs[$]; constraint k { foreach (hs[i]) x != hs[i]; } endclass",
            "c.randomize()",
            "a constraint array element other than 2-state integral",
        ),
        // The array of a loop is subject to its visibility too (8.18).
        (
            "class C; rand int x; local int a[]; endclass",
            "c.randomize() with { foreach (a[i]) x != a[i]; }",
            "local member `a` of class",
        ),
        (
            "class B; local int a[]; endclass \
             class C extends B; rand int x; constraint k { foreach (a[i]) x != a[i]; } endclass",
            "c.randomize()",
            "local member `a` of class",
        ),
        // A real has no integral value a constraint takes (18.3).
        (
            "class C; rand int x; real t; constraint k { x < t; } endclass",
            "c.randomize()",
            "a real in a constraint",
        ),
        (
            "class R; real t; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x < rs[i].t; } endclass",
            "c.randomize()",
            "a real in a constraint",
        ),
        // An inline loop cannot be disabled: one over an array the call
        // selects would constrain its elements.
        (
            "class C; rand int x; int a[]; endclass",
            "c.randomize(a) with { foreach (a[i]) x != a[i]; }",
            "an inline constraint foreach over an array the call randomizes",
        ),
        // The size and indices of an associative array are not random (18.4).
        (
            "class C; rand bit [3:0] x; rand int a[int]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "constraints currently require scalar integral rand properties",
        ),
        (
            "class C; rand bit [3:0] x; rand bit [127:0] w; constraint k { x < 5; } endclass",
            "c.randomize()",
            "constraints currently require scalar integral rand properties",
        ),
        // Selecting a wide property makes it random, which the solver
        // cannot do, with constraints of the class or of the call.
        (
            wide,
            "c.randomize(w)",
            "randomize argument wider than 64 bits under constraints",
        ),
        (
            "class C; rand bit [3:0] x; bit [127:0] w; endclass",
            "c.randomize(w) with { x < 5; }",
            "randomize argument wider than 64 bits under constraints",
        ),
        // A base handle dispatches to the randomizer of a constrained
        // derived class (18.6.2).
        (
            "class C; rand bit [3:0] x; bit [127:0] w; endclass \
             class D extends C; constraint k { x < 5; } endclass",
            "c.randomize(w)",
            "randomize argument wider than 64 bits under constraints",
        ),
    ] {
        let source =
            format!("module Top; {body} C c; initial begin c = new; void'({call}); end endmodule");
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("class.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("{body} {call} is unsupported");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{body} {call}: {error}");
    }
}

/// The objects rand object handles reach form a graph that may be cyclic:
/// each object is solved, and its hooks run, once per call (IEEE 1800-2023
/// 18.4, 18.6.2). Verilator 5.052 rejects a rand member of the containing
/// class's type and does not finish compiling a cycle through two classes.
#[test]
fn rand_object_cycles_are_solved_once() {
    let source = r#"module Top(output int sum, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class SubLeaf extends Leaf;
        constraint d { v != 4; }
    endclass
    class Stuck extends Leaf;
        constraint e { v == 9; }
    endclass
    class Node;
        rand bit [3:0] w;
        rand Leaf leaf;
        rand Leaf none;
        Leaf plain;
        rand Node self_ref;
        constraint k { w < 3; }
    endclass
    class Holder;
        rand Leaf leaf;
    endclass
    Node n;
    SubLeaf s;
    Holder h;
    int bad;
    initial begin
        n = new;
        s = new;
        n.leaf = s;
        n.plain = new;
        n.self_ref = n;
        for (int i = 0; i < 20; i++) begin
            if (!n.randomize()) $fatal(1, "randomize");
            if (n.w >= 3 || !(s.v inside {3, 5})) bad++;
        end
        // A non-rand handle is not randomized; the hooks ran once per call.
        sum = s.pre + s.post + n.plain.v + n.plain.pre;
        // A class without constraints of its own solves its rand objects.
        h = new;
        h.leaf = new;
        if (!h.randomize() || !(h.leaf.v inside {[3:5]}) || h.leaf.pre != 1) bad++;
        // An unsatisfiable member fails the call, changes nothing and runs
        // no post_randomize.
        begin
            Stuck stuck = new;
            n.leaf = stuck;
            n.w = 2;
            if (n.randomize() || n.w != 2 || stuck.v != 0 || stuck.pre != 1 || stuck.post != 0) bad++;
        end
        $display("%0d %0d", sum, bad);
        done = bad == 0;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("cycle.sv"))], "Top")
        .build()
        .unwrap();
    let (sum, done) = (sim.signal("sum"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sum), BigUint::from(40u8));
    assert_eq!(sim.get(done), BigUint::from(1u8));
}

/// post_randomize runs on the rand object members the solve took, also when
/// the containing object's post_randomize clears or replaces the handle
/// first, and pre_randomize on those the object's own pre_randomize
/// allocates (IEEE 1800-2023 18.6.2). A randomize call inside a hook does
/// not change them, and the hooks it dispatches are those of the classes
/// its rand handles can refer to, not the hook calling it. randomize(null)
/// makes the rand handles state too (18.11, 18.11.1): it checks the
/// object's own constraints and runs the hooks of no member.
#[test]
fn post_randomize_runs_on_the_solved_members() {
    let source = r#"module Top(output int bad, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Other;
        rand Leaf leaf;
    endclass
    class Parent;
        rand Leaf leaf;
        int mode;
        Leaf spare;
        Other other;
        function void pre_randomize();
            if (mode == 3) leaf = new;
        endfunction
        function void post_randomize();
            if (mode == 1) leaf = null;
            if (mode == 2) leaf = spare;
            if (mode == 4) void'(other.randomize());
        endfunction
    endclass
    Parent p;
    Leaf first, second;
    initial begin
        p = new;
        p.spare = new;
        // Cleared by the parent's post_randomize.
        first = new;
        p.leaf = first;
        p.mode = 1;
        if (!p.randomize() || p.leaf != null || first.pre != 1 || first.post != 1
            || !(first.v inside {[3:5]})) bad++;
        // Replaced: the solved leaf's hook runs, the replacement's does not.
        second = new;
        p.leaf = second;
        p.mode = 2;
        if (!p.randomize() || p.leaf != p.spare || second.post != 1 || p.spare.pre != 0
            || p.spare.post != 0) bad++;
        // Allocated by the parent's pre_randomize: solved, both hooks run.
        p.mode = 3;
        p.leaf = null;
        if (!p.randomize() || p.leaf == null || p.leaf.pre != 1 || p.leaf.post != 1
            || !(p.leaf.v inside {[3:5]})) bad++;
        // A randomize call in the parent's post_randomize keeps the
        // parent's members.
        p.mode = 4;
        p.other = new;
        p.other.leaf = new;
        first = new;
        p.leaf = first;
        if (!p.randomize() || first.post != 1 || p.other.leaf.post != 1
            || !(p.other.leaf.v inside {[3:5]})) bad++;
        // randomize(null): the member's constraint and hooks are not part
        // of the check.
        p.mode = 0;
        first = new;
        first.v = 9;
        p.leaf = first;
        if (p.randomize(null) != 1 || first.v != 9 || first.pre != 0 || first.post != 0) bad++;
        $display("%0d", bad);
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hooks.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// Each object's rand object handles are read once its pre_randomize ran,
/// at every depth, so a member a nested hook allocates gets its
/// pre_randomize, is solved and gets its post_randomize; each object of a
/// cycle is visited once (IEEE 1800-2023 18.6.2).
#[test]
fn pre_randomize_discovers_members_at_every_depth() {
    let source = r#"module Top(output int bad, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Node;
        rand Leaf leaf;
        int pre, post;
        function void pre_randomize(); pre++; if (leaf == null) leaf = new; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Parent;
        rand Node node;
    endclass
    class Pair;
        rand bit [3:0] w;
        rand Pair other;
        int pre, post;
        constraint k { w == 6; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    Parent p;
    Pair a, b;
    initial begin
        p = new;
        p.node = new;
        if (!p.randomize() || p.node.leaf == null || p.node.pre != 1 || p.node.post != 1
            || p.node.leaf.pre != 1 || p.node.leaf.post != 1
            || !(p.node.leaf.v inside {[3:5]})) bad++;
        a = new;
        b = new;
        a.other = b;
        b.other = a;
        if (!a.randomize() || a.w != 6 || b.w != 6 || a.pre != 1 || a.post != 1
            || b.pre != 1 || b.post != 1) bad++;
        $display("%0d", bad);
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("depth.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A rand array whose rand_mode is off keeps its size and elements (IEEE
/// 1800-2023 18.8), an inline constraint may name its size (18.7), and
/// randomize(null) checks its size constraint against its size and changes
/// nothing (18.11.1); a disabled block names no size (18.9); a selected
/// array is random for the call (18.11); a bounded queue takes at most its bound plus one
/// elements (7.10). Verilator 5.052 resizes an array whose rand_mode is
/// off, ignores the inline size constraint, does not support
/// randomize(null) on a class with a rand container and does not finish
/// randomizing a bounded queue; xezim 0.11.4 grows a bounded queue past its
/// bound.
#[test]
fn rand_array_modes_inline_sizes_and_checks() {
    let source = r#"module Top(output int bad, output bit done);
    class Item;
        rand bit [63:0] value[];
        constraint max_values { value.size() > 0 && value.size() < 10; }
        bit [7:0] other[];
    endclass
    class Plain;
        int unsigned d[];
        bit [3:0] x;
    endclass
    class Bounded;
        rand bit few[$:2];
        constraint c { few.size() >= 0; }
    endclass
    Item item;
    Bounded b;
    initial begin
        item = new;
        b = new;
        for (int i = 0; i < 20; i++) if (b.randomize() == 0 || b.few.size() > 3) bad++;
        item.value = '{7};
        item.value.rand_mode(0);
        if (item.randomize() == 0 || item.value.size() != 1 || item.value[0] != 7) bad++;
        item.value.rand_mode(1);
        if ((item.randomize() with { value.size() == 2; }) == 0 || item.value.size() != 2) bad++;
        // A selected array is random for the call (18.11): at its size, or
        // the size the call's constraints give.
        begin
            Plain plain = new;
            int changed = 0;
            plain.d = new[4];
            for (int i = 0; i < 10; i++) begin
                if (plain.randomize(d) == 0 || plain.d.size() != 4 || plain.x != 0) bad++;
                foreach (plain.d[j]) if (plain.d[j] != 0) changed++;
            end
            if (changed == 0) bad++;
            if ((item.randomize(other) with { other.size() == 3; }) == 0 || item.other.size() != 3)
                bad++;
        end
        // Without its block, no constraint names the size: it is kept.
        item.max_values.constraint_mode(0);
        item.value = new[12];
        if (item.randomize() == 0 || item.value.size() != 12) bad++;
        item.max_values.constraint_mode(1);
        item.value.delete();
        if (item.randomize(null) != 0) bad++;
        item.value = new[3];
        if (item.randomize(null) != 1 || item.value.size() != 3 || item.value[0] != 0) bad++;
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("check.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A loop over a state array whose elements leave no solution fails the
/// call and changes nothing (IEEE 1800-2023 18.5.8.1, 18.6.1). Verilator
/// 5.052 prints the unsatisfied constraints.
#[test]
fn constraint_loops_without_a_solution() {
    let source = r#"module Top(output int bad, output bit done);
    class Region;
        bit [7:0] first, last;
    endclass
    class Policy;
        rand bit [7:0] start;
        Region in_use[$];
        constraint c { foreach (in_use[i]) !(start >= in_use[i].first && start <= in_use[i].last); }
    endclass
    Policy p;
    Region r;
    initial begin
        p = new;
        r = new;
        r.first = 0;
        r.last = 255;
        p.in_use.push_back(r);
        p.start = 9;
        if (p.randomize() != 0 || p.start != 9) bad++;
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("loops.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// An element of nested loops over one array is the element of the loop
/// whose variable subscripts it (IEEE 1800-2023 18.5.8.1), and a class
/// derived from the class of the elements sees its protected members (8.18);
/// an inline constraint sees the members its caller's class sees and the
/// caller's variables inside a loop (18.7). A member of an element that the
/// call solves through a rand handle is its solver variable (18.4).
#[test]
fn constraint_loops_by_their_variables() {
    let source = r#"module Top(output int bad, output bit done);
    class Region;
        protected bit [7:0] m;
        local bit [7:0] secret;
        rand bit [7:0] pick;
        Region kids[$];
        function new(bit [7:0] value); m = value; secret = value; endfunction
        function int choose();
            return randomize() with { foreach (kids[i]) pick != kids[i].secret; pick < 2; };
        endfunction
    endclass
    class Pairs extends Region;
        rand bit [7:0] v, w;
        bit [7:0] vals[];
        Region rs[$];
        constraint c {
            foreach (vals[i])
                foreach (vals[j])
                    (i == 0 && j == 1) -> v == vals[i] + vals[j];
            foreach (rs[i]) w != rs[i].m;
            w < 3;
        }
        function new(); super.new(0); endfunction
    endclass
    class Child;
        rand bit [3:0] m;
    endclass
    class Holder;
        rand Child child;
        Child items[$];
        constraint c { foreach (items[i]) items[i].m == 5; }
    endclass
    Pairs p;
    Region r;
    Holder h;
    initial begin
        p = new;
        p.vals = '{8'd5, 8'd7};
        r = new(0);
        p.rs.push_back(r);
        r = new(2);
        p.rs.push_back(r);
        for (int i = 0; i < 5; i++)
            if (p.randomize() == 0 || p.v != 12 || p.w != 1) bad++;
        // A loop in a class's own inline constraint reads its local member.
        r.kids.push_back(p.rs[0]);
        for (int i = 0; i < 5; i++)
            if (r.choose() == 0 || r.pick != 1) bad++;
        // An element that is also the rand handle's object: its rand m is
        // solved; another element's m is state.
        h = new;
        h.child = new;
        h.items.push_back(h.child);
        begin
            Child other = new;
            other.m = 5;
            h.items.push_back(other);
            for (int i = 0; i < 5; i++)
                if (h.randomize() == 0 || h.child.m != 5 || other.m != 5) bad++;
            other.m = 4;
            if (h.randomize() != 0 || other.m != 4) bad++;
        end
        // The caller's variable in an inline loop body.
        begin
            bit [7:0] limit = 3;
            for (int i = 0; i < 5; i++)
                if ((p.randomize() with { foreach (vals[i]) w != vals[i] - limit; }) == 0
                    || p.w != 1) bad++;
        end
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("pairs.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A call whose random set includes the array of an enabled constraint loop
/// (IEEE 1800-2023 18.11), as `randomize(a)` directly or through a base
/// handle to a derived class with the loop, would constrain its elements,
/// which the solver does not take: the call faults. With the block disabled
/// (18.9), the loop takes no part and the call randomizes the array.
#[test]
fn constraint_loops_over_selected_arrays_fault() {
    let base = r#"module Top(output int done);
    class B;
        rand bit [3:0] x;
        bit [3:0] a[];
    endclass
    class C extends B;
        constraint k { foreach (a[i]) x != a[i]; }
    endclass
    B b;
    C c;
    initial begin
        c = new;
        c.a = new[2];
        void'(c.randomize());
        CALL;
        done = 1;
    end
endmodule
"#;
    for call in [
        "void'(c.randomize(a))",
        "begin b = c; void'(b.randomize(a)); end",
    ] {
        let source = base.replace("CALL", call);
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("select.sv"))],
            "Top",
        )
        .build()
        .unwrap();
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(
            error.contains("a constraint loop over an array the call randomizes"),
            "{call}: {error}"
        );
    }
    let source = base.replace(
        "CALL",
        "begin c.k.constraint_mode(0); void'(c.randomize(a)); end",
    );
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("select.sv"))], "Top")
            .build()
            .unwrap();
    let done = sim.signal("done");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
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

/// The SV frontend is recursive, and one case nests class specializations
/// 64 deep, so the cases build on a thread with room on its stack (like
/// suite cases in `suite::run_case`).
#[test]
fn misused_parameterized_classes_are_rejected() {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, misused_parameterized_classes)
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

fn misused_parameterized_classes() {
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
            // A named four-state constant of the module is not yet an
            // actual (#1964); a class passes its own such parameters on.
            "localparam logic [1:0] X = 2'bxx; class Four #(logic [1:0] N = 0); endclass
             Four#(X) four;",
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
    fn constrained_randomize_state_properties(sim) { @case "class::constrained_randomize_state_properties"; }
    fn constrained_randomize_rand_objects(sim) { @case "class::constrained_randomize_rand_objects"; }
    fn output_arguments_write_class_properties(sim) { @case "class::output_arguments_write_class_properties"; }
    fn virtual_interface_property_waits(sim) { @case "class::virtual_interface_property_waits"; }
    fn constrained_randomize_rand_arrays(sim) { @case "class::constrained_randomize_rand_arrays"; }
    fn constrained_randomize_shifts(sim) { @case "class::constrained_randomize_shifts"; }
    fn constrained_randomize_foreach(sim) { @case "class::constrained_randomize_foreach"; }
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

sv_backends! {
    fn class_scoped_typedef_handles(sim) { @case "class::scoped_typedef_handles"; }
    fn class_specialized_scope_typedef_calls(sim) { @case "class::specialized_scope_typedef_calls"; }
    fn class_specialized_scope_typedef_properties(sim) { @case "class::specialized_scope_typedef_properties"; }
    fn class_parameter_strings_wider_than_a_constant(sim) { @case "class::parameter_strings_wider_than_a_constant"; }
    fn class_new_initializers_of_scoped_and_alias_types(sim) { @case "class::new_initializers_of_scoped_and_alias_types"; }
    fn class_parameter_literals_hide_module_constants(sim) { @case "class::parameter_literals_hide_module_constants"; }
    fn class_static_initializers_run_once_before_time_zero(sim) { @case "class::static_initializers_run_once_before_time_zero"; }
    fn class_method_defaults_use_the_object(sim) { @case "class::method_defaults_use_the_object"; }
    fn class_nested_value_selects(sim) { @case "class::nested_value_selects"; }
    fn class_property_assignment_sites(sim) { @case "class::property_assignment_sites"; }
    fn class_property_place_addresses(sim) { @case "class::property_place_addresses"; }
    fn class_property_member_place_addresses(sim) { @case "class::property_member_place_addresses"; }
    fn class_formatting_task_place_outputs(sim) { @case "class::formatting_task_place_outputs"; }
    fn class_property_member_assignment_sites(sim) { @case "class::property_member_assignment_sites"; }
    fn class_packed_structure_property_members(sim) { @case "class::packed_structure_property_members"; }
    fn class_tagged_members_of_structure_properties(sim) { @case "class::tagged_members_of_structure_properties"; }
    fn class_methods_called_without_parentheses(sim) { @case "class::methods_called_without_parentheses"; }
    fn class_constrained_enumerations_and_randc(sim) { @case "class::constrained_enumerations_and_randc"; }
    fn class_prototype_formals_inherit_types(sim) { @case "class::prototype_formals_inherit_types"; }
    fn class_real_values_round_into_integral_properties(sim) { @case "class::real_values_round_into_integral_properties"; }
    fn class_real_properties(sim) { @case "class::real_properties"; }
}

sv_backends! {
    // A random enumeration of 1024 members takes one of them (IEEE 1800-2023
    // 18.3): its domain stays within the solver's limits.
    fn class_randomizes_large_enumerations(sim) {
        @setup {
            let members = (0..1024)
                .map(|index| format!("M{index} = 12'd{}", index * 3))
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(
                "module Top(output int ok, output int value);
                    typedef enum bit [11:0] {{ {members} }} big_e;
                    class Big; rand big_e e; constraint c {{ e > 12'd30; }} endclass
                    Big b;
                    initial begin
                        b = new;
                        ok = b.randomize() with {{ 1; }};
                        value = b.e;
                    end
                endmodule"
            );
        }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("big_enum.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u32.into());
        let value: u32 = sim.get(sim.signal("value")).try_into().unwrap();
        assert!(value > 30 && value.is_multiple_of(3), "{value}");
    }

    // A class parameter hides the module's constant of its name also in a
    // static call that passes it on by name (8.25).
    fn class_parameter_literals_hide_module_constants_in_static_calls(sim) {
        @setup {
            let source = r#"module Top(output int ok);
                localparam string S = "module";
                class Name #(string S = "x");
                    static function string get(); return S; endfunction
                endclass
                class Shadow #(string S = "x");
                    function string s(); return Name#(S)::get(); endfunction
                endclass
                Shadow #("a_name_longer_than_sixteen") shadow = new;
                initial ok = shadow.s() == "a_name_longer_than_sixteen";
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u32.into());
    }

    // A method's return type may name a method of a class declared after it,
    // which declaration-time queries see (8.25).
    fn class_signatures_naming_later_classes(sim) {
        @setup {
            let source = r#"package p;
                class A;
                    static function bit [$bits(B::g())-1:0] f(); return 0; endfunction
                endclass
                class B;
                    static function bit [11:0] g(); return 0; endfunction
                endclass
                localparam int W = $bits(A::f());
            endpackage
            module Top(output int width);
                import p::*;
                assign width = W;
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("signatures.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("width")), 12u32.into());
    }
}

/// A signed member of a property keeps its signedness as an inout actual:
/// the copy-in sign-extends it into a wider formal (IEEE 1800-2023 7.2.1,
/// 13.5), and a copy-out through a container element is published to a
/// forked waiter on the element as an assignment of it is (9.4.2).
#[test]
fn place_actuals_keep_signedness_and_publish_copy_outs() {
    let source = r#"`timescale 1ns/1ns
typedef struct packed { bit signed [3:0] s; bit [3:0] u; } sp_t;
class Holder; sp_t sp; endclass
module Top(output int seen, output int widened, output bit done);
  Holder h;
  int mq[$] = '{0, 0};
  function void widen(inout bit signed [7:0] io); widened = io; io = io - 1; endfunction
  task automatic bump(output int o); o = 7; endtask
  initial begin
    h = new;
    h.sp.s = -1;
    widen(h.sp.s);
    if (h.sp.s != -2) widened = 0;
    fork
      begin @(mq[1]); seen = mq[1] == 7 ? 1 : 2; end
      begin #1 bump(mq[1]); end
    join
    done = 1;
  end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("signed.sv"))], "Top")
        .build()
        .unwrap();
    let (seen, widened, done) = (
        sim.signal("seen"),
        sim.signal("widened"),
        sim.signal("done"),
    );
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(widened), BigUint::from(u32::MAX));
    assert_eq!(sim.get(seen), BigUint::from(1u8));
}

#[test]
fn conditional_handles_of_sibling_classes_take_their_common_base() {
    // IEEE 1800-2023 11.4.11 c to e: a derived arm takes the other arm's
    // class, and arms of sibling classes their closest common base class.
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        class C extends B; virtual function int id(); return 3; endfunction endclass
        class D extends B; virtual function int id(); return 4; endfunction endclass
        bit s;
        int sibling, derived, method;
        initial begin
            C c; D d; B b; A a;
            c = new; d = new;
            s = 1;
            b = s ? c : d;
            sibling = b.id();
            a = s ? d : b;
            derived = a.id();
            method = (s ? d : c).id();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("sibling")), 3u32.into());
    assert_eq!(sim.get(sim.signal("derived")), 4u32.into());
    assert_eq!(sim.get(sim.signal("method")), 4u32.into());
    // The result has the common base class, which a derived class's
    // variable does not take without `$cast` (8.16).
    let source = "module Top;
        class A; endclass
        class B extends A; endclass
        class C extends B; endclass
        class D extends B; endclass
        bit s;
        initial begin C c; D d; c = new; d = new; c = s ? c : d; end
    endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build_cranelift()
    else {
        panic!("a handle of the common base class was assigned to a derived class's variable");
    };
    assert!(
        error
            .to_string()
            .contains("a handle of class `B` is not assignment compatible"),
        "{error}"
    );
}
