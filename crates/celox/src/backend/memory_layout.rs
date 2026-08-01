use crate::HashMap;
use crate::ir::{
    AbsoluteAddr, Program, RegisterId, SIRInstruction, SIROffset, STABLE_REGION,
    collect_exact_zero_registers,
};
#[cfg(target_arch = "x86_64")]
pub use celox_state_layout::STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET;
pub use celox_state_layout::{
    LayoutInput, LayoutRequirements, LayoutSource, MemoryLayoutMode, StateObjectLayout,
    UnpackedArrayLayout, get_byte_size,
};
#[cfg(not(target_arch = "wasm32"))]
pub use celox_state_layout::{
    RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
    RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
    RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
    STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};

pub type MemoryLayout = celox_state_layout::MemoryLayout<AbsoluteAddr>;

impl LayoutSource<AbsoluteAddr> for Program {
    fn layout_input(&self, mode: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
        let unpacked_arrays = if mode == MemoryLayoutMode::ElementStrided {
            collect_strided_array_layouts(self)
        } else {
            HashMap::default()
        };
        let state_objects = self
            .design
            .state_objects
            .iter()
            .map(|(&address, metadata)| StateObjectLayout {
                address,
                width: metadata.width,
                is_4state: metadata.is_4state,
            })
            .collect();

        LayoutInput {
            state_objects,
            working_addresses: self.collect_working_region_addrs().into_iter().collect(),
            sparse_addresses: self
                .collect_sparse_working_region_addrs()
                .into_iter()
                .collect(),
            unpacked_arrays,
            requirements: self.layout_requirements.clone(),
            ff_referenced_addresses: collect_ff_referenced_addresses(self),
            num_events: self.num_events(),
            runtime_event_sites: self.runtime_schema.runtime_event_sites.clone(),
        }
    }
}

fn declared_strided_array_layouts(program: &Program) -> HashMap<AbsoluteAddr, UnpackedArrayLayout> {
    let mut layouts = HashMap::default();
    for (&address, metadata) in &program.design.state_objects {
        let element_count = metadata.array_dims.iter().copied().product::<usize>();
        if element_count <= 1 || metadata.width % element_count != 0 {
            continue;
        }
        let element_width = metadata.width / element_count;
        let element_bytes = get_byte_size(element_width);
        let element_stride = element_bytes;
        layouts.insert(
            address,
            UnpackedArrayLayout {
                element_width,
                element_count,
                element_stride,
                plane_size: element_stride * element_count,
            },
        );
    }
    for (&alias, &canonical) in program.layout_requirements.state_aliases() {
        layouts.remove(&alias);
        layouts.remove(&canonical);
    }
    layouts
}

fn supports_strided_access(
    layout: UnpackedArrayLayout,
    offset: &SIROffset,
    width: usize,
    whole_object_transfer: bool,
) -> bool {
    match offset {
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
            let single_element = start
                .checked_add(width.saturating_sub(1))
                .is_some_and(|end| *start / layout.element_width == end / layout.element_width);
            physically_contiguous || single_element || (whole_object_transfer && whole)
        }
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => {
            let whole = *bit_offset == 0 && width == layout.element_width * layout.element_count;
            *element_width == layout.element_width
                && ((layout.element_stride * 8 == layout.element_width
                    && bit_offset
                        .checked_add(width)
                        .is_some_and(|end| end <= layout.element_width * layout.element_count))
                    || (whole_object_transfer && whole))
        }
        SIROffset::Dynamic(_) => false,
    }
}

