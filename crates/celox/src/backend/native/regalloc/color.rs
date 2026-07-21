//! Streaming chordal coloring for verified, post-spill SSA MIR.
//!
//! This is the dominance-order algorithm from Hack--Grund--Goos.  It keeps
//! only the colors live at the current program point: no program-point live
//! table and no explicit interference graph are constructed.

use std::collections::HashMap;
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, MInst, PhiNode, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{
    ALLOCATABLE_REGS, AssignmentMap, EdgeLocation, PhysReg, RegConstraint, clobbers, is_reg_shift,
    use_constraints,
};
use super::cfg::NormalizedCfg;
use super::legalize::{PermBoundary, PermModel};

const PHYSICAL_COLOR_COUNT: usize = 16;

#[derive(Clone, Copy)]
struct ColorMask(u16);

impl ColorMask {
    const fn empty() -> Self {
        Self(0)
    }

    fn insert(&mut self, register: PhysReg) {
        self.0 |= register_bit(register);
    }

    fn contains(self, register: PhysReg) -> bool {
        self.0 & register_bit(register) != 0
    }
}

#[derive(Debug)]
pub(super) struct ColoringResult {
    pub(super) assignment: AssignmentMap,
    /// The matching selected at each Perm, keyed by its fresh destination.
    pub(super) perm_matching: HashMap<VReg, PhysReg>,
}

/// A structured failure of the coloring phase.  These failures identify a
/// violated producer/verifier contract; they never request another spill pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ColorError {
    pub(super) rule: &'static str,
    pub(super) block: BlockId,
    pub(super) instruction: Option<usize>,
    pub(super) value: Option<VReg>,
    pub(super) related: Option<VReg>,
    pub(super) register: Option<PhysReg>,
}

impl ColorError {
    fn at_block(rule: &'static str, block: BlockId) -> Self {
        Self {
            rule,
            block,
            instruction: None,
            value: None,
            related: None,
            register: None,
        }
    }

    fn at_value(
        rule: &'static str,
        block: BlockId,
        instruction: Option<usize>,
        value: VReg,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            value: Some(value),
            related: None,
            register: None,
        }
    }
}

