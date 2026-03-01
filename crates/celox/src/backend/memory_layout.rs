use crate::ir::{AbsoluteAddr, Program};
use crate::{HashMap, SimulatorOptions};

#[derive(Debug, Clone)]
pub struct MemoryLayout {
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
    /// Unified memory buffer size in bytes (stable + working).
    pub merged_total_size: usize,

    /// Bitset of triggered domain IDs.
    pub triggered_bits_offset: usize,
    pub triggered_bits_total_size: usize,
}

impl MemoryLayout {
    pub fn build(program: &Program, options: &SimulatorOptions) -> Self {
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
        let mut current_offset = 0;

        // 3. Execute packing
        for (addr, width, is_4state) in stable_vars_to_layout {
            let align = get_alignment(width);
            let size = get_byte_size(width);
            current_offset = (current_offset + align - 1) & !(align - 1);

            offsets.insert(addr, current_offset);
            widths.insert(addr, width);
            is_4states.insert(addr, is_4state);

            current_offset += size;
            if options.four_state {
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
            if options.four_state {
                working_current_offset += size;
            }
        }

        // Keep working region properly aligned when appended to the stable region.
        let working_base_offset = (current_offset + 7) & !7;

        // Triggered bits region (1 bit per event canonical ID)
        let num_potential_triggers = program.num_events();
        let triggered_bits_offset = (working_base_offset + working_current_offset + 7) & !7;
        let triggered_bits_total_size = (num_potential_triggers + 7) / 8;

        let merged_total_size = (triggered_bits_offset + triggered_bits_total_size + 7) & !7;

        Self {
            offsets,
            widths,
            is_4states,
            total_size: current_offset,
            working_offsets,
            working_base_offset,
            merged_total_size,
            triggered_bits_offset,
            triggered_bits_total_size,
        }
    }
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

