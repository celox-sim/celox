#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Edge detector: output is combinational (assign), internal `data` register
    // updates on clock edge. So the output reflects the difference between
    // current i_data and the registered (previous-cycle) value.
    //
    // Flow: set i_data -> tick (FF captures i_data into `data`) -> change i_data
    //       -> eval_comb -> read outputs (edge between old data and new i_data)
    fn test_edge_detector_basic(sim) {
        @case "std_edge_detector::test_edge_detector_basic";
    }

    // Clear suppresses posedge/negedge outputs.
    //
    // Note: the stdlib edge_detector has operator precedence such that:
    //   o_edge    = i_data ^ (data & ~i_clear)   -- XOR, not fully masked by clear
    //   o_posedge = i_data & ~data & ~i_clear     -- AND, fully masked by clear
    //   o_negedge = ~i_data & data & ~i_clear     -- AND, fully masked by clear
    fn test_edge_detector_clear(sim) {
        @case "std_edge_detector::test_edge_detector_clear";
    }

    // Multi-bit edge detection (WIDTH=4) using o_edge output. The assignment
    // context propagates WIDTH into the bitwise expression, so the one-bit
    // i_clear operand is widened before `~` is applied.
    fn test_edge_detector_multibit(sim) {
        @case "std_edge_detector::test_edge_detector_multibit";
    }
}
