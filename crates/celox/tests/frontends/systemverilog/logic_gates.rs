sv_backends! {
    fn scalar_concatenated_outputs(sim) { @case "logic_gates::scalar_concatenated_outputs"; }
    fn scalar_two_state_member(sim) { @case "logic_gates::scalar_two_state_member"; }
    fn scalar_expressions_in_generate_loops(sim) { @case "logic_gates::scalar_expressions_in_generate_loops"; }
    fn scalar_two_state_truth_tables(sim) { @case "logic_gates::scalar_two_state_truth_tables"; }
    fn scalar_truth_tables(sim) { @case "logic_gates::scalar_truth_tables"; }
    fn scalar_three_inputs(sim) { @case "logic_gates::scalar_three_inputs"; }
}

#[test]
fn interpreter_truth_tables() {
    for case in [
        "scalar_concatenated_outputs",
        "scalar_truth_tables",
        "scalar_two_state_member",
        "scalar_two_state_truth_tables",
        "scalar_three_inputs",
        "scalar_expressions_in_generate_loops",
    ] {
        crate::suite::run_case(&format!("logic_gates::{case}"), "interpreter");
    }
}
