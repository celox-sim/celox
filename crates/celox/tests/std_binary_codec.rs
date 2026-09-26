#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Binary encoder: onehot input -> binary output (UNARY_WIDTH=8, BIN_WIDTH=3)
    fn test_binary_encoder(sim) {
        @case "std_binary_codec::test_binary_encoder";
    }

    // Binary decoder: binary input -> onehot output (BIN_WIDTH=3, UNARY_WIDTH=8)
    fn test_binary_decoder(sim) {
        @case "std_binary_codec::test_binary_decoder";
    }

    // Roundtrip: encoder -> decoder (onehot -> binary -> onehot)
    fn test_binary_codec_roundtrip(sim) {
        @case "std_binary_codec::test_binary_codec_roundtrip";
    }

    // Encoder with enable=0: output should be 0 (masked)
    fn test_binary_encoder_disabled(sim) {
        @case "std_binary_codec::test_binary_encoder_disabled";
    }
}
