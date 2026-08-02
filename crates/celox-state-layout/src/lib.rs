//! Shared physical simulation-state layout contracts.
//!
//! These types and offsets form the ABI between layout construction, generated
//! code, and runtime state access. They contain no frontend or backend IR.

use celox_design::{
    AbsoluteAddrBase, RegionedAbsoluteAddrBase, RuntimeEventSite, SPARSE_WORKING_REGION,
    STABLE_REGION,
};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::hash::Hash;

pub const RUNTIME_EVENT_CAPACITY: usize = 1024;
pub const RUNTIME_EVENT_WRITING: u64 = u64::MAX;
pub const STATE_HEADER_SIZE: usize = 32;
pub const STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET: usize = 0;
/// Remaining iterations for an in-function native tick loop.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET: usize = 8;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET: usize = 24;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET: usize = 16;
/// Runtime-event write sequence observed when a native tick batch starts.
pub const RUNTIME_EVENT_HEADER_SIZE: usize = 8;
pub const RUNTIME_EVENT_SLOT_SEQ_OFFSET: usize = 0;
pub const RUNTIME_EVENT_SLOT_SITE_OFFSET: usize = 8;
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

/// One semantic state object that requires stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateObjectLayout<A> {
    pub address: A,
    pub width: usize,
    pub is_4state: bool,
}

/// Semantic constraints produced by optimization and consumed when physical
/// state layout is finalized.
///
/// This deliberately contains no physical offsets.  Alias pairs state that
/// two semantic objects may share one stable-state home; layout construction
/// still validates representation compatibility before applying the pair.
#[derive(Debug, Clone)]
pub struct LayoutRequirements<A> {
    state_aliases: HashMap<A, A>,
}

impl<A> Default for LayoutRequirements<A> {
    fn default() -> Self {
        Self {
            state_aliases: HashMap::default(),
        }
    }
}

impl<A> LayoutRequirements<A> {
    pub fn state_aliases(&self) -> &HashMap<A, A> {
        &self.state_aliases
    }

    pub fn state_aliases_mut(&mut self) -> &mut HashMap<A, A> {
        &mut self.state_aliases
    }

    pub fn is_empty(&self) -> bool {
        self.state_aliases.is_empty()
    }

    pub fn clear(&mut self) {
        self.state_aliases.clear();
    }
}

/// Complete, backend-independent input to physical layout construction.
///
/// The compiler facade adapts its phase artifacts into this value. Layout
/// construction therefore never needs to inspect frontend IR, optimizer plans,
/// or a mixed compiler `Program`.
#[derive(Debug, Clone)]
pub struct LayoutInput<A> {
    pub state_objects: Vec<StateObjectLayout<A>>,
    pub working_addresses: Vec<A>,
    pub sparse_addresses: Vec<A>,
    pub unpacked_arrays: HashMap<A, UnpackedArrayLayout>,
    pub requirements: LayoutRequirements<A>,
    pub ff_referenced_addresses: HashSet<A>,
    pub num_events: usize,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
}

/// Adapter implemented by the phase artifact that precedes physical layout.
pub trait LayoutSource<A> {
    fn layout_input(&self, mode: MemoryLayoutMode) -> LayoutInput<A>;
}

#[derive(Debug, Clone)]
pub struct MemoryLayout<A> {
    pub four_state: bool,
    pub mode: MemoryLayoutMode,
    /// Stable region offsets. Includes all declared state objects.
    pub offsets: HashMap<A, usize>,
    pub widths: HashMap<A, usize>,
    /// Whether each state object has a four-state source type.
    pub is_4states: HashMap<A, bool>,
    pub unpacked_arrays: HashMap<A, UnpackedArrayLayout>,
    /// Stable region size in bytes.
    pub total_size: usize,