impl fmt::Display for ColorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.rule, self.block)?;
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if let Some(value) = self.value {
            write!(formatter, " value={value}")?;
        }
        if let Some(related) = self.related {
            write!(formatter, " related={related}")?;
        }
        if let Some(register) = self.register {
            write!(formatter, " register={register}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ColorError {}

/// Color strict SSA in dominance order without retaining per-instruction live
/// sets or an interference graph.
pub(super) fn color_ssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
    analysis: &AnalysisResult,
    perms: &PermModel,
    register_count: usize,
) -> Result<ColoringResult, ColorError> {
    let entry = func
        .blocks
        .first()
        .map(|block| block.id)
        .unwrap_or(BlockId(0));
    if func.blocks.is_empty()
        || cfg.idom.len() != func.blocks.len()
        || analysis.entry_distances.len() != func.blocks.len()
        || analysis.exit_distances.len() != func.blocks.len()
        || register_count > ALLOCATABLE_REGS.len()
    {
        return Err(ColorError::at_block("color.cfg-shape", entry));
    }
    let registers = &ALLOCATABLE_REGS[..register_count];
    let required = required_colors(func)?;
    let forbidden = forbidden_colors(func, analysis)?;
    let preferences = phi_preferences(func, cfg);
    let mut colors = vec![None; func.vregs.count() as usize];
    let mut perm_matching = HashMap::<VReg, PhysReg>::new();
    let mut boundary_for_block = HashMap::<BlockId, &PermBoundary>::new();
    for boundary in &perms.boundaries {
        if boundary_for_block
            .insert(boundary.block, boundary)
            .is_some()
        {
            return Err(ColorError::at_block(
                "color.duplicate-perm-boundary",
                boundary.block,
            ));
        }
    }

    let mut children = vec![Vec::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let Some(parent) = cfg.idom[block] else {
            return Err(ColorError::at_block(
                "color.missing-immediate-dominator",
                func.blocks[block].id,
            ));
        };
        if parent >= func.blocks.len() {
            return Err(ColorError::at_block(
                "color.invalid-immediate-dominator",
                func.blocks[block].id,
            ));
        }
        children[parent].push(block);
    }

    let mut work = vec![0usize];
    while let Some(block_index) = work.pop() {
        let block = &func.blocks[block_index];
        let mut last_use = HashMap::<VReg, usize>::new();
        for (instruction, inst) in block.insts.iter().enumerate() {
            for value in inst.uses() {
                last_use.insert(value, instruction);
            }
        }
        let live_out = &analysis.exit_distances[block_index];
        let mut active = ActiveColors::default();
        for &value in analysis.entry_distances[block_index].keys() {
            let register = color_of(&colors, value).ok_or_else(|| {
                ColorError::at_value("color.live-in-without-color", block.id, None, value)
            })?;
            active.add(block.id, None, value, register)?;
        }

        let boundary = boundary_for_block.get(&block.id).copied();
        let perm_rows = boundary.map_or(&[][..], |boundary| boundary.rows.as_slice());
        if boundary.is_some() && !active.is_empty() {
            return Err(ColorError::at_block(
                "color.perm-does-not-cut-live-in",
                block.id,
            ));
        }
        if let Some(boundary) = boundary {
            let Some(matching) = boundary.match_colors(|source| color_of(&colors, source)) else {
                return Err(ColorError::at_block(
                    "color.perm-has-no-perfect-matching",
                    block.id,
                ));
            };
            for row in &boundary.rows {
                if !block.phis.iter().any(|phi| phi.dst == row.destination) {
                    return Err(ColorError::at_value(
                        "color.perm-destination-is-not-phi",
                        block.id,
                        None,
                        row.destination,
                    ));
                }
                if color_of(&colors, row.source).is_none() {
                    return Err(ColorError::at_value(
                        "color.perm-source-without-color",
                        block.id,
                        None,
                        row.source,
                    ));
                }
                let Some(&register) = matching.get(&row.destination) else {
                    return Err(ColorError::at_value(
                        "color.incomplete-perm-matching",
                        block.id,
                        None,
                        row.destination,
                    ));
                };
                let destination = row.destination.0 as usize;
                if !registers.contains(&register)
                    || required.get(destination).is_none_or(|required| {
                        required.is_some_and(|required| required != register)
                    })
                    || forbidden
                        .get(destination)
                        .is_none_or(|forbidden| forbidden.contains(register))
                {
                    let mut error = ColorError::at_value(
                        "color.invalid-perm-matched-color",
                        block.id,
                        None,
                        row.destination,
                    );
                    error.register = Some(register);
                    return Err(error);
                }
                set_color(
                    &mut colors,
                    block.id,
                    None,
                    row.destination,
                    register,
                    "color.perm-destination-out-of-range",
                )?;
                if perm_matching.insert(row.destination, register).is_some() {
                    return Err(ColorError::at_value(
                        "color.duplicate-perm-destination",
                        block.id,
                        None,
                        row.destination,
                    ));
                }
            }
        }

        // Phi/Perm results are simultaneous definitions at block entry. Dead
        // ordinary phis need a complete assignment for verification but do not
        // interfere with any live value, so color them before installing the
        // live bundle. Perm rows are already matched as one constrained bundle.
        for phi in &block.phis {
            let is_perm = perm_rows.iter().any(|row| row.destination == phi.dst);
            let is_live = is_live_after_entry(phi.dst, &last_use, live_out);
            if is_perm && !is_live {
                return Err(ColorError::at_value(
                    "color.dead-perm-row",
                    block.id,
                    None,
                    phi.dst,
                ));
            }
            if !is_perm && !is_live {
                let register = choose_color(
                    block.id,
                    None,
                    phi.dst,
                    registers,
                    &required,
                    &forbidden,
                    [None, None],
                    &preferences,
                    &colors,
                    &active,
                )?;
                set_color(
                    &mut colors,
                    block.id,
                    None,
                    phi.dst,
                    register,
                    "color.phi-out-of-range",
                )?;
            }
        }

        for phi in &block.phis {
            if !is_live_after_entry(phi.dst, &last_use, live_out)
                || !perm_rows.iter().any(|row| row.destination == phi.dst)
            {
                continue;
            }
            let register = color_of(&colors, phi.dst).ok_or_else(|| {
                ColorError::at_value("color.phi-without-color", block.id, None, phi.dst)
            })?;
            active.add(block.id, None, phi.dst, register)?;
        }

        let ordinary_live_phis = block
            .phis
            .iter()
            .filter(|phi| {
                is_live_after_entry(phi.dst, &last_use, live_out)
                    && !perm_rows.iter().any(|row| row.destination == phi.dst)
            })
            .collect::<Vec<_>>();
        let ordinary_live_colors = choose_phi_bundle_colors(
            block.id,
            &ordinary_live_phis,
            registers,
            &required,
            &forbidden,
            &preferences,
            &colors,
            &active,
        )?;
        for (phi, register) in ordinary_live_phis.into_iter().zip(ordinary_live_colors) {
            set_color(
                &mut colors,
                block.id,
                None,
                phi.dst,
                register,
                "color.phi-out-of-range",
            )?;
            active.add(block.id, None, phi.dst, register)?;
        }

        for (instruction, inst) in block.insts.iter().enumerate() {
            let uses = inst.uses();
            for (operand, &value) in uses.iter().enumerate() {
                if uses[..operand].contains(&value) {
                    continue;
                }
                let register = color_of(&colors, value).ok_or_else(|| {
                    ColorError::at_value(
                        "color.use-without-color",
                        block.id,
                        Some(instruction),
                        value,
                    )
                })?;
                active.require(block.id, instruction, value, register)?;
            }
            for (operand, &value) in uses.iter().enumerate() {
                if uses[..operand].contains(&value) {
                    continue;
                }
                if last_use.get(&value) == Some(&instruction) && !live_out.contains_key(&value) {
                    let register = color_of(&colors, value).ok_or_else(|| {
                        ColorError::at_value(
                            "color.last-use-without-color",
                            block.id,
                            Some(instruction),
                            value,
                        )
                    })?;
                    active.remove(block.id, instruction, value, register)?;
                }
            }

            if let Some(definition) = inst.def() {
                if color_of(&colors, definition).is_some() {
                    return Err(ColorError::at_value(
                        "color.duplicate-definition",
                        block.id,
                        Some(instruction),
                        definition,
                    ));
                }
                let register = choose_color(
                    block.id,
                    Some(instruction),
                    definition,
                    registers,
                    &required,
                    &forbidden,
                    definition_preferences(inst),
                    &preferences,
                    &colors,
                    &active,
                )?;
                set_color(
                    &mut colors,
                    block.id,
                    Some(instruction),
                    definition,
                    register,
                    "color.definition-out-of-range",
                )?;
                if last_use.contains_key(&definition) || live_out.contains_key(&definition) {
                    active.add(block.id, Some(instruction), definition, register)?;
                }
            }
        }

        work.extend(children[block_index].iter().rev().copied());
    }

    let mut assignment = AssignmentMap::default();
    for (value, register) in colors.into_iter().enumerate() {
        if let Some(register) = register {
            assignment.set(VReg(value as u32), register);
        }
    }
    for successor in &func.blocks {
        for phi in &successor.phis {
            for &(predecessor, source) in &phi.sources {
                let Some(register) = assignment.get(source) else {
                    return Err(ColorError::at_value(
                        "color.edge-source-without-color",
                        successor.id,
                        None,
                        source,
                    ));
                };
                assignment.set_edge_location(predecessor, source, EdgeLocation::Register(register));
            }
        }
    }

    Ok(ColoringResult {
        assignment,
        perm_matching,
    })
}

fn required_colors(func: &MFunction) -> Result<Vec<Option<PhysReg>>, ColorError> {
    let mut required = vec![None; func.vregs.count() as usize];
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            if !is_reg_shift(inst) {
                continue;
            }
            for (value, constraint) in inst.uses().into_iter().zip(use_constraints(
                inst,
                func.target_features.variable_shift_encoding(),
            )) {
                let RegConstraint::Fixed(register) = constraint else {
                    continue;
                };
                let Some(slot) = required.get_mut(value.0 as usize) else {
                    return Err(ColorError::at_value(
                        "color.fixed-value-out-of-range",
                        block.id,
                        Some(instruction),
                        value,
                    ));
                };
                if slot.is_some_and(|previous| previous != register) {
                    let mut error = ColorError::at_value(
                        "color.conflicting-fixed-colors",
                        block.id,
                        Some(instruction),
                        value,
                    );
                    error.register = Some(register);
                    return Err(error);
                }
                *slot = Some(register);
            }
        }
    }
    Ok(required)
}

