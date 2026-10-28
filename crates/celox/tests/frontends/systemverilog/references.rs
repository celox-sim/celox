sv_backends! {
    fn ref_writes_are_seen_before_the_return(sim) {
        @case "references::ref_writes_are_seen_before_the_return";
    }
    fn ref_formals_of_one_actual_alias_each_other(sim) {
        @case "references::ref_formals_of_one_actual_alias_each_other";
    }
    fn ref_task_keeps_the_reference_while_suspended(sim) {
        @case "references::ref_task_keeps_the_reference_while_suspended";
    }
    fn ref_fills_caller_queues(sim) {
        @case "references::ref_fills_caller_queues";
    }
    fn ref_replaces_a_class_handle(sim) {
        @case "references::ref_replaces_a_class_handle";
    }
    fn const_ref_reads_the_current_value(sim) {
        @case "references::const_ref_reads_the_current_value";
    }
    fn const_ref_in_a_continuous_assignment(sim) {
        @case "references::const_ref_in_a_continuous_assignment";
    }
    fn ref_default_and_named_arguments(sim) {
        @case "references::ref_default_and_named_arguments";
    }
    fn ref_actual_of_an_equivalent_type(sim) {
        @case "references::ref_actual_of_an_equivalent_type";
    }
    fn ref_class_property_names_the_object_of_the_call(sim) {
        @case "references::ref_class_property_names_the_object_of_the_call";
    }
    fn ref_property_of_this(sim) {
        @case "references::ref_property_of_this";
    }
    fn ref_task_keeps_a_property_while_suspended(sim) {
        @case "references::ref_task_keeps_a_property_while_suspended";
    }
    fn ref_array_element_index_is_fixed_at_the_call(sim) {
        @case "references::ref_array_element_index_is_fixed_at_the_call";
    }
    fn ref_elements_of_concurrent_task_activations(sim) {
        @case "references::ref_elements_of_concurrent_task_activations";
    }
    fn ref_static_class_properties(sim) {
        @case "references::ref_static_class_properties";
    }
    fn ref_property_forwarded_to_output_and_inout(sim) {
        @case "references::ref_property_forwarded_to_output_and_inout";
    }
    fn ref_element_select_past_the_element(sim) {
        @case "references::ref_element_select_past_the_element";
    }
    fn ref_element_after_an_input_in_forked_activations(sim) {
        @case "references::ref_element_after_an_input_in_forked_activations";
    }
}
