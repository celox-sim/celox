#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Exhaustive 8-bit onehot detection
    fn test_onehot_8bit_exhaustive(sim) {
        @case "std_onehot::test_onehot_8bit_exhaustive";
    }
}