/// Compute list-color exclusions with a single backwards scan.  `live` is the
/// set after the current instruction; `before = after - def + uses`, so fixed
/// unions and clobber intersections need no cloned program-point set.
fn forbidden_colors(
    func: &MFunction,
    analysis: &AnalysisResult,
) -> Result<Vec<ColorMask>, ColorError> {
    let mut forbidden = vec![ColorMask::empty(); func.vregs.count() as usize];
    for (block_index, block) in func.blocks.iter().enumerate() {
        let Some(exit) = analysis.exit_distances.get(block_index) else {
            return Err(ColorError::at_block("color.missing-live-out", block.id));
        };
        let mut live = SmallLiveSet::with_values(exit.keys().copied());
        for (instruction, inst) in block.insts.iter().enumerate().rev() {
            let uses = inst.uses();
            let definition = inst.def();
            if is_reg_shift(inst) {
                for (fixed, constraint) in uses.iter().copied().zip(use_constraints(
                    inst,
                    func.target_features.variable_shift_encoding(),
                )) {
                    let RegConstraint::Fixed(register) = constraint else {
                        continue;
                    };
                    for &value in live.iter().chain(uses.iter()) {
                        if value != fixed {
                            forbid(&mut forbidden, block.id, instruction, value, register)?;
                        }
                    }
                }
            }
            for &register in clobbers(inst) {
                for &value in live.iter() {
                    if Some(value) != definition {
                        forbid(&mut forbidden, block.id, instruction, value, register)?;
                    }
                }
            }
            if let Some(definition) = definition {
                live.remove(definition);
            }
            for value in uses {
                live.insert(value);
            }
        }
    }
    Ok(forbidden)
}

