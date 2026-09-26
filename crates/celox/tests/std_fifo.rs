#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// After reset, FIFO should be empty
fn test_fifo_initial_empty(sim) {
    @ignore_on(sv);
    @case "std_fifo::test_fifo_initial_empty";
}

// Push one item, verify not empty, pop it back
fn test_fifo_push_pop_single(sim) {
    @ignore_on(sv);
    @case "std_fifo::test_fifo_push_pop_single";
}

// Push until full (DEPTH=4), verify full flag
fn test_fifo_full(sim) {
    @ignore_on(sv);
    @case "std_fifo::test_fifo_full";
}

// Push 4 items then pop all, verify FIFO ordering
fn test_fifo_ordering(sim) {
    @ignore_on(sv);
    @case "std_fifo::test_fifo_ordering";
}

// Clear resets the FIFO to empty
fn test_fifo_clear(sim) {
    @ignore_on(sv);
    @case "std_fifo::test_fifo_clear";
}

}
