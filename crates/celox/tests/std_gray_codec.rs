#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Gray encode → decode roundtrip: o_bin == i_bin for all 8-bit values
    fn test_gray_roundtrip_8bit(sim) {
        @case "std_gray_codec::test_gray_roundtrip_8bit";
    }

    // Verify Gray code property: adjacent binary values differ by exactly 1 bit in Gray
    fn test_gray_single_bit_change(sim) {
        @case "std_gray_codec::test_gray_single_bit_change";
    }
}
