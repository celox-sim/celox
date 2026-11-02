sv_backends! {
fn overlapped_boolean_implication(sim) { @case "concurrent_assertions::overlapped_boolean_implication"; }
fn overlapped_unknown_antecedent(sim) { @case "concurrent_assertions::overlapped_unknown_antecedent"; }
fn sampled_history_while_disabled(sim) { @case "concurrent_assertions::sampled_history_while_disabled"; }
fn sampled_history_predicates(sim) { @case "concurrent_assertions::sampled_history_predicates"; }
fn sampled_history_gates_and_indices(sim) { @case "concurrent_assertions::sampled_history_gates_and_indices"; }
fn sampled_history_unknown_transitions(sim) { @case "concurrent_assertions::sampled_history_unknown_transitions"; }
fn sampled_history_explicit_nesting(sim) { @case "concurrent_assertions::sampled_history_explicit_nesting"; }
fn sampled_predicate_live_action(_sim) { @case "concurrent_assertions::sampled_predicate_live_action"; }
fn disable_iff_after_nba(_sim) { @case "concurrent_assertions::disable_iff_after_nba"; }
fn predicates_before_all_actions(_sim) { @case "concurrent_assertions::predicates_before_all_actions"; }
fn assume_cover_and_generate(_sim) { @case "concurrent_assertions::assume_cover_and_generate"; }
}
sv_backends! {
fn unknown_predicates_and_disable_conditions(_sim) { @case "concurrent_assertions::unknown_predicates_and_disable_conditions"; }
}

#[test]
fn native_images_preserve_assertion_regions() {
    crate::suite::run_case(
        "concurrent_assertions::sampled_predicate_live_action",
        "native-image",
    );
}

#[test]
fn native_images_preserve_sampled_history_monitors() {
    for case in [
        "concurrent_assertions::sampled_history_gates_and_indices",
        "concurrent_assertions::sampled_history_unknown_transitions",
    ] {
        crate::suite::run_case(case, "native-image");
    }
}
