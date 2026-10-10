#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn two_state_runtime_matrix(sim) { @case "partial_word_shift::two_state_runtime_matrix"; }

    fn sar_padding_does_not_escape(sim) { @case "partial_word_shift::sar_padding_does_not_escape"; }

    fn sign_z_65(sim) { @case "partial_word_shift::sign_z_65"; }
    fn sign_x_65(sim) { @case "partial_word_shift::sign_x_65"; }
    fn two_state_matrix(sim) { @case "partial_word_shift::two_state_matrix"; }
    fn four_state_matrix(sim) { @case "partial_word_shift::four_state_matrix"; }
}
