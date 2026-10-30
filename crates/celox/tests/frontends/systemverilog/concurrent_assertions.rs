sv_backends! {
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
