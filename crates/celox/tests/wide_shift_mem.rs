#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Tests for memory-backed wide shift operations (>= 256-bit / 4 chunks).
//
// The translator uses a memory-backed path (stack slots with O(1) dynamic
// indexing) for shift/sar when the operand width is >= MEM_SHIFT_THRESHOLD
// (4 chunks = 256 bits). These tests ensure correctness of that path.

// ============================================================
// 256-bit shifts (exactly at threshold: 4 chunks)
// ============================================================

// ============================================================
// 512-bit shifts (8 chunks, well above threshold)
// ============================================================

// ============================================================
// 512-bit shifts in always_ff (through clock edge)
// ============================================================

// ============================================================
// 1024-bit shifts (16 chunks)
// ============================================================

// ============================================================
// Edge cases
// ============================================================

// ============================================================
// Narrow source, wide destination (OOB regression)
// ============================================================

all_backends! {

    fn test_256bit_shift_left_by_zero(sim) {
        @case "wide_shift_mem::test_256bit_shift_left_by_zero";
    }

    fn test_256bit_shift_left_within_chunk(sim) {
        @case "wide_shift_mem::test_256bit_shift_left_within_chunk";
    }

    fn test_256bit_shift_left_exact_chunk_boundary(sim) {
        @case "wide_shift_mem::test_256bit_shift_left_exact_chunk_boundary";
    }

    fn test_256bit_shift_left_cross_chunk(sim) {
        @case "wide_shift_mem::test_256bit_shift_left_cross_chunk";
    }

    fn test_256bit_shift_left_overflow(sim) {
        @case "wide_shift_mem::test_256bit_shift_left_overflow";
    }

    fn test_256bit_shift_right_logical(sim) {
        @case "wide_shift_mem::test_256bit_shift_right_logical";
    }

    fn test_256bit_arithmetic_shift_right(sim) {
        @case "wide_shift_mem::test_256bit_arithmetic_shift_right";
    }

    fn test_512bit_shift_left(sim) {
        @case "wide_shift_mem::test_512bit_shift_left";
    }

    fn test_512bit_shift_right(sim) {
        @case "wide_shift_mem::test_512bit_shift_right";
    }

    fn test_512bit_arithmetic_shift_right(sim) {
        @case "wide_shift_mem::test_512bit_arithmetic_shift_right";
    }

    fn test_512bit_shift_left_multiword_pattern(sim) {
        @case "wide_shift_mem::test_512bit_shift_left_multiword_pattern";
    }

    fn test_512bit_shift_left_ff(sim) {
        @case "wide_shift_mem::test_512bit_shift_left_ff";
    }

    fn test_512bit_sar_ff(sim) {
        @case "wide_shift_mem::test_512bit_sar_ff";
    }

    fn test_1024bit_shift_left(sim) {
        @case "wide_shift_mem::test_1024bit_shift_left";
    }

    fn test_1024bit_shift_right(sim) {
        @case "wide_shift_mem::test_1024bit_shift_right";
    }

    fn test_1024bit_sar_sign_extension(sim) {
        @case "wide_shift_mem::test_1024bit_sar_sign_extension";
    }

    fn test_256bit_all_ones_shift_left_one(sim) {
        @case "wide_shift_mem::test_256bit_all_ones_shift_left_one";
    }

    fn test_512bit_shift_right_complete_overflow(sim) {
        @case "wide_shift_mem::test_512bit_shift_right_complete_overflow";
    }

    fn test_256bit_shift_right_cross_chunk(sim) {
        @case "wide_shift_mem::test_256bit_shift_right_cross_chunk";
    }

    // Regression test: when lhs is narrower than dst, the memory-backed source
    // slot must be zero-padded to num_chunks (= common_logical_width / 64).
    // Without padding, load_or_default reads uninitialised memory beyond the
    // source slot, producing garbage in the upper chunks.
    fn test_narrow_source_wide_dest_shift_left(sim) {
        @ignore_on(sv);
        @case "wide_shift_mem::test_narrow_source_wide_dest_shift_left";
    }

    fn test_narrow_source_wide_dest_shift_right(sim) {
        @ignore_on(sv);
        @case "wide_shift_mem::test_narrow_source_wide_dest_shift_right";
    }

    fn test_narrow_source_wide_dest_sar(sim) {
        @case "wide_shift_mem::test_narrow_source_wide_dest_sar";
    }
}
