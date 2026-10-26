use super::*;

sv_backends! {
    fn optional_parentheses_packed_result(sim) { @case "containers::optional_parentheses_packed_result"; }
    fn optional_parentheses(sim) { @case "containers::optional_parentheses"; }
    fn optional_parentheses_queries(sim) { @case "containers::optional_parentheses_queries"; }
    fn optional_parentheses_four_state(sim) { @case "containers::optional_parentheses_four_state"; }
    fn queue_argument_side_effects(sim) { @case "containers::queue_argument_side_effects"; }
    fn queue_argument_four_state(sim) { @case "containers::queue_argument_four_state"; }
    fn string_associative_foreach(sim) { @case "containers::string_associative_foreach"; }
    fn object_elements_keep_handle_semantics(sim) { @case "containers::object_elements_keep_handle_semantics"; }
    fn runtime_queries_and_traversal_conversion(sim) { @case "containers::runtime_queries_and_traversal_conversion"; }
    fn equal_updates_preserve_event_waits(sim) { @case "containers::equal_updates_preserve_event_waits"; }
    fn bounded_queue_and_equality(sim) { @case "containers::bounded_queue_and_equality"; }
    fn four_state_container_equality(sim) { @case "containers::four_state_container_equality"; }
    fn string_elements_and_gc(sim) { @case "containers::string_elements_and_gc"; }
    fn foreach_and_control_flow(sim) { @case "containers::foreach_and_control_flow"; }
    fn dynamic_resize_and_copy(sim) { @case "containers::dynamic_resize_and_copy"; }
    fn queue_methods(sim) { @case "containers::queue_methods"; }
    fn associative_integer_keys(sim) { @case "containers::associative_integer_keys"; }
    fn associative_string_keys(sim) { @case "containers::associative_string_keys"; }
    fn four_state_elements_and_indices(sim) { @case "containers::four_state_elements_and_indices"; }
    fn declaration_initializers_and_locals(sim) { @case "containers::declaration_initializers_and_locals"; }
}

sv_backends! {
    fn container_checkpoints_restore_all_three_kinds(sim) {
        @setup {
            let source = r#"module Top(output int value);
                timeunit 1ps / 1ps;
                int a[], q[$], aa[int];
                initial begin
                    a = new[1]; a[0] = 1; q.push_back(2); aa[0] = 3;
                    #10 value = a[0] + q[0] + aa[0];
                    #10 a[0] = 4; q.push_back(5); aa[0] = 6;
                    #10 value = a[0] + q[1] + aa[0];
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("container_checkpoint.sv"))], "Top");
        sim.run_until(5).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let bytes = checkpoint.to_bytes().unwrap();
        // SAFETY: bytes came directly from this simulator's checkpoint.
        let persisted = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        let value = sim.signal("value");
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(15u8));
        sim.restore(&persisted).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get(value), BigUint::from(6u8));
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(15u8));
    }
}

sv_backends! {
    fn negative_dynamic_array_size_is_a_runtime_error(sim) {
        @setup { let source = "module Top; int a[]; initial a = new[-1]; endmodule"; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("negative_array.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("negative dynamic array size"), "{error}");
    }
}
