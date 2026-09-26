#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Test 1: shift inside if_reset else branch

// Test 2: shift inside for loop (non-self-referencing)

// Test 3: if_reset + for + shift (non-self-referencing)

// Test 4: right shift in if_reset

// Test 5: dynamic shift amount in if_reset

// Test 6: shift with for loop index (writing to different array elements)

// Test 7: shift amount wider than value (for loop const is 32-bit)

all_backends! {

    fn test_shift_in_if_reset(sim) {
        @case "shift_bug_test::test_shift_in_if_reset";
    }

    fn test_shift_in_for_loop(sim) {
        @case "shift_bug_test::test_shift_in_for_loop";
    }

    fn test_shift_ifreset_for(sim) {
        @case "shift_bug_test::test_shift_ifreset_for";
    }

    fn test_right_shift_in_if_reset(sim) {
        @case "shift_bug_test::test_right_shift_in_if_reset";
    }

    fn test_dynamic_shift_in_if_reset(sim) {
        @case "shift_bug_test::test_dynamic_shift_in_if_reset";
    }

    fn test_shift_to_array_by_loop_index(sim) {
        @case "shift_bug_test::test_shift_to_array_by_loop_index";
    }

    fn test_shift_with_wide_const_amount(sim) {
        @case "shift_bug_test::test_shift_with_wide_const_amount";
    }
}
