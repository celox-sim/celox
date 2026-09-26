#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // DELAY=0: passthrough (no delay)
    fn test_delay_zero(sim) {
        @case "std_delay::test_delay_zero";
    }

    // DELAY=1: one cycle delay
    fn test_delay_one(sim) {
        @case "std_delay::test_delay_one";
    }

    // DELAY=3: three cycle pipeline
    fn test_delay_three(sim) {
        @case "std_delay::test_delay_three";
    }
}