    /// Working region offsets. Includes only actually-used state objects.
    pub working_offsets: HashMap<A, usize>,
    pub working_base_offset: usize,
    /// Copy-on-write next-state data for dynamically addressed FF targets.
    pub sparse_offsets: HashMap<A, usize>,
    pub sparse_base_offset: usize,
    pub sparse_layouts: HashMap<A, SparseWorkingLayout>,
    pub sparse_active_bits_offset: usize,
    pub sparse_active_capacity: usize,
    pub merged_total_size: usize,

    pub triggered_bits_offset: usize,
    pub triggered_bits_total_size: usize,

    pub scratch_base_offset: usize,
    pub scratch_size: usize,

    pub runtime_event_capacity: usize,
    pub runtime_event_slot_size: usize,
    pub runtime_event_buffer_size: usize,
    pub runtime_event_site_layouts: Vec<RuntimeEventSiteLayout>,
}

type PhysicalLayoutObject<A> = (A, usize, bool, usize, usize);

fn sort_layout_objects<A: Copy + Ord>(objects: &mut [PhysicalLayoutObject<A>]) {
    // Packing by decreasing alignment avoids padding. Equal-alignment objects
    // use semantic-address order so randomized input maps cannot perturb every
    // physical offset and the generated machine code that embeds it.
    objects.sort_unstable_by_key(|(address, _, _, _, alignment)| {
        (std::cmp::Reverse(*alignment), *address)
    });
}

