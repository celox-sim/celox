sv_backends! {
    fn input_container_changes_are_not_seen_by_the_caller(sim) {
        @case "container_formals::input_container_changes_are_not_seen_by_the_caller";
    }
    fn input_container_from_container_values(sim) {
        @case "container_formals::input_container_from_container_values";
    }
    fn input_container_from_an_empty_concatenation(sim) {
        @case "container_formals::input_container_from_an_empty_concatenation";
    }
    fn output_container_starts_empty_and_is_copied_back(sim) {
        @case "container_formals::output_container_starts_empty_and_is_copied_back";
    }
    fn inout_container_is_copied_in_and_back(sim) {
        @case "container_formals::inout_container_is_copied_in_and_back";
    }
    fn task_copies_containers_back_when_it_returns(sim) {
        @case "container_formals::task_copies_containers_back_when_it_returns";
    }
    fn container_formals_of_class_handles_copy_the_container(sim) {
        @case "container_formals::container_formals_of_class_handles_copy_the_container";
    }
    fn container_formals_of_class_methods(sim) {
        @case "container_formals::container_formals_of_class_methods";
    }
    fn static_subroutine_container_formals(sim) {
        @case "container_formals::static_subroutine_container_formals";
    }
    fn container_formals_forwarded_and_read_from_properties(sim) {
        @case "container_formals::container_formals_forwarded_and_read_from_properties";
    }
}
