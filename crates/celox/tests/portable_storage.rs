//! Run shared storage cases at the adapter's default optimization level.
//! The original O0 and optimized regressions remain in their original files.
#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn ff_captures_every_one_bit_array_element(sim) {
        @case "ff_narrow_arrays::ff_captures_every_one_bit_array_element";
    }

    fn ff_captures_padded_elements_and_unknown_masks(sim) {
        @case "ff_narrow_arrays::ff_captures_padded_elements_and_unknown_masks";
    }

    fn packed_scatter_last_lane_does_not_touch_adjacent_storage(sim) {
        @case "packed_scatter_store::packed_scatter_last_lane_does_not_touch_adjacent_storage";
    }
}
