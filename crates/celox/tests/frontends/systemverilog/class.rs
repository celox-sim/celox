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
fn misplaced_super_new_and_static_misuse_are_rejected() {
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