impl<A> MemoryLayout<A>
where
    A: Copy + Eq + Hash + Ord,
{
    pub fn build<S>(source: &S, four_state: bool, mode: MemoryLayoutMode) -> Self
    where
        S: LayoutSource<A>,
    {
        let input = source.layout_input(mode);
        let LayoutInput {
            state_objects,
            working_addresses,
            sparse_addresses,
            unpacked_arrays,
            requirements,
            ff_referenced_addresses,
            num_events,
            runtime_event_sites,
        } = input;

        let mut stable_objects = state_objects
            .into_iter()
            .map(|object| {
                let size = unpacked_arrays
                    .get(&object.address)
                    .map(|layout| layout.plane_size)
                    .unwrap_or_else(|| get_byte_size(object.width));
                let alignment = unpacked_arrays
                    .get(&object.address)
                    .map(|layout| layout.element_stride.min(8))
                    .unwrap_or_else(|| get_alignment(object.width));
                (
                    object.address,
                    object.width,
                    object.is_4state,
                    size,
                    alignment,
                )
            })
            .collect::<Vec<_>>();
        sort_layout_objects(&mut stable_objects);

        let mut offsets = HashMap::default();
        let mut widths = HashMap::default();
        let mut is_4states = HashMap::default();
        let runtime_event_site_layouts = build_runtime_event_site_layouts(&runtime_event_sites);
        let runtime_event_slot_size = RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
            + runtime_event_site_layouts
                .iter()
                .map(|site| site.payload_words)
                .max()
                .unwrap_or(0)
                * 8;

        let mut current_offset = STATE_HEADER_SIZE;
        for (address, width, is_4state, size, alignment) in stable_objects {
            current_offset = align_up(current_offset, alignment);
            offsets.insert(address, current_offset);
            widths.insert(address, width);
            is_4states.insert(address, is_4state);
            current_offset += size;
            if four_state {
                current_offset += size;
            }
        }

        let mut working_objects = working_addresses
            .iter()
            .map(|address| {
                let width = widths[address];
                let size = unpacked_arrays
                    .get(address)
                    .map(|layout| layout.plane_size)
                    .unwrap_or_else(|| get_byte_size(width));
                let alignment = unpacked_arrays
                    .get(address)
                    .map(|layout| layout.element_stride.min(8))
                    .unwrap_or_else(|| get_alignment(width));
                (*address, width, is_4states[address], size, alignment)
            })
            .collect::<Vec<_>>();
        sort_layout_objects(&mut working_objects);

        let mut working_offsets = HashMap::default();
        let mut working_size = 0;
        for (address, _, _, size, alignment) in working_objects {
            working_size = align_up(working_size, alignment);
            working_offsets.insert(address, working_size);
            working_size += size;
            if four_state {
                working_size += size;
            }
        }

        let mut sparse_objects = sparse_addresses
            .iter()
            .map(|address| {
                let width = widths[address];
                let size = unpacked_arrays
                    .get(address)
                    .map(|layout| layout.plane_size)
                    .unwrap_or_else(|| get_byte_size(width));
                let alignment = unpacked_arrays
                    .get(address)
                    .map(|layout| layout.element_stride.min(8))
                    .unwrap_or_else(|| get_alignment(width));
                (*address, width, is_4states[address], size, alignment)
            })
            .collect::<Vec<_>>();
        sort_layout_objects(&mut sparse_objects);

        let mut sparse_offsets = HashMap::default();
        let mut sparse_size = 0usize;
        for (address, _, _, size, alignment) in sparse_objects {
            sparse_size = align_up(sparse_size, alignment);
            sparse_offsets.insert(address, sparse_size);
            let plane_count = if four_state { 2 } else { 1 };
            let final_chunk_size = align_up(size, 8);
            let physical_extent = (plane_count - 1) * size + final_chunk_size;
            sparse_size += align_up(physical_extent, 8);
        }

        let working_base_offset = align_up(current_offset, 8);
        let sparse_base_offset = align_up(working_base_offset + working_size, 8);
        let mut sparse_metadata_offset = align_up(sparse_base_offset + sparse_size, 8);
        let mut sparse_layouts = HashMap::default();
        let mut sparse_order = sparse_addresses;
        sparse_order.sort_unstable();
        let sparse_active_capacity = sparse_order.len();
        for (active_index, address) in sparse_order.into_iter().enumerate() {
            let chunk_count = unpacked_arrays
                .get(&address)
                .map(|layout| layout.plane_size.div_ceil(8))
                .unwrap_or_else(|| widths[&address].div_ceil(64));
            let dirty_word_count = chunk_count.div_ceil(64);
            let summary_word_count = dirty_word_count.div_ceil(64);
            let dirty_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += dirty_word_count * 8;
            let summary_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += summary_word_count * 8;
            sparse_layouts.insert(
                address,
                SparseWorkingLayout {
                    active_index,
                    chunk_count,
                    dirty_words_offset,
                    dirty_word_count,
                    summary_words_offset,
                    summary_word_count,
                },
            );
        }

        let sparse_active_bits_offset = align_up(sparse_metadata_offset, 8);
        sparse_metadata_offset =
            sparse_active_bits_offset + sparse_active_capacity.div_ceil(64) * 8;
        let triggered_bits_offset = align_up(sparse_metadata_offset, 8);
        let triggered_bits_total_size = num_events.div_ceil(8);
        let scratch_base_offset = align_up(triggered_bits_offset + triggered_bits_total_size, 8);
        let runtime_event_buffer_size =
            RUNTIME_EVENT_HEADER_SIZE + RUNTIME_EVENT_CAPACITY * runtime_event_slot_size;
        let merged_total_size = scratch_base_offset;

        let mut address_aliases = requirements.state_aliases.into_iter().collect::<Vec<_>>();
        address_aliases.sort_unstable();
        for (alias, canonical) in address_aliases {
            let fourstate_ok = !four_state
                || (is_4states.get(&alias) == Some(&false)
                    && is_4states.get(&canonical) == Some(&false));
            let alias_fits = widths
                .get(&alias)
                .zip(widths.get(&canonical))
                .is_some_and(|(&alias_width, &canonical_width)| alias_width <= canonical_width);
            if fourstate_ok
                && alias_fits
                && !ff_referenced_addresses.contains(&alias)
                && let Some(&canonical_offset) = offsets.get(&canonical)
            {
                offsets.insert(alias, canonical_offset);
            }
        }

        Self {
            four_state,
            mode,
            offsets,
            widths,
            is_4states,
            unpacked_arrays,
            total_size: current_offset,
            working_offsets,
            working_base_offset,
            sparse_offsets,
            sparse_base_offset,
            sparse_layouts,
            sparse_active_bits_offset,
            sparse_active_capacity,
            merged_total_size,
            triggered_bits_offset,
            triggered_bits_total_size,
            scratch_base_offset,
            scratch_size: 0,
            runtime_event_capacity: RUNTIME_EVENT_CAPACITY,
            runtime_event_slot_size,
            runtime_event_buffer_size,
            runtime_event_site_layouts,
        }
    }

    /// Append backend-private scratch storage without changing any semantic
    /// state offset. Backend planning happens after the backend-neutral state
    /// layout has been finalized, so scratch is always the final region.
    pub fn with_backend_scratch(mut self, scratch_size: usize) -> Self {
        self.scratch_size = scratch_size;
        self.merged_total_size = align_up(self.scratch_base_offset + scratch_size, 8);
        self
    }

    pub fn plane_size(&self, address: &A) -> usize {
        self.unpacked_arrays
            .get(address)
            .map(|layout| layout.plane_size)
            .unwrap_or_else(|| get_byte_size(self.widths[address]))
    }

    pub fn region_base_offset<R>(&self, address: &R) -> usize
    where
        R: RegionedAddress<A>,
    {
        let absolute = address.absolute_address();
        match address.region() {
            STABLE_REGION => self.offsets[&absolute],
            SPARSE_WORKING_REGION => self.sparse_base_offset + self.sparse_offsets[&absolute],
            _ => self.working_base_offset + self.working_offsets[&absolute],
        }
    }

    pub fn map_static_bit_offset(&self, address: &A, bit_offset: usize) -> (usize, usize) {
        let Some(array) = self.unpacked_arrays.get(address) else {
            return (bit_offset / 8, bit_offset % 8);
        };
        let element = bit_offset / array.element_width;
        let intra_element = bit_offset % array.element_width;
        (
            element * array.element_stride + intra_element / 8,
            intra_element % 8,
        )
    }

    pub fn regioned_static_byte_and_intra<R>(
        &self,
        address: &R,
        bit_offset: usize,
    ) -> Option<(i32, usize)>
    where
        R: RegionedAddress<A>,
    {
        let absolute = address.absolute_address();
        let base = match address.region() {
            STABLE_REGION => *self.offsets.get(&absolute).unwrap_or(&0),
            SPARSE_WORKING_REGION => {
                self.sparse_base_offset + *self.sparse_offsets.get(&absolute).unwrap_or(&0)
            }
            _ => self.working_base_offset + *self.working_offsets.get(&absolute).unwrap_or(&0),
        };
        let (byte, intra) = self.map_static_bit_offset(&absolute, bit_offset);
        Some((i32::try_from(base.checked_add(byte)?).ok()?, intra))
    }
}

