#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Independent reductions without feedback retain their shared loop and
// induction variable. Check both reductions across changing external inputs.
fn recovered_independent_reductions_share_input_correctly(sim) {
    @ignore_on(sv);
    @case "recovered_unrolled_fold::recovered_independent_reductions_share_input_correctly";
}

// Read enables determine the write grant, but never read the granted writes.
// Recovering both reductions as one atomic fold must not invent that feedback.
fn recovered_mmio_read_write_dependencies_are_independent(sim) {
    @ignore_on(sv);
    @case "recovered_unrolled_fold::recovered_mmio_read_write_dependencies_are_independent";
}

fn recovered_unrolled_store_forward_selects_older_entry(sim) {
    @ignore_on(sv);
    @case "recovered_unrolled_fold::recovered_unrolled_store_forward_selects_older_entry";
}

fn recovered_unrolled_multi_state_priority(sim) {
    @ignore_on(sv);
    @case "recovered_unrolled_fold::recovered_unrolled_multi_state_priority";
}

fn recovered_unrolled_guard_uses_procedural_four_state_truth(sim) {
    @ignore_on(sv);
    @case "recovered_unrolled_fold::recovered_unrolled_guard_uses_procedural_four_state_truth";
}

}