fn forbid(
    forbidden: &mut [ColorMask],
    block: BlockId,
    instruction: usize,
    value: VReg,
    register: PhysReg,
) -> Result<(), ColorError> {
    let Some(mask) = forbidden.get_mut(value.0 as usize) else {
        return Err(ColorError::at_value(
            "color.live-value-out-of-range",
            block,
            Some(instruction),
            value,
        ));
    };
    mask.insert(register);
    Ok(())
}

fn phi_preferences(func: &MFunction, cfg: &NormalizedCfg) -> HashMap<VReg, Vec<VReg>> {
    // CSSA isolates an interfering phi source with an exact snapshot copy:
    //
    //     snapshot = mov source
    //     result   = phi(... snapshot ...)
    //
    // The snapshot is colored after `source`.  At a loop header `result` is
    // colored before either of them, so retaining only the two immediate
    // affinities prevents the header color from ever reaching `source`.
    // Contract that artificial copy node for preference purposes only on an
    // internal edge of the natural loop whose header owns the phi. Ordinary
    // one-shot joins keep their existing choices. This is a soft affinity
    // only: ordinary liveness and forbidden-color checks still reject
    // coalescing whenever `source` and `result` interfere.
    let mut exact_copy_source = vec![None; func.vregs.count() as usize];
    for block in &func.blocks {
        for inst in &block.insts {
            let MInst::Mov { dst, src } = inst else {
                continue;
            };
            if let Some(slot) = exact_copy_source.get_mut(dst.0 as usize) {
                *slot = Some(*src);
            }
        }
    }

    let mut preferences = HashMap::<VReg, Vec<VReg>>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let natural_loop = cfg
            .loop_for_header
            .get(&block_index)
            .and_then(|&loop_index| cfg.loops.get(loop_index));
        for phi in &block.phis {
            for &(predecessor, source) in &phi.sources {
                add_preference(&mut preferences, phi.dst, source);
                let is_repeated_loop_edge = natural_loop.is_some_and(|natural_loop| {
                    cfg.block_index
                        .get(&predecessor)
                        .is_some_and(|predecessor| natural_loop.blocks.contains(predecessor))
                });
                if is_repeated_loop_edge
                    && let Some(original) =
                        exact_copy_source.get(source.0 as usize).copied().flatten()
                {
                    add_preference(&mut preferences, phi.dst, original);
                }
            }
        }
    }
    preferences
}

fn add_preference(preferences: &mut HashMap<VReg, Vec<VReg>>, left: VReg, right: VReg) {
    preferences.entry(left).or_default().push(right);
    preferences.entry(right).or_default().push(left);
}

