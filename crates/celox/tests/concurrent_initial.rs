#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
    fn concurrent_initial_shared_clock(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::shared_clock";
    }
    fn concurrent_initial_control_flow(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::control_flow";
    }
    fn concurrent_initial_clock_periods(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::clock_periods";
    }
    fn concurrent_initial_reset_only_clock(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::reset_only_clock";
    }
    fn concurrent_initial_simultaneous_edges(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::simultaneous_edges";
    }
    fn concurrent_initial_reset(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::reset";
    }
    fn concurrent_initial_reset_between_edges(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::reset_between_edges";
    }
    fn concurrent_initial_hierarchy(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::hierarchy";
    }
    fn concurrent_initial_mixed_edges(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "concurrent_initial::mixed_edges";
    }
}