pub trait RegionedAddress<A> {
    fn region(&self) -> u32;
    fn absolute_address(&self) -> A;
}

impl<V: Copy> RegionedAddress<AbsoluteAddrBase<V>> for RegionedAbsoluteAddrBase<V> {
    fn region(&self) -> u32 {
        self.region
    }

    fn absolute_address(&self) -> AbsoluteAddrBase<V> {
        self.absolute_addr()
    }
}

fn build_runtime_event_site_layouts(sites: &[RuntimeEventSite]) -> Vec<RuntimeEventSiteLayout> {
    sites
        .iter()
        .map(|site| {
            let mut payload_words = 0;
            let args = site
                .arg_widths
                .iter()
                .map(|width| {
                    let word_count = (*width).div_ceil(64).max(1);
                    let value_word_offset = payload_words;
                    payload_words += word_count;
                    let mask_word_offset = payload_words;
                    payload_words += word_count;
                    RuntimeEventArgLayout {
                        value_word_offset,
                        mask_word_offset,
                        word_count,
                    }
                })
                .collect();
            RuntimeEventSiteLayout {
                args,
                payload_words,
            }
        })
        .collect()
}

const fn align_up(offset: usize, alignment: usize) -> usize {
    (offset + alignment - 1) & !(alignment - 1)
}