pub(crate) fn collect_strided_array_layouts(
    program: &Program,
) -> HashMap<AbsoluteAddr, UnpackedArrayLayout> {
    let mut candidates = declared_strided_array_layouts(program);

    let mut inspect = |inst: &SIRInstruction<crate::ir::RegionedAbsoluteAddr>,
                       exact_zeros: &crate::HashSet<RegisterId>| {
        let mut check = |addr: &crate::ir::RegionedAbsoluteAddr,
                         offset: &SIROffset,
                         width: usize,
                         whole_object_transfer: bool| {
            let abs = addr.absolute_addr();
            let Some(layout) = candidates.get(&abs).copied() else {
                return;
            };
            if !supports_strided_access(layout, offset, width, whole_object_transfer) {
                candidates.remove(&abs);
            }
        };
        match inst {
            SIRInstruction::Load(_, addr, offset, width) => {
                check(addr, offset, *width, false);
            }
            SIRInstruction::Store(addr, offset, width, source, triggers, capture_sites) => {
                let state_bulk_zero = matches!(
                    addr.region,
                    STABLE_REGION | crate::ir::SPARSE_WORKING_REGION
                ) && triggers.is_empty()
                    && capture_sites.is_empty()
                    && exact_zeros.contains(source);
                check(addr, offset, *width, state_bulk_zero);
            }
            SIRInstruction::Commit(src, dst, offset, width, _) => {
                check(src, offset, *width, true);
                check(dst, offset, *width, true);
            }
            _ => {}
        }
    };
    for eu in program
        .sir
        .eval_comb
        .iter()
        .chain(program.sir.eval_apply_ffs.values().flatten())
        .chain(program.sir.eval_comb_apply_ffs.values().flatten())
        .chain(program.sir.eval_only_ffs.values().flatten())
        .chain(program.sir.apply_ffs.values().flatten())
    {
        let mut zero_roots = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, offset, width, source, ..)
                    if matches!(
                        address.region,
                        STABLE_REGION | crate::ir::SPARSE_WORKING_REGION
                    ) && offset.constant_bit_offset() == Some(0)
                        && *width > 64 =>
                {
                    Some(*source)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        zero_roots.sort_unstable();
        zero_roots.dedup();
        let exact_zeros = collect_exact_zero_registers(eu, zero_roots);
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                inspect(inst, &exact_zeros);
            }
        }
    }
    candidates
}

/// Collect whole objects referenced by FF.
///
/// FF reads cannot share a persistent home without a phase-correct StateSSA
/// proof that the identity definition precedes the read on every event path.
fn collect_ff_referenced_addresses(program: &Program) -> crate::HashSet<AbsoluteAddr> {
    let mut addrs = crate::HashSet::default();
    // eval_comb_apply_ffs contains the complete comb graph, not just FF
    // state.  The split FF forms are retained as the authoritative inventory;
    // scanning the fused form would conservatively reject every useful comb
    // identity alias.
    let ff_eus = program
        .sir
        .eval_apply_ffs
        .values()
        .flat_map(|v| v.iter())
        .chain(program.sir.eval_only_ffs.values().flat_map(|v| v.iter()))
        .chain(program.sir.apply_ffs.values().flat_map(|v| v.iter()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_array_accepts_only_semantic_whole_object_transfers() {
        let layout = UnpackedArrayLayout {
            element_width: 51,
            element_count: 4096,
            element_stride: 8,
            plane_size: 4096 * 8,
        };
        let whole_width = 51 * 4096;

        assert!(!supports_strided_access(
            layout,
            &SIROffset::Static(0),
            whole_width,
            false,
        ));
        assert!(supports_strided_access(
            layout,
            &SIROffset::Static(0),
            whole_width,
            true,
        ));
        assert!(!supports_strided_access(
            layout,
            &SIROffset::Static(51),
            whole_width - 51,
            true,
        ));
    }

    #[test]
    fn packed_elements_access_requires_physically_packed_storage() {
        let padded = UnpackedArrayLayout {
            element_width: 1,
            element_count: 32,
            element_stride: 1,
            plane_size: 32,
        };
        let packed = UnpackedArrayLayout {
            element_width: 8,
            element_count: 4,
            element_stride: 1,
            plane_size: 4,
        };

        assert!(!supports_strided_access(
            padded,
            &SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 1,
            },
            32,
            false,
        ));
        assert!(supports_strided_access(
            packed,
            &SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 8,
            },
            32,
            false,
        ));
        assert!(supports_strided_access(
            padded,
            &SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 1,
            },
            32,
            true,
        ));
        assert!(!supports_strided_access(
            padded,
            &SIROffset::PackedElements {
                bit_offset: 1,
                element_width: 1,
            },
            31,
            true,
        ));
    }
}
