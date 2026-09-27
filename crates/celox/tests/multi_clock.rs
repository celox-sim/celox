#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Two independent clock domains: each FF only advances on its own clock.
    fn test_independent_clock_domains(sim) {
        @ignore_on(sv);
        @case "multi_clock::test_independent_clock_domains";
    }

    // A counter in one clock domain feeding into another (CDC pattern).
    // Tests that domains are truly independent.
    fn test_clock_domain_crossing_pattern(sim) {
        @ignore_on(sv);
        @case "multi_clock::test_clock_domain_crossing_pattern";
    }

    // FF with separate clocks and separate resets.
    fn test_separate_resets_per_domain(sim) {
        @case "multi_clock::test_separate_resets_per_domain";
    }
}