fn get_alignment(width: usize) -> usize {
    let size = get_byte_size(width);
    if size == 0 {
        1
    } else if size <= 8 {
        size.next_power_of_two()
    } else {
        8
    }
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
    fn layout_order_is_independent_of_input_iteration_order() {
        let high_address = (3u32, 64, false, 8, 8);
        let low_address = (1u32, 64, false, 8, 8);
        let less_aligned = (0u32, 32, false, 4, 4);
        let mut forward = vec![high_address, less_aligned, low_address];
        let mut reverse = forward.iter().copied().rev().collect::<Vec<_>>();

        sort_layout_objects(&mut forward);
        sort_layout_objects(&mut reverse);

        assert_eq!(forward, reverse);
        assert_eq!(forward, vec![low_address, high_address, less_aligned]);
    }

    #[test]
    fn layout_requirements_own_semantic_aliases_until_layout() {
        let mut requirements = LayoutRequirements::default();
        requirements.state_aliases_mut().insert(2u32, 1u32);

        assert_eq!(requirements.state_aliases().get(&2), Some(&1));
        assert!(!requirements.is_empty());

        requirements.clear();
        assert!(requirements.is_empty());
    }

    #[test]
    fn layout_applies_aliases_from_requirements() {
        struct AliasLayoutSource;

        impl LayoutSource<u32> for AliasLayoutSource {
            fn layout_input(&self, _mode: MemoryLayoutMode) -> LayoutInput<u32> {
                let mut requirements = LayoutRequirements::default();
                requirements.state_aliases_mut().insert(2, 1);
                LayoutInput {
                    state_objects: vec![
                        StateObjectLayout {
                            address: 1,
                            width: 8,
                            is_4state: false,
                        },
                        StateObjectLayout {
                            address: 2,
                            width: 8,
                            is_4state: false,
                        },
                    ],
                    working_addresses: Vec::new(),
                    sparse_addresses: Vec::new(),
                    unpacked_arrays: HashMap::default(),
                    requirements,
                    ff_referenced_addresses: HashSet::default(),
                    num_events: 0,
                    runtime_event_sites: Vec::new(),
                }
            }
        }

        let layout = MemoryLayout::build(&AliasLayoutSource, false, MemoryLayoutMode::Packed);
        assert_eq!(layout.offsets[&1], layout.offsets[&2]);
    }

    #[test]
    fn backend_scratch_only_extends_the_final_layout_region() {
        struct EmptyLayoutSource;

        impl LayoutSource<u32> for EmptyLayoutSource {
            fn layout_input(&self, _mode: MemoryLayoutMode) -> LayoutInput<u32> {
                LayoutInput {
                    state_objects: Vec::new(),
                    working_addresses: Vec::new(),
                    sparse_addresses: Vec::new(),
                    unpacked_arrays: HashMap::default(),
                    requirements: LayoutRequirements::default(),
                    ff_referenced_addresses: HashSet::default(),
                    num_events: 3,
                    runtime_event_sites: Vec::new(),
                }
            }
        }

        let base = MemoryLayout::build(&EmptyLayoutSource, false, MemoryLayoutMode::Packed);
        let expanded = base.clone().with_backend_scratch(13);

        assert_eq!(base.scratch_size, 0);
        assert_eq!(expanded.scratch_base_offset, base.scratch_base_offset);
        assert_eq!(expanded.scratch_size, 13);
        assert_eq!(expanded.merged_total_size, base.scratch_base_offset + 16);
        assert_eq!(expanded.offsets, base.offsets);
        assert_eq!(expanded.working_offsets, base.working_offsets);
        assert_eq!(expanded.triggered_bits_offset, base.triggered_bits_offset);
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn state_header_fields_do_not_overlap() {
        const {
            assert!(
                STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET + 8
                    <= STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET
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
}
