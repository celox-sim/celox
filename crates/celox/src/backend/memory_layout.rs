use crate::HashMap;
use crate::ir::{AbsoluteAddr, Program, SIRInstruction, SIROffset};

pub const RUNTIME_EVENT_CAPACITY: usize = 1024;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const RUNTIME_EVENT_WRITING: u64 = u64::MAX;
pub const STATE_HEADER_SIZE: usize = 32;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET: usize = 0;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub const STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET: usize = 16;
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

#[derive(Debug, Clone)]
pub struct MemoryLayout {
    pub four_state: bool,
    pub mode: MemoryLayoutMode,
    /// Stable region (region = 0) offsets. Includes all declared variables.
    pub offsets: HashMap<AbsoluteAddr, usize>,
    pub widths: HashMap<AbsoluteAddr, usize>,
    /// Whether the variable is a 4-state type.
    pub is_4states: HashMap<AbsoluteAddr, bool>,
    pub unpacked_arrays: HashMap<AbsoluteAddr, UnpackedArrayLayout>,
    /// Stable region size in bytes.
    pub total_size: usize,

    /// Working region (region != 0) offsets. Includes only actually-used variables.
    pub working_offsets: HashMap<AbsoluteAddr, usize>,
    /// Base offset (bytes) of the working region inside the unified memory buffer.
    pub working_base_offset: usize,
    /// Copy-on-write next-state data for dynamically addressed FF targets.
    pub sparse_offsets: HashMap<AbsoluteAddr, usize>,
    pub sparse_base_offset: usize,
    pub sparse_layouts: HashMap<AbsoluteAddr, SparseWorkingLayout>,
    /// Fixed-capacity worklist of sparse regions touched by the current FF
    /// evaluation.  A region appears at most once, guarded by its active byte.
    pub sparse_active_count_offset: usize,
    pub sparse_active_flags_offset: usize,
    pub sparse_active_list_offset: usize,
    pub sparse_active_capacity: usize,
    /// Unified memory buffer size in bytes (stable + working).
    pub merged_total_size: usize,

    /// Bitset of triggered domain IDs.
    pub triggered_bits_offset: usize,
    pub triggered_bits_total_size: usize,

    /// Scratch region for spilling inter-chunk registers.
    /// Located after triggered bits. Zero if no spilling needed.
    pub scratch_base_offset: usize,
    pub scratch_size: usize,

    /// Runtime event ring geometry. The ring storage itself is backend-owned;
    /// generated code finds it through the state header.
    pub runtime_event_capacity: usize,
    pub runtime_event_slot_size: usize,
    pub runtime_event_buffer_size: usize,
    pub runtime_event_site_layouts: Vec<RuntimeEventSiteLayout>,
}

impl MemoryLayout {
    pub fn build(program: &Program, four_state: bool, mode: MemoryLayoutMode) -> Self {
        let scratch_bytes = match &program.eval_comb_plan {
            Some(crate::ir::EvalCombPlan::MemorySpilled(plan)) => plan.scratch_bytes,
            _ => 0,
        };
        let unpacked_arrays = if mode == MemoryLayoutMode::ElementStrided {
            collect_strided_array_layouts(program)
        } else {
            HashMap::default()
        };
        let mut stable_vars_to_layout = Vec::new();

        for (instance_id, module_id) in &program.instance_module {
            let variables = &program.module_variables[module_id];
            for info in variables.values() {
                let addr = AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                };
                let size = unpacked_arrays
                    .get(&addr)
                    .map(|layout| layout.plane_size)
                    .unwrap_or_else(|| get_byte_size(info.width));
                let align = unpacked_arrays
                    .get(&addr)
                    .map(|layout| layout.element_stride.min(8))
                    .unwrap_or_else(|| get_alignment(info.width));
                stable_vars_to_layout.push((addr, info.width, info.is_4state, size, align));
            }
        }

        stable_vars_to_layout.sort_by_key(|&(_, _, _, _, align)| std::cmp::Reverse(align));

