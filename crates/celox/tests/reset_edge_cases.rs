#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Test `reset_async_high`: the FF resets when the reset signal is HIGH.
    fn test_reset_async_high(sim) {
        @case "reset_edge_cases::test_reset_async_high";
    }

    // Test `reset_sync_high`: synchronous reset, active HIGH.
    fn test_reset_sync_high(sim) {
        @case "reset_edge_cases::test_reset_sync_high";
    }

    // Test `reset_sync_low`: synchronous reset, active LOW.
    fn test_reset_sync_low(sim) {
        @case "reset_edge_cases::test_reset_sync_low";
    }

    // Test multiple FF blocks sharing the same reset signal.
    fn test_shared_reset(sim) {
        @case "reset_edge_cases::test_shared_reset";
    }

    // Reset value that is non-zero: verifies the reset assignment uses
    // the specified value, not just zero.
    fn test_nonzero_reset_value(sim) {
        @case "reset_edge_cases::test_nonzero_reset_value";
    }
}