/// x86 two-address affinity for a definition whose operand dies at the same
/// instruction.  These are preferences only: `choose_color` still requires
/// the color to satisfy every interference, fixed-register, and clobber rule.
fn definition_preferences(inst: &MInst) -> [Option<VReg>; 2] {
    match inst {
        MInst::Mov { src, .. }
        | MInst::AndImm { src, .. }
        | MInst::OrImm { src, .. }
        | MInst::ShrImm { src, .. }
        | MInst::ShlImm { src, .. }
        | MInst::SarImm { src, .. }
        | MInst::AddImm { src, .. }
        | MInst::SubImm { src, .. }
        | MInst::BitNot { src, .. }
        | MInst::Neg { src, .. }
        | MInst::Popcnt { src, .. }
        | MInst::Bsf { src, .. }
        | MInst::Bsr { src, .. }
        | MInst::BsrOr { src, .. } => [Some(*src), None],
        MInst::Add { lhs, rhs, .. }
        | MInst::Mul { lhs, rhs, .. }
        | MInst::And { lhs, rhs, .. }
        | MInst::Or { lhs, rhs, .. }
        | MInst::Xor { lhs, rhs, .. } => [Some(*lhs), Some(*rhs)],
        MInst::Sub { lhs, .. }
        | MInst::Shr { lhs, .. }
        | MInst::Shl { lhs, .. }
        | MInst::Sar { lhs, .. } => [Some(*lhs), None],
        MInst::Select {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::GuardedCmpSelect {
            true_val,
            false_val,
            ..
        } => [Some(*false_val), Some(*true_val)],
        MInst::LoadConstantTableAddr { .. } => [None, None],
        _ => [None, None],
    }
}

#[allow(clippy::too_many_arguments)]
fn choose_color(
    block: BlockId,
    instruction: Option<usize>,
    value: VReg,
    registers: &[PhysReg],
    required: &[Option<PhysReg>],
    forbidden: &[ColorMask],
    instruction_preferences: [Option<VReg>; 2],
    preferences: &HashMap<VReg, Vec<VReg>>,
    colors: &[Option<PhysReg>],
    active: &ActiveColors,
) -> Result<PhysReg, ColorError> {
    let Some(&required) = required.get(value.0 as usize) else {
        return Err(ColorError::at_value(
            "color.value-out-of-range",
            block,
            instruction,
            value,
        ));
    };
    let Some(&forbidden) = forbidden.get(value.0 as usize) else {
        return Err(ColorError::at_value(
            "color.value-out-of-range",
            block,
            instruction,
            value,
        ));
    };
    let available = |register: PhysReg| {
        registers.contains(&register)
            && !forbidden.contains(register)
            && !active.contains(register)
            && required.is_none_or(|required| required == register)
    };
    if let Some(required) = required {
        if available(required) {
            return Ok(required);
        }
    } else {
        for preference in instruction_preferences.into_iter().flatten() {
            if let Some(register) = color_of(colors, preference)
                && available(register)
            {
                return Ok(register);
            }
        }
        if let Some(preferences) = preferences.get(&value) {
            for &preference in preferences {
                if let Some(register) = color_of(colors, preference)
                    && available(register)
                {
                    return Ok(register);
                }
            }
        }
    }
    if let Some(register) = registers
        .iter()
        .copied()
        .find(|register| available(*register))
    {
        return Ok(register);
    }
    Err(ColorError::at_value(
        "color.no-available-register",
        block,
        instruction,
        value,
    ))
}

/// Color the simultaneously-live ordinary phi definitions as one bundle.
///
/// Greedy row-by-row coloring can consume the only incoming source color of a
/// later phi even when the earlier row has an equally good alternative.  The
/// resulting avoidable edge copies are especially expensive when the join
/// dominates a large switch.  With at most the target's physical register
/// count of live rows, an exact subset dynamic program is small and makes the
/// same standard coalescing decision for the complete bundle: maximize the
/// number of already-colored incoming sources which become identity copies.
/// Register order is the deterministic lexicographic tie-breaker.
#[allow(clippy::too_many_arguments)]
fn choose_phi_bundle_colors(
    block: BlockId,
    phis: &[&PhiNode],
    registers: &[PhysReg],
    required: &[Option<PhysReg>],
    forbidden: &[ColorMask],
    preferences: &HashMap<VReg, Vec<VReg>>,
    colors: &[Option<PhysReg>],
    active: &ActiveColors,
) -> Result<Vec<PhysReg>, ColorError> {
    if phis.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::<Vec<(usize, i64)>>::with_capacity(phis.len());
    for phi in phis {
        let Some(&required) = required.get(phi.dst.0 as usize) else {
            return Err(ColorError::at_value(
                "color.value-out-of-range",
                block,
                None,
                phi.dst,
            ));
        };
        let Some(&forbidden) = forbidden.get(phi.dst.0 as usize) else {
            return Err(ColorError::at_value(
                "color.value-out-of-range",
                block,
                None,
                phi.dst,
            ));
        };
        let mut row = Vec::new();
        for (register_index, &register) in registers.iter().enumerate() {
            if active.contains(register)
                || forbidden.contains(register)
                || required.is_some_and(|required| required != register)
            {
                continue;
            }
            let affinity = preferences.get(&phi.dst).map_or(0, |neighbors| {
                neighbors
                    .iter()
                    .filter(|&&neighbor| color_of(colors, neighbor) == Some(register))
                    .count() as i64
            });
            row.push((register_index, affinity));
        }
        if row.is_empty() {
            return Err(ColorError::at_value(
                "color.no-available-register",
                block,
                None,
                phi.dst,
            ));
        }
        candidates.push(row);
    }

    let state_count = 1usize << registers.len();
    let mut memo = vec![i64::MIN; phis.len() * state_count];
    let optimum = best_phi_bundle_score(0, 0, &candidates, state_count, &mut memo);
    if optimum < 0 {
        return Err(ColorError::at_value(
            "color.no-available-register",
            block,
            None,
            phis[0].dst,
        ));
    }

    let mut result = Vec::with_capacity(phis.len());
    let mut row = 0usize;
    let mut used = 0usize;
    let mut remaining = optimum;
    while row < phis.len() {
        let mut selected = None;
        for &(register_index, affinity) in &candidates[row] {
            let bit = 1usize << register_index;
            if used & bit != 0 {
                continue;
            }
            let suffix =
                best_phi_bundle_score(row + 1, used | bit, &candidates, state_count, &mut memo);
            if suffix >= 0 && affinity + suffix == remaining {
                selected = Some((register_index, affinity));
                break;
            }
        }
        let Some((register_index, affinity)) = selected else {
            return Err(ColorError::at_value(
                "color.phi-bundle-reconstruction",
                block,
                None,
                phis[row].dst,
            ));
        };
        result.push(registers[register_index]);
        used |= 1usize << register_index;
        remaining -= affinity;
        row += 1;
    }
    Ok(result)
}

fn best_phi_bundle_score(
    row: usize,
    used: usize,
    candidates: &[Vec<(usize, i64)>],
    state_count: usize,
    memo: &mut [i64],
) -> i64 {
    if row == candidates.len() {
        return 0;
    }
    let slot = row * state_count + used;
    if memo[slot] != i64::MIN {
        return memo[slot];
    }
    let mut best = -1i64;
    for &(register, affinity) in &candidates[row] {
        let bit = 1usize << register;
        if used & bit != 0 {
            continue;
        }
        let suffix = best_phi_bundle_score(row + 1, used | bit, candidates, state_count, memo);
        if suffix >= 0 {
            best = best.max(affinity + suffix);
        }
    }
    memo[slot] = best;
    best
}

fn is_live_after_entry(
    value: VReg,
    last_use: &HashMap<VReg, usize>,
    live_out: &crate::HashMap<VReg, u32>,
) -> bool {
    last_use.contains_key(&value) || live_out.contains_key(&value)
}

fn color_of(colors: &[Option<PhysReg>], value: VReg) -> Option<PhysReg> {
    colors.get(value.0 as usize).copied().flatten()
}

fn set_color(
    colors: &mut [Option<PhysReg>],
    block: BlockId,
    instruction: Option<usize>,
    value: VReg,
    register: PhysReg,
    out_of_range_rule: &'static str,
) -> Result<(), ColorError> {
    let Some(slot) = colors.get_mut(value.0 as usize) else {
        return Err(ColorError::at_value(
            out_of_range_rule,
            block,
            instruction,
            value,
        ));
    };
    if slot.is_some() {
        return Err(ColorError::at_value(
            "color.value-already-colored",
            block,
            instruction,
            value,
        ));
    }
    *slot = Some(register);
    Ok(())
}

#[derive(Default)]
struct ActiveColors {
    owner: [Option<VReg>; PHYSICAL_COLOR_COUNT],
    mask: u16,
}

impl ActiveColors {
    fn contains(&self, register: PhysReg) -> bool {
        self.mask & register_bit(register) != 0
    }

    fn is_empty(&self) -> bool {
        self.mask == 0
    }

    fn add(
        &mut self,
        block: BlockId,
        instruction: Option<usize>,
        value: VReg,
        register: PhysReg,
    ) -> Result<(), ColorError> {
        let color = register_index(register);
        if let Some(previous) = self.owner[color] {
            let mut error = ColorError::at_value(
                "color.simultaneously-live-color-conflict",
                block,
                instruction,
                value,
            );
            error.related = Some(previous);
            error.register = Some(register);
            return Err(error);
        }
        self.owner[color] = Some(value);
        self.mask |= register_bit(register);
        Ok(())
    }

    fn require(
        &self,
        block: BlockId,
        instruction: usize,
        value: VReg,
        register: PhysReg,
    ) -> Result<(), ColorError> {
        if self.owner[register_index(register)] == Some(value) {
            return Ok(());
        }
        let mut error =
            ColorError::at_value("color.use-is-not-live", block, Some(instruction), value);
        error.related = self.owner[register_index(register)];
        error.register = Some(register);
        Err(error)
    }

    fn remove(
        &mut self,
        block: BlockId,
        instruction: usize,
        value: VReg,
        register: PhysReg,
    ) -> Result<(), ColorError> {
        self.require(block, instruction, value, register)?;
        self.owner[register_index(register)] = None;
        self.mask &= !register_bit(register);
        Ok(())
    }
}

#[derive(Default)]
struct SmallLiveSet {
    values: Vec<VReg>,
}

impl SmallLiveSet {
    fn with_values(values: impl IntoIterator<Item = VReg>) -> Self {
        let mut result = Self::default();
        for value in values {
            result.insert(value);
        }
        result
    }

    fn insert(&mut self, value: VReg) -> bool {
        if self.values.contains(&value) {
            false
        } else {
            self.values.push(value);
            true
        }
    }

    fn remove(&mut self, value: VReg) {
        if let Some(index) = self.values.iter().position(|current| *current == value) {
            self.values.swap_remove(index);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &VReg> {
        self.values.iter()
    }
}

fn register_index(register: PhysReg) -> usize {
    register as usize
}

fn register_bit(register: PhysReg) -> u16 {
    1u16 << register_index(register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::features::X86Features;
    use crate::backend::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};

    fn analyze_and_color(func: &mut MFunction) -> Result<ColoringResult, ColorError> {
        let cfg = super::super::cfg::normalize(func).unwrap();
        let analysis = super::super::analysis::analyze(func);
        color_ssa(
            func,
            &cfg,
            &analysis,
            &PermModel::default(),
            super::super::NUM_REGS,
        )
    }

    #[test]
    fn streaming_coloring_reuses_colors_only_after_last_use() {
        let mut vregs = VRegAllocator::new();
        let left = vregs.alloc();
        let right = vregs.alloc();
        let sum = vregs.alloc();
        let later = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: left,
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: right,
            value: 2,
        });
        block.push(MInst::Add {
            dst: sum,
            lhs: left,
            rhs: right,
        });
        block.push(MInst::LoadImm {
            dst: later,
            value: 3,
        });
        block.push(MInst::Add {
            dst: result,
            lhs: sum,
            rhs: later,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let colored = analyze_and_color(&mut func).unwrap();
        assert_ne!(colored.assignment.get(left), colored.assignment.get(right));
        assert_eq!(colored.assignment.get(sum), colored.assignment.get(left));
        assert_ne!(colored.assignment.get(sum), colored.assignment.get(later));
        assert_eq!(colored.assignment.get(result), colored.assignment.get(sum));
    }

    #[test]
    fn instruction_affinity_beats_the_first_free_color() {
        let source = VReg(0);
        let destination = VReg(1);
        let required = vec![None; 2];
        let forbidden = vec![ColorMask::empty(); 2];
        let preferences = HashMap::new();
        let colors = vec![Some(PhysReg::R14), None];

        let color = choose_color(
            BlockId(0),
            Some(0),
            destination,
            ALLOCATABLE_REGS,
            &required,
            &forbidden,
            [Some(source), None],
            &preferences,
            &colors,
            &ActiveColors::default(),
        )
        .unwrap();

        assert_eq!(color, PhysReg::R14);
    }

    #[test]
    fn phi_bundle_matching_avoids_a_greedy_copy() {
        let first_rax = VReg(0);
        let first_rdx = VReg(1);
        let second_rax = VReg(2);
        let first_destination = VReg(3);
        let second_destination = VReg(4);
        let first = PhiNode {
            dst: first_destination,
            sources: vec![(BlockId(0), first_rax), (BlockId(1), first_rdx)],
        };
        let second = PhiNode {
            dst: second_destination,
            sources: vec![(BlockId(0), second_rax)],
        };
        let required = vec![None; 5];
        let forbidden = vec![ColorMask::empty(); 5];
        let preferences = HashMap::from([
            (first_destination, vec![first_rax, first_rdx]),
            (second_destination, vec![second_rax]),
        ]);
        let colors = vec![
            Some(PhysReg::RAX),
            Some(PhysReg::RDX),
            Some(PhysReg::RAX),
            None,
            None,
        ];

        let result = choose_phi_bundle_colors(
            BlockId(2),
            &[&first, &second],
            &[PhysReg::RAX, PhysReg::RDX],
            &required,
            &forbidden,
            &preferences,
            &colors,
            &ActiveColors::default(),
        )
        .unwrap();

        assert_eq!(result, vec![PhysReg::RDX, PhysReg::RAX]);
    }

    #[test]
    fn loop_phi_bundle_sees_through_an_exact_backedge_snapshot() {
        let mut vregs = VRegAllocator::new();
        let initial = vregs.alloc();
        let loop_condition = vregs.alloc();
        let intermediate = vregs.alloc();
        let snapshot = vregs.alloc();
        let outer_destination = vregs.alloc();
        let left_source = vregs.alloc();
        let right_source = vregs.alloc();
        let inner = PhiNode {
            dst: intermediate,
            sources: vec![(BlockId(10), left_source), (BlockId(11), right_source)],
        };
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: initial,
            value: 0,
        });
        entry.push(MInst::LoadImm {
            dst: loop_condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: left_source,
            value: 2,
        });
        entry.push(MInst::LoadImm {
            dst: right_source,
            value: 3,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        func.push_block(entry);

        let mut outer_header = MBlock::new(BlockId(1));
        outer_header.phis.push(PhiNode {
            dst: outer_destination,
            sources: vec![(BlockId(0), initial), (BlockId(2), snapshot)],
        });
        outer_header.push(MInst::Branch {
            cond: loop_condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        func.push_block(outer_header);

        let mut backedge = MBlock::new(BlockId(2));
        backedge.push(MInst::LoadImm {
            dst: intermediate,
            value: 4,
        });
        backedge.push(MInst::Mov {
            dst: snapshot,
            src: intermediate,
        });
        backedge.push(MInst::Jump { target: BlockId(1) });
        func.push_block(backedge);

        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        func.push_block(exit);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let preferences = phi_preferences(&func, &cfg);
        assert!(
            preferences
                .get(&intermediate)
                .is_some_and(|neighbors| neighbors.contains(&outer_destination))
        );

        let required = vec![None; 7];
        let forbidden = vec![ColorMask::empty(); 7];
        let colors = vec![
            None,
            None,
            None,
            None,
            Some(PhysReg::RAX),
            Some(PhysReg::RDX),
            Some(PhysReg::RBX),
        ];
        let result = choose_phi_bundle_colors(
            BlockId(2),
            &[&inner],
            &[PhysReg::RAX, PhysReg::RDX, PhysReg::RBX],
            &required,
            &forbidden,
            &preferences,
            &colors,
            &ActiveColors::default(),
        )
        .unwrap();

        assert_eq!(result, vec![PhysReg::RAX]);
    }

    #[test]
    fn ordinary_join_does_not_contract_an_edge_snapshot() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let original = vregs.alloc();
        let snapshot = vregs.alloc();
        let other = vregs.alloc();
        let destination = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: original,
            value: 2,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        func.push_block(entry);

        let mut true_block = MBlock::new(BlockId(1));
        true_block.push(MInst::Mov {
            dst: snapshot,
            src: original,
        });
        true_block.push(MInst::Jump { target: BlockId(3) });
        func.push_block(true_block);

        let mut false_block = MBlock::new(BlockId(2));
        false_block.push(MInst::LoadImm {
            dst: other,
            value: 3,
        });
        false_block.push(MInst::Jump { target: BlockId(3) });
        func.push_block(false_block);

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: destination,
            sources: vec![(BlockId(1), snapshot), (BlockId(2), other)],
        });
        join.push(MInst::Return);
        func.push_block(join);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let preferences = phi_preferences(&func, &cfg);
        let destination_preferences = preferences.get(&destination).unwrap();
        assert!(destination_preferences.contains(&snapshot));
        assert!(destination_preferences.contains(&other));
        assert!(!destination_preferences.contains(&original));
    }

    #[test]
    fn fixed_use_is_colored_locally_without_global_precoloring() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let amount = vregs.alloc();
        let fixed = vregs.alloc();
        let shifted = vregs.alloc();
        let later = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        func.target_features = X86Features::for_test(false);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Mov {
            dst: fixed,
            src: amount,
        });
        block.push(MInst::Shl {
            dst: shifted,
            lhs,
            rhs: fixed,
        });
        block.push(MInst::Add {
            dst: later,
            lhs,
            rhs: shifted,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let colored = analyze_and_color(&mut func).unwrap();
        assert_eq!(colored.assignment.get(fixed), Some(PhysReg::RCX));
        assert_ne!(colored.assignment.get(lhs), Some(PhysReg::RCX));
    }

    #[test]
    fn perm_rows_are_assigned_by_one_local_perfect_matching() {
        let mut vregs = VRegAllocator::new();
        let left = vregs.alloc();
        let amount = vregs.alloc();
        let shifted = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        func.target_features = X86Features::for_test(false);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: left,
            value: 8,
        });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: shifted,
            lhs: left,
            rhs: amount,
        });
        block.push(MInst::Add {
            dst: result,
            lhs: left,
            rhs: shifted,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let initial_cfg = super::super::cfg::normalize(&mut func).unwrap();
        let (cfg, perms) =
            super::super::legalize::materialize_constraint_perms(&mut func, &initial_cfg).unwrap();
        let analysis = super::super::analysis::analyze(&func);
        let colored = color_ssa(&func, &cfg, &analysis, &perms, super::super::NUM_REGS).unwrap();
        let boundary = &perms.boundaries[0];
        let boundary_block = &func.blocks[cfg.block_index[&boundary.block]];
        let fixed = match boundary_block.insts.first() {
            Some(MInst::Shl { rhs, .. }) => *rhs,
            instruction => panic!("expected constrained shift, got {instruction:?}"),
        };
        let left_after = boundary
            .rows
            .iter()
            .find(|row| row.source == left)
            .unwrap()
            .destination;

        assert_eq!(colored.perm_matching.get(&fixed), Some(&PhysReg::RCX));
        assert_eq!(colored.assignment.get(fixed), Some(PhysReg::RCX));
        assert_eq!(
            colored.assignment.get(left_after),
            colored.assignment.get(left)
        );
    }
}