        let mut offsets = HashMap::default();
        let mut widths = HashMap::default();
        let mut is_4states = HashMap::default();
        let runtime_event_site_layouts = build_runtime_event_site_layouts(program);
        let runtime_event_slot_size = RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
            + runtime_event_site_layouts
                .iter()
                .map(|site| site.payload_words)
                .max()
                .unwrap_or(0)
                * 8;

        let mut current_offset = STATE_HEADER_SIZE;

        // 3. Execute packing
        for (addr, width, is_4state, size, align) in stable_vars_to_layout {
            current_offset = (current_offset + align - 1) & !(align - 1);

            offsets.insert(addr, current_offset);
            widths.insert(addr, width);
            is_4states.insert(addr, is_4state);

            current_offset += size;
            if four_state {
                current_offset += size;
            }
        }

        // Compact working region: only variables actually written in WORKING region.
        let working_addrs = program.collect_working_region_addrs();
        let mut working_vars_to_layout: Vec<(AbsoluteAddr, usize, bool, usize, usize)> =
            working_addrs
                .iter()
                .map(|addr| {
                    let width = widths[addr];
                    let is_4state = is_4states[addr];
                    let size = unpacked_arrays
                        .get(addr)
                        .map(|layout| layout.plane_size)
                        .unwrap_or_else(|| get_byte_size(width));
                    let align = unpacked_arrays
                        .get(addr)
                        .map(|layout| layout.element_stride.min(8))
                        .unwrap_or_else(|| get_alignment(width));
                    (*addr, width, is_4state, size, align)
                })
                .collect();

        working_vars_to_layout.sort_by_key(|&(_, _, _, _, align)| std::cmp::Reverse(align));

        let mut working_offsets = HashMap::default();
        let mut working_current_offset = 0;
        for (addr, _width, _is_4state, size, align) in working_vars_to_layout {
            working_current_offset = (working_current_offset + align - 1) & !(align - 1);

            working_offsets.insert(addr, working_current_offset);

            working_current_offset += size;
            if four_state {
                working_current_offset += size;
            }
        }

        let sparse_addrs = program.collect_sparse_working_region_addrs();
        let mut sparse_vars_to_layout: Vec<(AbsoluteAddr, usize, bool, usize, usize)> =
            sparse_addrs
                .iter()
                .map(|addr| {
                    let width = widths[addr];
                    let size = unpacked_arrays
                        .get(addr)
                        .map(|layout| layout.plane_size)
                        .unwrap_or_else(|| get_byte_size(width));
                    let align = unpacked_arrays
                        .get(addr)
                        .map(|layout| layout.element_stride.min(8))
                        .unwrap_or_else(|| get_alignment(width));
                    (*addr, width, is_4states[addr], size, align)
                })
                .collect();
        sparse_vars_to_layout.sort_by_key(|&(_, _, _, _, align)| std::cmp::Reverse(align));
        let mut sparse_offsets = HashMap::default();
        let mut sparse_current_offset = 0usize;
        for (addr, _width, _is_4state, size, align) in sparse_vars_to_layout {
            sparse_current_offset = (sparse_current_offset + align - 1) & !(align - 1);
            sparse_offsets.insert(addr, sparse_current_offset);
            // First-write initialization accesses the final logical chunk as
            // a u64. Keep its tail inside this variable's slot so it cannot
            // overwrite the next sparse value or the dirty metadata.
            let plane_count = if four_state { 2 } else { 1 };
            let final_chunk_size = (size + 7) & !7;
            let physical_extent = (plane_count - 1) * size + final_chunk_size;
            sparse_current_offset += (physical_extent + 7) & !7;
        }

