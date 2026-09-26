// Tests that verify unimplemented paths panic instead of producing silent wrong results.

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Wide dynamic shift (a >> idx with 128-bit a) is handled by the runtime
// shift select chain, NOT by lower_wide_extract. Verify it works correctly.
fn wide_dynamic_shift_works(sim) {
    @case "test_unimplemented_paths::wide_dynamic_shift_works";
}

// Dynamic offset Store with 4-state mask: array write with variable index
// on a logic (4-state) type must correctly store the mask.
fn dynamic_mask_store(sim) {
    @ignore_on(sv);
    @case "test_unimplemented_paths::dynamic_mask_store";
}

}
