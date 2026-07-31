//! Shared physical simulation-state layout contracts.
//!
//! These types and offsets form the ABI between layout construction, generated
//! code, and runtime state access. They contain no frontend or backend IR.

pub const RUNTIME_EVENT_CAPACITY: usize = 1024;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const RUNTIME_EVENT_WRITING: u64 = u64::MAX;
pub const STATE_HEADER_SIZE: usize = 32;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET: usize = 0;
/// Remaining iterations for an in-function native tick loop.
#[cfg(target_arch = "x86_64")]
pub const STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET: usize = 8;
#[cfg(target_arch = "x86_64")]
pub const STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET: usize = 24;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET: usize = 16;
/// Runtime-event write sequence observed when a native tick batch starts.
pub const RUNTIME_EVENT_HEADER_SIZE: usize = 8;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const RUNTIME_EVENT_SLOT_SEQ_OFFSET: usize = 0;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const RUNTIME_EVENT_SLOT_SITE_OFFSET: usize = 8;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET: usize = 16;
pub const RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET: usize = 24;

#[derive(Debug, Clone)]
pub struct RuntimeEventArgLayout {
    pub value_word_offset: usize,
    pub mask_word_offset: usize,
    pub word_count: usize,
}

#[derive(Debug, Clone)]
pub struct RuntimeEventSiteLayout {
    pub args: Vec<RuntimeEventArgLayout>,
    pub payload_words: usize,
}

#[derive(Debug, Clone)]
pub struct SparseWorkingLayout {
    pub active_index: usize,
    pub chunk_count: usize,
    pub dirty_words_offset: usize,
    pub dirty_word_count: usize,
    pub summary_words_offset: usize,
    pub summary_word_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLayoutMode {
    Packed,
    ElementStrided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackedArrayLayout {
    pub element_width: usize,
    pub element_count: usize,
    pub element_stride: usize,
    pub plane_size: usize,
}

pub const fn get_byte_size(width: usize) -> usize {
    width.div_ceil(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_rounds_up_partial_bytes() {
        assert_eq!(get_byte_size(0), 0);
        assert_eq!(get_byte_size(1), 1);
        assert_eq!(get_byte_size(8), 1);
        assert_eq!(get_byte_size(9), 2);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn state_header_fields_do_not_overlap() {
        assert!(
            STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET + 8 <= STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET
        );
        assert!(
            STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET + 8
                <= STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
        );
        assert!(
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET + 8
                <= STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET
        );
        assert!(STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET + 8 <= STATE_HEADER_SIZE);
    }
}
