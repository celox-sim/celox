#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// LFSR Galois has no reset port -- uses i_set for initialization.
// The Top module wraps it with a clock but no reset connection.

all_backends! {

    // Basic LFSR: seed then shift, verify output changes
    fn test_lfsr_basic_shift(sim) {
        @case "std_lfsr::test_lfsr_basic_shift";
    }

    // Cycle detection: LFSR should produce a deterministic repeating cycle
    fn test_lfsr_deterministic_cycle(sim) {
        @case "std_lfsr::test_lfsr_deterministic_cycle";
    }

    // LFSR disabled (i_en=0) should hold its value
    fn test_lfsr_enable_hold(sim) {
        @case "std_lfsr::test_lfsr_enable_hold";
    }
}
