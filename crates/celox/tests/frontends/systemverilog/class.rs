//! Class objects, properties, and methods (IEEE 1800-2023 clause 8).

use super::*;

sv_backends! {
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
}

sv_backends! {
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

#[test]
fn four_state_properties_are_rejected() {
    let source = r#"
        module Top;
            class C;
                logic [3:0] x;
            endclass
            C c;
            initial c = new;
        endmodule
    "#;
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build_cranelift()
    else {
        panic!("a 4-state class property is unsupported");
    };
    let error = error.to_string();
    assert!(error.contains("4-state class property"), "{error}");
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
    fn class_randomize_rejects_dynamic_base_handle(sim) {
        @setup {
            let source = r#"module Top(output int value, hooks, result);
                class Base;
                    rand int x = 77;
                    static int calls;
                    function void pre_randomize(); calls++; endfunction
                    function void post_randomize(); calls++; endfunction
                endclass
                class Derived extends Base;
                    constraint c { x == 1; }
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

sv_backends! {
    fn class_randomization_enum_inheritance(sim) { @case "class::randomization_enum_inheritance"; }
    fn class_randomization_enum_alias(sim) { @case "class::randomization_enum_alias"; }
    fn class_randomization_enum_domain(sim) { @case "class::randomization_enum_domain"; }
}

sv_backends! {
    fn class_randomization_enum_fill_literals(sim) { @case "class::randomization_enum_fill_literals"; }
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
