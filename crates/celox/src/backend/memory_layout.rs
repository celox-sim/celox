use crate::HashMap;
use crate::ir::{AbsoluteAddr, Program, SIRInstruction};

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
    pub chunk_count: usize,
    pub dirty_words_offset: usize,
    pub dirty_word_count: usize,
    pub summary_words_offset: usize,
    pub summary_word_count: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryLayout {
    pub four_state: bool,
    /// Stable region (region = 0) offsets. Includes all declared variables.
    pub offsets: HashMap<AbsoluteAddr, usize>,
    pub widths: HashMap<AbsoluteAddr, usize>,
    /// Whether the variable is a 4-state type.
    pub is_4states: HashMap<AbsoluteAddr, bool>,
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
    pub fn build(program: &Program, four_state: bool) -> Self {
        let scratch_bytes = match &program.eval_comb_plan {
            Some(crate::ir::EvalCombPlan::MemorySpilled(plan)) => plan.scratch_bytes,
            _ => 0,
        };
        let mut stable_vars_to_layout = Vec::new();

        for (instance_id, module_id) in &program.instance_module {
            let variables = &program.module_variables[module_id];
            for info in variables.values() {
                let addr = AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                };
                stable_vars_to_layout.push((addr, info.width, info.is_4state));
            }
        }

        stable_vars_to_layout.sort_by_key(|&(_, width, _)| std::cmp::Reverse(get_alignment(width)));

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
        for (addr, width, is_4state) in stable_vars_to_layout {
            let align = get_alignment(width);
            let size = get_byte_size(width);
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
        let mut working_vars_to_layout: Vec<(AbsoluteAddr, usize, bool)> = working_addrs
            .iter()
            .map(|addr| {
                let width = widths[addr];
                let is_4state = is_4states[addr];
                (*addr, width, is_4state)
            })
            .collect();

        working_vars_to_layout
            .sort_by_key(|&(_, width, _)| std::cmp::Reverse(get_alignment(width)));

        let mut working_offsets = HashMap::default();
        let mut working_current_offset = 0;
        for (addr, width, _is_4state) in working_vars_to_layout {
            let align = get_alignment(width);
            let size = get_byte_size(width);
            working_current_offset = (working_current_offset + align - 1) & !(align - 1);

            working_offsets.insert(addr, working_current_offset);

            working_current_offset += size;
            if four_state {
                working_current_offset += size;
            }
        }

        let sparse_addrs = program.collect_sparse_working_region_addrs();
        let mut sparse_vars_to_layout: Vec<(AbsoluteAddr, usize, bool)> = sparse_addrs
            .iter()
            .map(|addr| (*addr, widths[addr], is_4states[addr]))
            .collect();
        sparse_vars_to_layout.sort_by_key(|&(_, width, _)| std::cmp::Reverse(get_alignment(width)));
        let mut sparse_offsets = HashMap::default();
        let mut sparse_current_offset = 0usize;
        for (addr, width, _is_4state) in sparse_vars_to_layout {
            let align = get_alignment(width);
            let size = get_byte_size(width);
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
        for addr in sparse_order {
            let chunk_count = widths[&addr].div_ceil(64);
            let dirty_word_count = chunk_count.div_ceil(64);
            let summary_word_count = dirty_word_count.div_ceil(64);
            let dirty_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += dirty_word_count * 8;
            let summary_words_offset = sparse_metadata_offset;
            sparse_metadata_offset += summary_word_count * 8;
            sparse_layouts.insert(
                addr,
                SparseWorkingLayout {
                    chunk_count,
                    dirty_words_offset,
                    dirty_word_count,
                    summary_words_offset,
                    summary_word_count,
                },
            );
        }

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
            offsets,
            widths,
            is_4states,
            total_size: current_offset,
            working_offsets,
            working_base_offset,
            sparse_offsets,
            sparse_base_offset,
            sparse_layouts,
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
