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
