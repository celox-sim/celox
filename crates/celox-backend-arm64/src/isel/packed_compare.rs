//! AArch64 NEON packed-compare selection.

use crate::mir::CmpKind;
use crate::{
    BinaryOp, HashMap, HashSet, MemoryLayout, RegionedAbsoluteAddr, RegisterId, RegisterType,
    SIRInstruction, SIROffset,
};
use celox_sir::analysis::{
    ExactU64Constant as ExactSirConstant, UseSite as SirUseSite,
    block_instruction_definitions as collect_sir_defs, exact_u64_constant as exact_sir_constant,
    instruction_definition as sir_def_reg,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PackedLaneComparePlanRhs {
    Scalar(RegisterId),
    Memory(RegionedAbsoluteAddr),
}

#[derive(Debug, Clone)]
pub(super) struct PackedLaneComparePlan {
    pub(super) dst: RegisterId,
    pub(super) rhs: PackedLaneComparePlanRhs,
    pub(super) kind: CmpKind,
    pub(super) address: RegionedAbsoluteAddr,
    pub(super) lane_count: usize,
    pub(super) element_stride: usize,
    pub(super) bit_offset: usize,
    pub(super) field_width: usize,
    covered_indices: Vec<usize>,
}

#[derive(Debug, Default)]
pub(super) struct PackedLaneComparePlans {
    pub(super) roots: HashMap<usize, PackedLaneComparePlan>,
    pub(super) skip_indices: HashSet<usize>,
}

#[derive(Debug, Clone)]
pub(super) struct PackedByteAffineComparePlan {
    pub(super) dst: RegisterId,
    pub(super) base: RegisterId,
    pub(super) rhs: RegisterId,
    pub(super) kind: CmpKind,
    covered_indices: Vec<usize>,
}

#[derive(Debug, Default)]
pub(super) struct PackedByteAffineComparePlans {
    pub(super) roots: HashMap<usize, PackedByteAffineComparePlan>,
    pub(super) skip_indices: HashSet<usize>,
}

#[derive(Clone, Copy)]
struct StaticLaneLoad {
    load_index: usize,
    slice_index: Option<usize>,
    address: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
}

fn static_lane_load(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    register_types: &HashMap<RegisterId, RegisterType>,
    layout: &MemoryLayout,
    register: RegisterId,
    lane: usize,
) -> Option<StaticLaneLoad> {
    fn constant_offset(
        block: &crate::BasicBlock<RegionedAbsoluteAddr>,
        defs: &HashMap<RegisterId, usize>,
        constants: &HashMap<RegisterId, ExactSirConstant>,
        register_types: &HashMap<RegisterId, RegisterType>,
        register: RegisterId,
        active: &mut HashSet<RegisterId>,
    ) -> Option<u64> {
        if let Some(value) = constants.get(&register) {
            return Some(value.value);
        }
        if !active.insert(register) {
            return None;
        }
        let definition = *defs.get(&register)?;
        let value = match block.instructions.get(definition)? {
            SIRInstruction::Imm(_, value) => exact_sir_constant(value)?.value,
            SIRInstruction::Binary(_, lhs, op, rhs) => {
                let lhs = constant_offset(block, defs, constants, register_types, *lhs, active)?;
                let rhs = constant_offset(block, defs, constants, register_types, *rhs, active)?;
                match op {
                    BinaryOp::Add => lhs.wrapping_add(rhs),
                    BinaryOp::Mul => lhs.wrapping_mul(rhs),
                    _ => return None,
                }
            }
            _ => return None,
        };
        active.remove(&register);
        let width = register_types.get(&register)?.width();
        if width > 64 {
            return None;
        }
        Some(if width == 64 {
            value
        } else {
            value & ((1_u64 << width) - 1)
        })
    }

    let load = |index: usize,
                address: RegionedAbsoluteAddr,
                offset: &SIROffset,
                width: usize|
     -> Option<StaticLaneLoad> {
        let offset = match offset {
            SIROffset::Static(offset)
            | SIROffset::PackedElements {
                bit_offset: offset, ..
            } => *offset,
            SIROffset::Dynamic(offset) => {
                let value = constant_offset(
                    block,
                    defs,
                    constants,
                    register_types,
                    *offset,
                    &mut HashSet::default(),
                );
                usize::try_from(value?).ok()?
            }
            SIROffset::Element { .. } => return None,
        };
        Some(StaticLaneLoad {
            load_index: index,
            slice_index: None,
            address,
            offset,
            width,
        })
    };
    let definition = *defs.get(&register)?;
    let lane_load = match block.instructions.get(definition)? {
        SIRInstruction::Load(_, address, offset, width) => {
            load(definition, *address, offset, *width)?
        }
        SIRInstruction::Slice(_, source, slice_offset, slice_width) => {
            let load_index = *defs.get(source)?;
            let SIRInstruction::Load(_, address, offset, load_width) =
                block.instructions.get(load_index)?
            else {
                return None;
            };
            let mut load = load(load_index, *address, offset, *load_width)?;
            load.slice_index = Some(definition);
            load.offset = load.offset.checked_add(*slice_offset)?;
            load.width = *slice_width;
            load
        }
        _ => return None,
    };
    let belongs_to_lane = layout
        .unpacked_arrays
        .get(&lane_load.address.absolute_addr())
        .map_or_else(
            || {
                lane_load.width.is_multiple_of(8)
                    && lane_load.offset == lane.saturating_mul(lane_load.width)
            },
            |array| lane_load.offset / array.element_width == lane,
        );
    belongs_to_lane.then_some(lane_load)
}

fn packed_compare_kind(op: BinaryOp) -> Option<CmpKind> {
    Some(match op {
        BinaryOp::Eq => CmpKind::Eq,
        BinaryOp::Ne => CmpKind::Ne,
        BinaryOp::LtU => CmpKind::LtU,
        BinaryOp::LtS => CmpKind::LtS,
        BinaryOp::LeU => CmpKind::LeU,
        BinaryOp::LeS => CmpKind::LeS,
        BinaryOp::GtU => CmpKind::GtU,
        BinaryOp::GtS => CmpKind::GtS,
        BinaryOp::GeU => CmpKind::GeU,
        BinaryOp::GeS => CmpKind::GeS,
        _ => return None,
    })
}

fn swap_compare_kind(kind: CmpKind) -> CmpKind {
    match kind {
        CmpKind::Eq => CmpKind::Eq,
        CmpKind::Ne => CmpKind::Ne,
        CmpKind::LtU => CmpKind::GtU,
        CmpKind::LtS => CmpKind::GtS,
        CmpKind::LeU => CmpKind::GeU,
        CmpKind::LeS => CmpKind::GeS,
        CmpKind::GtU => CmpKind::LtU,
        CmpKind::GtS => CmpKind::LtS,
        CmpKind::GeU => CmpKind::LeU,
        CmpKind::GeS => CmpKind::LeS,
    }
}

fn byte_affine_lane(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    register_types: &HashMap<RegisterId, RegisterType>,
    register: RegisterId,
    lane: usize,
) -> Option<(RegisterId, usize)> {
    fn constant_in_block(
        block: &crate::BasicBlock<RegionedAbsoluteAddr>,
        defs: &HashMap<RegisterId, usize>,
        constants: &HashMap<RegisterId, ExactSirConstant>,
        register_types: &HashMap<RegisterId, RegisterType>,
        register: RegisterId,
        active: &mut HashSet<RegisterId>,
    ) -> Option<u64> {
        if let Some(value) = constants.get(&register) {
            return Some(value.value);
        }
        if !active.insert(register) {
            return None;
        }
        let definition = *defs.get(&register)?;
        let value = match block.instructions.get(definition)? {
            SIRInstruction::Imm(_, value) => exact_sir_constant(value)?.value,
            SIRInstruction::Binary(_, lhs, operation, rhs) => {
                let lhs = constant_in_block(block, defs, constants, register_types, *lhs, active)?;
                let rhs = constant_in_block(block, defs, constants, register_types, *rhs, active)?;
                match operation {
                    BinaryOp::Add => lhs.wrapping_add(rhs),
                    BinaryOp::Sub => lhs.wrapping_sub(rhs),
                    BinaryOp::Mul => lhs.wrapping_mul(rhs),
                    BinaryOp::And => lhs & rhs,
                    BinaryOp::Or => lhs | rhs,
                    BinaryOp::Xor => lhs ^ rhs,
                    BinaryOp::Shr => u32::try_from(rhs)
                        .ok()
                        .and_then(|rhs| lhs.checked_shr(rhs))
                        .unwrap_or(0),
                    BinaryOp::Shl => u32::try_from(rhs)
                        .ok()
                        .and_then(|rhs| lhs.checked_shl(rhs))
                        .unwrap_or(0),
                    _ => return None,
                }
            }
            _ => return None,
        };
        active.remove(&register);
        let width = register_types.get(&register)?.width();
        if width > 64 {
            return None;
        }
        Some(if width == 64 {
            value
        } else {
            value & ((1_u64 << width) - 1)
        })
    }

    if register_types.get(&register)?.width() != 8 {
        return None;
    }
    let definition = *defs.get(&register)?;
    let SIRInstruction::Binary(dst, lhs, BinaryOp::Add, rhs) =
        block.instructions.get(definition)?
    else {
        return None;
    };
    if *dst != register {
        return None;
    }
    let lhs_constant = constant_in_block(
        block,
        defs,
        constants,
        register_types,
        *lhs,
        &mut HashSet::default(),
    );
    let rhs_constant = constant_in_block(
        block,
        defs,
        constants,
        register_types,
        *rhs,
        &mut HashSet::default(),
    );
    let (base, increment) = match (lhs_constant, rhs_constant) {
        (None, Some(increment)) => (*lhs, increment),
        (Some(increment), None) => (*rhs, increment),
        _ => return None,
    };
    if register_types.get(&base)?.width() != 8 || increment != lane as u64 {
        return None;
    }
    Some((base, definition))
}

/// Recognize a complete 16-bit predicate pack whose lanes compare
/// `(base + lane) mod 256` with one scalar byte. This common HDL array-bound
/// shape is one byte-vector operation; scalarizing every lane creates a long
/// compare/shift/or chain and unnecessary register pressure.
pub(super) fn find_packed_byte_affine_compare_plans(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> PackedByteAffineComparePlans {
    let mut result = PackedByteAffineComparePlans::default();
    let defs = collect_sir_defs(block);
    for (root_idx, instruction) in block.instructions.iter().enumerate() {
        let SIRInstruction::Concat(dst, predicates) = instruction else {
            continue;
        };
        if predicates.len() != 16 || register_types[dst].width() != 16 {
            continue;
        }
        let mut base = None;
        let mut rhs_scalar = None;
        let mut kind = None;
        let mut covered = vec![root_idx];
        let mut valid = true;
        for (position, &predicate) in predicates.iter().enumerate() {
            let lane = predicates.len() - position - 1;
            let Some(&compare_idx) = defs.get(&predicate).filter(|&&idx| idx < root_idx) else {
                valid = false;
                break;
            };
            let SIRInstruction::Binary(compare_dst, lhs, operation, rhs) =
                &block.instructions[compare_idx]
            else {
                valid = false;
                break;
            };
            if *compare_dst != predicate
                || uses.get(compare_dst).is_none_or(|sites| sites.len() != 1)
            {
                valid = false;
                break;
            }
            let Some(compare_kind) = packed_compare_kind(*operation) else {
                valid = false;
                break;
            };
            let lhs_affine = byte_affine_lane(block, &defs, constants, register_types, *lhs, lane);
            let rhs_affine = byte_affine_lane(block, &defs, constants, register_types, *rhs, lane);
            let (lane_base, add_idx, scalar, compare_kind) = match (lhs_affine, rhs_affine) {
                (Some((lane_base, add_idx)), None) => (lane_base, add_idx, *rhs, compare_kind),
                (None, Some((lane_base, add_idx))) => {
                    (lane_base, add_idx, *lhs, swap_compare_kind(compare_kind))
                }
                _ => {
                    valid = false;
                    break;
                }
            };
            if register_types.get(&scalar).map(RegisterType::width) != Some(8)
                || base.is_some_and(|previous| previous != lane_base)
                || rhs_scalar.is_some_and(|previous| previous != scalar)
                || kind.is_some_and(|previous| previous != compare_kind)
            {
                valid = false;
                break;
            }
            base = Some(lane_base);
            rhs_scalar = Some(scalar);
            kind = Some(compare_kind);
            covered.extend([add_idx, compare_idx]);
        }
        if valid {
            result.roots.insert(
                root_idx,
                PackedByteAffineComparePlan {
                    dst: *dst,
                    base: base.expect("nonempty affine compare pack has a base"),
                    rhs: rhs_scalar.expect("nonempty affine compare pack has a scalar RHS"),
                    kind: kind.expect("nonempty affine compare pack has a comparison kind"),
                    covered_indices: covered,
                },
            );
        }
    }
    let covered = result
        .roots
        .values()
        .flat_map(|plan| plan.covered_indices.iter().copied())
        .collect::<HashSet<_>>();
    for &index in &covered {
        let is_root = result.roots.contains_key(&index);
        let all_uses_covered = sir_def_reg(&block.instructions[index]).is_some_and(|definition| {
            uses.get(&definition).is_none_or(|sites| {
                sites.iter().all(|site| {
                    site.block == block.id
                        && site
                            .inst_idx
                            .is_some_and(|use_idx| covered.contains(&use_idx))
                })
            })
        });
        if is_root || all_uses_covered {
            result.skip_indices.insert(index);
        }
    }
    result
}

/// Recognize the canonical SIR representation of a lane-wise array compare:
/// lane-aligned loads and comparisons followed by one Concat. Block scheduling
/// may interleave independent work, so memory-version safety is proved by
/// checking the interval from the earliest load to the root for an object write.
pub(super) fn find_packed_lane_compare_plans(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    layout: &MemoryLayout,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> PackedLaneComparePlans {
    let mut result = PackedLaneComparePlans::default();
    let defs = collect_sir_defs(block);
    for (root_idx, instruction) in block.instructions.iter().enumerate() {
        let SIRInstruction::Concat(dst, predicates) = instruction else {
            continue;
        };
        let lane_count = predicates.len();
        if lane_count == 0 || lane_count > 64 || register_types[dst].width() != lane_count {
            continue;
        }
        let mut rhs_plan = None;
        let mut kind = None;
        let mut address = None;
        let mut element_width = None;
        let mut bit_offset = None;
        let mut field_width = None;
        let mut valid = true;
        let mut covered = vec![root_idx];
        let mut earliest_load = root_idx;

        for (position, &predicate) in predicates.iter().enumerate() {
            let Some(&compare_idx) = defs.get(&predicate).filter(|&&idx| idx < root_idx) else {
                valid = false;
                break;
            };
            let SIRInstruction::Binary(compare_dst, lhs, compare_op, rhs) =
                &block.instructions[compare_idx]
            else {
                valid = false;
                break;
            };
            if *compare_dst != predicate
                || uses.get(compare_dst).is_none_or(|sites| sites.len() != 1)
            {
                valid = false;
                break;
            }
            let Some(base_kind) = packed_compare_kind(*compare_op) else {
                valid = false;
                break;
            };
            let lane = lane_count - position - 1;
            let lhs_load =
                static_lane_load(block, &defs, constants, register_types, layout, *lhs, lane);
            let mut rhs_load =
                static_lane_load(block, &defs, constants, register_types, layout, *rhs, lane);
            if matches!(rhs_plan, Some(PackedLaneComparePlanRhs::Scalar(value)) if value == *rhs) {
                rhs_load = None;
            }
            let (lhs_load, current_rhs, current_kind) = match (lhs_load, rhs_load) {
                (Some(lhs_load), Some(rhs_load)) => (
                    lhs_load,
                    PackedLaneComparePlanRhs::Memory(rhs_load.address),
                    base_kind,
                ),
                (Some(lhs_load), None) => {
                    (lhs_load, PackedLaneComparePlanRhs::Scalar(*rhs), base_kind)
                }
                (None, Some(rhs_load)) => (
                    rhs_load,
                    PackedLaneComparePlanRhs::Scalar(*lhs),
                    swap_compare_kind(base_kind),
                ),
                (None, None) => {
                    valid = false;
                    break;
                }
            };
            let array = layout
                .unpacked_arrays
                .get(&lhs_load.address.absolute_addr());
            let current_element_width = array.map_or(lhs_load.width, |array| array.element_width);
            let current_element_stride =
                array.map_or(lhs_load.width / 8, |array| array.element_stride);
            let within = lhs_load.offset % current_element_width;
            if register_types[lhs].width() != register_types[rhs].width()
                || register_types[lhs].width() != lhs_load.width
                || rhs_plan.is_some_and(|previous| previous != current_rhs)
                || kind.is_some_and(|previous| previous != current_kind)
                || address.is_some_and(|previous| previous != lhs_load.address)
                || field_width.is_some_and(|previous| previous != lhs_load.width)
                || within
                    .checked_add(lhs_load.width)
                    .is_none_or(|end| end > current_element_width)
                || element_width.is_some_and(|previous| previous != current_element_width)
                || bit_offset.is_some_and(|previous| previous != within)
            {
                valid = false;
                break;
            }
            if let Some(rhs_load) = rhs_load {
                let rhs_array = layout
                    .unpacked_arrays
                    .get(&rhs_load.address.absolute_addr());
                let rhs_element_width =
                    rhs_array.map_or(rhs_load.width, |array| array.element_width);
                let rhs_element_stride =
                    rhs_array.map_or(rhs_load.width / 8, |array| array.element_stride);
                if rhs_load.width != lhs_load.width
                    || rhs_load.offset % rhs_element_width != within
                    || rhs_element_width != current_element_width
                    || rhs_element_stride != current_element_stride
                {
                    valid = false;
                    break;
                }
                earliest_load = earliest_load.min(rhs_load.load_index);
                covered.push(rhs_load.load_index);
                if let Some(slice_index) = rhs_load.slice_index {
                    covered.push(slice_index);
                }
            }
            rhs_plan = Some(current_rhs);
            kind = Some(current_kind);
            address = Some(lhs_load.address);
            element_width = Some(current_element_width);
            bit_offset = Some(within);
            field_width = Some(lhs_load.width);
            earliest_load = earliest_load.min(lhs_load.load_index);
            covered.extend([lhs_load.load_index, compare_idx]);
            if let Some(slice_index) = lhs_load.slice_index {
                covered.push(slice_index);
            }
        }
        if !valid {
            continue;
        }
        let address = address.expect("nonempty compare pack has an address");
        let element_stride = layout
            .unpacked_arrays
            .get(&address.absolute_addr())
            .map_or_else(|| field_width.unwrap() / 8, |array| array.element_stride);
        let bit_offset = bit_offset.unwrap();
        let field_width = field_width.unwrap();
        let kind = kind.unwrap();
        let physical_bytes = lane_count.saturating_mul(element_stride);
        if !matches!(element_stride, 1 | 2 | 4)
            || physical_bytes == 0
            || !physical_bytes.is_multiple_of(16)
            || bit_offset + field_width > element_stride * 8
            || (element_stride == 1 && bit_offset != 0)
            || (!matches!(kind, CmpKind::Eq | CmpKind::Ne) && field_width != element_stride * 8)
        {
            continue;
        }
        if block.instructions[earliest_load..root_idx]
            .iter()
            .any(|instruction| match instruction {
                SIRInstruction::Store(destination, ..) => {
                    *destination == address
                        || matches!(rhs_plan, Some(PackedLaneComparePlanRhs::Memory(rhs)) if *destination == rhs)
                }
                SIRInstruction::Commit(_, destination, ..) => {
                    *destination == address
                        || matches!(rhs_plan, Some(PackedLaneComparePlanRhs::Memory(rhs)) if *destination == rhs)
                }
                _ => false,
            })
        {
            continue;
        }
        result.roots.insert(
            root_idx,
            PackedLaneComparePlan {
                dst: *dst,
                rhs: rhs_plan.unwrap(),
                kind,
                address,
                lane_count,
                element_stride,
                bit_offset,
                field_width,
                covered_indices: covered,
            },
        );
    }
    let covered = result
        .roots
        .values()
        .flat_map(|plan| plan.covered_indices.iter().copied())
        .collect::<HashSet<_>>();
    for &index in &covered {
        let is_root = result.roots.contains_key(&index);
        let all_uses_covered = sir_def_reg(&block.instructions[index]).is_some_and(|definition| {
            uses.get(&definition).is_none_or(|sites| {
                sites.iter().all(|site| {
                    site.block == block.id
                        && site
                            .inst_idx
                            .is_some_and(|use_idx| covered.contains(&use_idx))
                })
            })
        });
        if is_root || all_uses_covered {
            result.skip_indices.insert(index);
        }
    }
    result
}
