#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn signed_divrem_i8(sim) {
        @ignore_on(veryl, sv);
        @case "signed_divrem::signed_divrem_i8";
    }

    fn signed_divrem_i64(sim) {
        @case "signed_divrem::signed_divrem_i64";
    }

    fn signed_divrem_i128(sim) {
        @case "signed_divrem::signed_divrem_i128";
    }

    fn signed_divrem_always_ff(sim) {
        @case "signed_divrem::signed_divrem_always_ff";
    }

    fn signed_divrem_four_state_unknown(sim) {
        @case "signed_divrem::signed_divrem_four_state_unknown";
    }

    fn signed_divrem_four_state_zero_divisor(sim) {
        @case "signed_divrem::signed_divrem_four_state_zero_divisor";
    }
}