        // Keep working region properly aligned when appended to the stable region.
        let working_base_offset = (current_offset + 7) & !7;
        let sparse_base_offset = (working_base_offset + working_current_offset + 7) & !7;
        let mut sparse_metadata_offset = (sparse_base_offset + sparse_current_offset + 7) & !7;
        let mut sparse_layouts = HashMap::default();
        let mut sparse_order: Vec<_> = sparse_addrs.into_iter().collect();
        sparse_order.sort();
        let sparse_active_capacity = sparse_order.len();
        for (active_index, addr) in sparse_order.into_iter().enumerate() {
            let chunk_count = unpacked_arrays
                .get(&addr)
                .map(|layout| layout.plane_size.div_ceil(8))
                .unwrap_or_else(|| widths[&addr].div_ceil(64));
            let dirty_word_count = chunk_count.div_ceil(64);
            let summary_word_count = dirty_word_count.div_ceil(64);
            let dirty_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += dirty_word_count * 8;
            let summary_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += summary_word_count * 8;
            sparse_layouts.insert(
                addr,
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

        let sparse_active_count_offset = (sparse_metadata_offset + 7) & !7;
        let sparse_active_flags_offset = sparse_active_count_offset + 8;
        let sparse_active_list_offset =
            (sparse_active_flags_offset + sparse_active_capacity + 3) & !3;
        sparse_metadata_offset = sparse_active_list_offset + sparse_active_capacity * 4;

        // Triggered bits region (1 bit per event canonical ID)
        let num_potential_triggers = program.num_events();
        let triggered_bits_offset = (sparse_metadata_offset + 7) & !7;
        let triggered_bits_total_size = num_potential_triggers.div_ceil(8);

        let scratch_base_offset = (triggered_bits_offset + triggered_bits_total_size + 7) & !7;
        let runtime_event_buffer_size =
            RUNTIME_EVENT_HEADER_SIZE + RUNTIME_EVENT_CAPACITY * runtime_event_slot_size;
        let merged_total_size = (scratch_base_offset + scratch_bytes + 7) & !7;

        // Apply address aliases: aliased variables share the canonical's offset.
        // Only alias when:
        // - Both variables have the same 4-state mode
        // - Alias width fits within canonical width
        // - Neither address is used in FF execution units (FF timing requires separate storage)
        let ff_addrs = collect_ff_addresses(program);
        for (alias_addr, canonical_addr) in &program.address_aliases {
            // In 4-state mode, skip 4-state variables: aliasing only shares
            // the value offset, but 4-state also needs mask offset sharing.
            // In 2-state mode (four_state=false), masks are not allocated so all types are safe.
            let fourstate_ok = !four_state
                || (is_4states.get(alias_addr) == Some(&false)
                    && is_4states.get(canonical_addr) == Some(&false));
            let alias_fits = widths
                .get(alias_addr)
                .zip(widths.get(canonical_addr))
                .is_some_and(|(&aw, &cw)| aw <= cw);
            // Only the alias (non-canonical) side must not be in FF.
            // The canonical side can be in FF — aliasing only shares the STABLE
            // region offset, and WORKING offsets are allocated independently.
            let not_in_ff = !ff_addrs.contains(alias_addr);
            if fourstate_ok && alias_fits && not_in_ff {
                if let Some(&canonical_offset) = offsets.get(canonical_addr) {
                    offsets.insert(*alias_addr, canonical_offset);
                }
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
            sparse_active_count_offset,
            sparse_active_flags_offset,
            sparse_active_list_offset,
            sparse_active_capacity,
            merged_total_size,
            triggered_bits_offset,
            triggered_bits_total_size,
            scratch_base_offset,
            scratch_size: scratch_bytes,
            runtime_event_capacity: RUNTIME_EVENT_CAPACITY,
            runtime_event_slot_size,
            runtime_event_buffer_size,
            runtime_event_site_layouts,
        }
    }

    pub fn plane_size(&self, addr: &AbsoluteAddr) -> usize {
        self.unpacked_arrays
            .get(addr)
            .map(|layout| layout.plane_size)
            .unwrap_or_else(|| get_byte_size(self.widths[addr]))
    }

    pub fn map_static_bit_offset(&self, addr: &AbsoluteAddr, bit_offset: usize) -> (usize, usize) {
        let Some(array) = self.unpacked_arrays.get(addr) else {
            return (bit_offset / 8, bit_offset % 8);
        };
        let element = bit_offset / array.element_width;
        let intra_element = bit_offset % array.element_width;
        (
            element * array.element_stride + intra_element / 8,
            intra_element % 8,
        )
    }
}

fn declared_strided_array_layouts(program: &Program) -> HashMap<AbsoluteAddr, UnpackedArrayLayout> {
    let mut layouts = HashMap::default();
    for (instance_id, module_id) in &program.instance_module {
        for info in program.module_variables[module_id].values() {
            let element_count = info.array_dims.iter().copied().product::<usize>();
            if element_count <= 1 || info.width % element_count != 0 {
                continue;
            }
            let element_width = info.width / element_count;
            let element_bytes = get_byte_size(element_width);
            let element_stride = element_bytes;
            layouts.insert(
                AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                },
                UnpackedArrayLayout {
                    element_width,
                    element_count,
                    element_stride,
                    plane_size: element_stride * element_count,
                },
            );
        }
    }
    for (&alias, &canonical) in &program.address_aliases {
        layouts.remove(&alias);
        layouts.remove(&canonical);
    }
    layouts
}

fn collect_strided_array_layouts(program: &Program) -> HashMap<AbsoluteAddr, UnpackedArrayLayout> {
    let mut candidates = declared_strided_array_layouts(program);

    let mut inspect = |inst: &SIRInstruction<crate::ir::RegionedAbsoluteAddr>| {
        let mut check = |addr: &crate::ir::RegionedAbsoluteAddr,
                         offset: &SIROffset,
                         width: usize,
                         whole_commit: bool| {
            let abs = addr.absolute_addr();
            let Some(layout) = candidates.get(&abs).copied() else {
                return;
            };
            let supported = match offset {
                SIROffset::Element {
                    element_width,
                    bit_offset,
                    ..
                } => {
                    *element_width == layout.element_width
                        && bit_offset
                            .checked_add(width)
                            .is_some_and(|end| end <= layout.element_width)
                }
                SIROffset::Static(start) => {
                    let physically_contiguous = layout.element_stride * 8 == layout.element_width;
                    let whole = *start == 0 && width == layout.element_width * layout.element_count;
                    let single_element =
                        start
                            .checked_add(width.saturating_sub(1))
                            .is_some_and(|end| {
                                *start / layout.element_width == end / layout.element_width
                            });
                    physically_contiguous || single_element || (whole_commit && whole)
                }
                SIROffset::Dynamic(_) => false,
            };
            if !supported {
                candidates.remove(&abs);
            }
        };
        match inst {
            SIRInstruction::Load(_, addr, offset, width) => {
                check(addr, offset, *width, false);
            }
            SIRInstruction::Store(addr, offset, width, ..) => {
                check(addr, offset, *width, false);
            }
            SIRInstruction::Commit(src, dst, offset, width, _) => {
                check(src, offset, *width, true);
                check(dst, offset, *width, true);
            }
            _ => {}
        }
    };
    for eu in program
        .eval_comb
        .iter()
        .chain(program.eval_apply_ffs.values().flatten())
        .chain(program.eval_only_ffs.values().flatten())
        .chain(program.apply_ffs.values().flatten())
    {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                inspect(inst);
            }
        }
    }
    candidates
}

fn build_runtime_event_site_layouts(program: &Program) -> Vec<RuntimeEventSiteLayout> {
    program
        .runtime_event_sites
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

/// Collect all absolute addresses referenced in FF execution units.
fn collect_ff_addresses(program: &Program) -> crate::HashSet<AbsoluteAddr> {
    let mut addrs = crate::HashSet::default();
    let ff_eus = program
        .eval_apply_ffs
        .values()
        .flat_map(|v| v.iter())
        .chain(program.eval_only_ffs.values().flat_map(|v| v.iter()))
        .chain(program.apply_ffs.values().flat_map(|v| v.iter()));
    for eu in ff_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, _, _)
                    | SIRInstruction::Store(addr, _, _, _, _, _) => {
                        addrs.insert(addr.absolute_addr());
                    }
                    SIRInstruction::Commit(src, dst, _, _, _) => {
                        addrs.insert(src.absolute_addr());
                        addrs.insert(dst.absolute_addr());
                    }
                    _ => {}
                }
            }
        }
    }
    addrs
}

pub fn get_byte_size(width: usize) -> usize {
    (width + 7) >> 3
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
