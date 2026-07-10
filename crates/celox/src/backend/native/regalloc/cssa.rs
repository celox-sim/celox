//! Conventional-SSA construction and its semantic verifier.
//!
//! Braun--Hack spill-home formation coalesces every phi congruence class into
//! one logical value. That is sound only in conventional SSA, where two
//! distinct members of the same class never interfere. [`normalize_to_cssa`]
//! implements Sreedhar Method I. [`verify_cssa`] deliberately checks actual
//! edge-sensitive liveness instead of accepting the Method-I syntax as proof.

use std::collections::VecDeque;
use std::fmt;

use crate::backend::native::mir::{
    BlockId, MFunction, MInst, PhiNode, SpillDesc, VReg, VRegAllocator,
};
use crate::{HashMap, HashSet};

use super::cfg::NormalizedCfg;

/// Stable identifier for one connected component of phi operands/results.
///
/// The smallest VReg number in the component is used as its identifier. This
/// makes rebuilding and comparing the partition deterministic without storing
/// a hash entry or member vector for singleton classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct CssaClass(pub u32);

/// Phi-congruence information after CSSA normalization.
#[derive(Debug, Clone)]
pub(super) struct CssaInfo {
    class_for_vreg: Vec<CssaClass>,
    nontrivial_members: HashMap<CssaClass, Vec<VReg>>,
}

impl CssaInfo {
    /// Build the semantic phi-congruence partition for an existing function.
    ///
    /// This does not claim that the partition is conventional; call
    /// [`verify_cssa`] to prove that property.
    pub(super) fn from_function(func: &MFunction) -> Result<Self, CssaError> {
        let count = func.vregs.count() as usize;
        let mut classes = DisjointSets::new(count);

        for block in &func.blocks {
            for phi in &block.phis {
                if phi.dst.0 >= func.vregs.count() {
                    return Err(CssaError::new(
                        "CSSA.VREG_RANGE",
                        Some(block.id),
                        None,
                        None,
                        vec![phi.dst],
                        "phi destination is outside the allocated VReg range",
                    ));
                }
                for &(_, source) in &phi.sources {
                    if source.0 >= func.vregs.count() {
                        return Err(CssaError::new(
                            "CSSA.VREG_RANGE",
                            Some(block.id),
                            None,
                            None,
                            vec![source],
                            "phi source is outside the allocated VReg range",
                        ));
                    }
                    classes.union(phi.dst.0, source.0);
                }
            }
        }

        let class_for_vreg = (0..count as u32)
            .map(|value| CssaClass(classes.minimum(value)))
            .collect::<Vec<_>>();
        let mut class_sizes = vec![0usize; count];
        for class in &class_for_vreg {
            class_sizes[class.0 as usize] += 1;
        }
        let mut nontrivial_members = HashMap::<CssaClass, Vec<VReg>>::default();
        for (value, &class) in class_for_vreg.iter().enumerate() {
            if class_sizes[class.0 as usize] > 1 {
                nontrivial_members
                    .entry(class)
                    .or_default()
                    .push(VReg(value as u32));
            }
        }

        Ok(Self {
            class_for_vreg,
            nontrivial_members,
        })
    }

    pub(super) fn class(&self, value: VReg) -> CssaClass {
        self.class_for_vreg[value.0 as usize]
    }

    pub(super) fn is_nontrivial(&self, class: CssaClass) -> bool {
        self.nontrivial_members.contains_key(&class)
    }

    pub(super) fn members(&self, class: CssaClass) -> impl Iterator<Item = VReg> + '_ {
        self.nontrivial_members
            .get(&class)
            .into_iter()
            .flatten()
            .copied()
            .chain((!self.is_nontrivial(class)).then_some(VReg(class.0)))
    }

    pub(super) fn nontrivial_classes(&self) -> impl Iterator<Item = CssaClass> + '_ {
        self.nontrivial_members.keys().copied()
    }
}

/// Iterative, size-balanced union--find with a stable minimum-value class id.
///
/// Phi webs can contain hundreds of thousands of values.  Making the smallest
/// VReg the parent (rather than merely the externally visible class id) creates
/// a linear chain for descending unions and makes a recursive `find` overflow
/// the native stack.  Tree shape and stable naming are intentionally separate:
/// union-by-size controls the former, while `minimum` preserves the latter.
struct DisjointSets {
    parent: Vec<u32>,
    size: Vec<u32>,
    minimum: Vec<u32>,
}

impl DisjointSets {
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count as u32).collect(),
            size: vec![1; count],
            minimum: (0..count as u32).collect(),
        }
    }

    fn root(&mut self, value: u32) -> u32 {
        let mut root = value;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }

        let mut current = value;
        while self.parent[current as usize] != current {
            let next = self.parent[current as usize];
            self.parent[current as usize] = root;
            current = next;
        }
        root
    }

    fn union(&mut self, left: u32, right: u32) {
        let mut left = self.root(left);
        let mut right = self.root(right);
        if left == right {
            return;
        }
        if self.size[left as usize] < self.size[right as usize] {
            std::mem::swap(&mut left, &mut right);
        }
        let combined_size = self.size[left as usize] + self.size[right as usize];
        let combined_minimum = self.minimum[left as usize].min(self.minimum[right as usize]);
        self.parent[right as usize] = left;
        self.size[left as usize] = combined_size;
        self.minimum[left as usize] = combined_minimum;
    }

    fn minimum(&mut self, value: u32) -> u32 {
        let root = self.root(value);
        self.minimum[root as usize]
    }
}

/// A CSSA normalization or semantic-verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CssaError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub class: Option<CssaClass>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl CssaError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        class: Option<CssaClass>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            class,
            values,
            message: message.into(),
        }
    }

    fn function(rule: &'static str, message: impl Into<String>) -> Self {
        Self::new(rule, None, None, None, Vec::new(), message)
    }

    fn interference(
        block: BlockId,
        instruction: usize,
        class: CssaClass,
        left: VReg,
        right: VReg,
    ) -> Self {
        Self::new(
            "CSSA.CONGRUENCE_MEMBERS_DO_NOT_INTERFERE",
            Some(block),
            Some(instruction),
            Some(class),
            vec![left, right],
            format!(
                "{left} and {right} are distinct live members of phi congruence class {}",
                class.0
            ),
        )
    }
}

impl fmt::Display for CssaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "CSSA [{}]", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for CssaError {}

/// Rewrite every existing phi using Sreedhar Method I.
///
/// ```text
/// d = phi(pred_i: s_i)
///
/// pred_i: s'_i = mov s_i       (one fresh copy per incoming edge)
/// join:   d'   = phi(s'_i)
///         d    = mov d'         (one fresh phi result)
/// ```
///
/// CFG normalization guarantees that each incoming predecessor is specific to
/// this edge. The transformation changes neither blocks nor edges, so `cfg`
/// remains valid afterwards.
pub(super) fn normalize_to_cssa(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
) -> Result<CssaInfo, CssaError> {
    if func.blocks.len() != cfg.predecessors.len()
        || func.blocks.len() != cfg.successors.len()
        || func.blocks.len() != cfg.block_index.len()
    {
        return Err(CssaError::function(
            "CSSA.CFG_MATCHES_FUNCTION",
            "normalized CFG dimensions do not match the MIR function",
        ));
    }

    let mut edge_copies = vec![Vec::<MInst>::new(); func.blocks.len()];
    for block_index in 0..func.blocks.len() {
        let block_id = func.blocks[block_index].id;
        let original_phis = std::mem::take(&mut func.blocks[block_index].phis);
        if original_phis.is_empty() {
            continue;
        }

        let mut rewritten_phis = Vec::with_capacity(original_phis.len());
        let mut entry_copies = Vec::with_capacity(original_phis.len());
        for phi in original_phis {
            let original_destination = phi.dst;
            let fresh_destination = alloc_snapshot(func, original_destination, block_id)?;
            let mut fresh_sources = Vec::with_capacity(phi.sources.len());

            for (predecessor_id, source) in phi.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                    return Err(CssaError::new(
                        "CSSA.PHI_PREDECESSOR_EXISTS",
                        Some(block_id),
                        None,
                        None,
                        vec![original_destination, source],
                        format!("phi names missing predecessor {predecessor_id}"),
                    ));
                };
                if cfg.successors.get(predecessor).map(Vec::as_slice) != Some(&[block_index]) {
                    return Err(CssaError::new(
                        "CSSA.EDGE_COPY_ISOLATED",
                        Some(block_id),
                        None,
                        None,
                        vec![original_destination, source],
                        format!(
                            "source copy edge {predecessor_id} -> {block_id} is not a dedicated edge block"
                        ),
                    ));
                }
                let fresh_source = alloc_snapshot(func, source, block_id)?;
                edge_copies[predecessor].push(MInst::Mov {
                    dst: fresh_source,
                    src: source,
                });
                fresh_sources.push((predecessor_id, fresh_source));
            }

            rewritten_phis.push(PhiNode {
                dst: fresh_destination,
                sources: fresh_sources,
            });
            entry_copies.push(MInst::Mov {
                dst: original_destination,
                src: fresh_destination,
            });
        }

        func.blocks[block_index].phis = rewritten_phis;
        func.blocks[block_index].insts.splice(0..0, entry_copies);
    }

    for (block, copies) in func.blocks.iter_mut().zip(edge_copies) {
        if copies.is_empty() {
            continue;
        }
        let Some(terminator) = block.insts.pop() else {
            return Err(CssaError::new(
                "CSSA.EDGE_BLOCK_TERMINATED",
                Some(block.id),
                None,
                None,
                Vec::new(),
                "edge-copy block has no terminator",
            ));
        };
        if !terminator.is_terminator() {
            return Err(CssaError::new(
                "CSSA.EDGE_BLOCK_TERMINATED",
                Some(block.id),
                None,
                None,
                Vec::new(),
                "edge-copy block does not end in a terminator",
            ));
        }
        block.insts.extend(copies);
        block.insts.push(terminator);
    }

    CssaInfo::from_function(func)
}

fn alloc_snapshot(func: &mut MFunction, source: VReg, block: BlockId) -> Result<VReg, CssaError> {
    let (vregs, spill_descs, value_widths) = (
        &mut func.vregs,
        &mut func.spill_descs,
        &mut func.value_widths,
    );
    alloc_snapshot_from_parts(vregs, spill_descs, value_widths, source, block)
}

fn alloc_snapshot_from_parts(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    value_widths: &mut Vec<Option<u8>>,
    source: VReg,
    block: BlockId,
) -> Result<VReg, CssaError> {
    let descriptor = spill_descs
        .get(source.0 as usize)
        .map(SpillDesc::copy_for_snapshot)
        .unwrap_or_else(SpillDesc::transient);
    let width = value_widths.get(source.0 as usize).copied().flatten();
    let fresh = vregs.try_alloc().map_err(|error| {
        CssaError::new(
            "CSSA.VREG_EXHAUSTED",
            Some(block),
            None,
            None,
            vec![source],
            error.to_string(),
        )
    })?;
    if fresh.0 as usize != spill_descs.len()
        || (!value_widths.is_empty() && fresh.0 as usize != value_widths.len())
    {
        return Err(CssaError::new(
            "CSSA.SIDETABLE_APPEND_POSITION",
            Some(block),
            None,
            None,
            vec![fresh, source],
            "fresh CSSA VReg does not append consistently to MIR side tables",
        ));
    }
    spill_descs.push(descriptor);
    if !value_widths.is_empty() {
        value_widths.push(width);
    }
    Ok(fresh)
}

/// Prove that no distinct members of a phi-congruence class interfere.
///
/// The fixed point stores liveness only at block boundaries. Verification of
/// instruction points is streaming: each block is scanned backwards once,
/// maintaining at most one live member per nontrivial congruence class. Phi
/// sources are edge uses and are never merged into the successor's live-in
/// set, which is essential for avoiding false interference at joins.
pub(super) fn verify_cssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
    info: &CssaInfo,
) -> Result<(), CssaError> {
    verify_partition(func, info)?;
    if cfg.predecessors.len() != func.blocks.len()
        || cfg.successors.len() != func.blocks.len()
        || cfg.block_index.len() != func.blocks.len()
    {
        return Err(CssaError::function(
            "CSSA.CFG_MATCHES_FUNCTION",
            "normalized CFG dimensions do not match the MIR function",
        ));
    }

    let liveness = compute_boundary_liveness(func, cfg, info)?;
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut live = LiveClasses::new(info);
        for &value in &liveness.live_out[block_index] {
            live.add(value, block.id, block.insts.len())?;
        }

        for (instruction, inst) in block.insts.iter().enumerate().rev() {
            if let Some(definition) = inst.def() {
                live.remove(definition);
            }
            for value in inst.uses() {
                live.add(value, block.id, instruction)?;
            }
        }

        // Phi definitions happen simultaneously at block entry. They have
        // already entered `live` through their dominated uses; removing every
        // destination together leaves precisely the ordinary live-in set.
        for phi in &block.phis {
            live.remove(phi.dst);
        }
        if live.values != liveness.live_in[block_index] {
            let mut mismatched_values = live
                .values
                .symmetric_difference(&liveness.live_in[block_index])
                .copied()
                .collect::<Vec<_>>();
            mismatched_values.sort_unstable();
            return Err(CssaError::new(
                "CSSA.LIVENESS_ENTRY_MATCH",
                Some(block.id),
                Some(0),
                None,
                mismatched_values,
                "streaming instruction liveness disagrees with boundary liveness",
            ));
        }
    }
    Ok(())
}

fn verify_partition(func: &MFunction, info: &CssaInfo) -> Result<(), CssaError> {
    if info.class_for_vreg.len() != func.vregs.count() as usize {
        return Err(CssaError::function(
            "CSSA.PARTITION_COVERS_VREGS",
            format!(
                "partition has {} VRegs but function allocated {}",
                info.class_for_vreg.len(),
                func.vregs.count()
            ),
        ));
    }
    let expected = CssaInfo::from_function(func)?;
    if info.class_for_vreg != expected.class_for_vreg
        || info.nontrivial_members != expected.nontrivial_members
    {
        return Err(CssaError::function(
            "CSSA.PARTITION_MATCHES_PHIS",
            "congruence partition does not match current phi operands and results",
        ));
    }
    Ok(())
}

struct BoundaryLiveness {
    live_in: Vec<HashSet<VReg>>,
    live_out: Vec<HashSet<VReg>>,
}

fn compute_boundary_liveness(
    func: &MFunction,
    cfg: &NormalizedCfg,
    info: &CssaInfo,
) -> Result<BoundaryLiveness, CssaError> {
    let blocks = func.blocks.len();
    let mut definitions = vec![HashSet::<VReg>::default(); blocks];
    let mut upward_uses = vec![HashSet::<VReg>::default(); blocks];
    for (block_index, block) in func.blocks.iter().enumerate() {
        definitions[block_index].extend(
            block
                .phis
                .iter()
                .map(|phi| phi.dst)
                .filter(|value| info.is_nontrivial(info.class(*value))),
        );
        for inst in &block.insts {
            for value in inst.uses() {
                if info.is_nontrivial(info.class(value))
                    && !definitions[block_index].contains(&value)
                {
                    upward_uses[block_index].insert(value);
                }
            }
            if let Some(definition) = inst
                .def()
                .filter(|value| info.is_nontrivial(info.class(*value)))
            {
                definitions[block_index].insert(definition);
            }
        }
    }

    let mut phi_edge_uses = cfg
        .successors
        .iter()
        .map(|successors| vec![HashSet::<VReg>::default(); successors.len()])
        .collect::<Vec<_>>();
    for (predecessor, successors) in cfg.successors.iter().enumerate() {
        let predecessor_id = func.blocks[predecessor].id;
        for (edge, &successor) in successors.iter().enumerate() {
            for phi in &func.blocks[successor].phis {
                let Some(source) = phi
                    .sources
                    .iter()
                    .find_map(|&(source_predecessor, source)| {
                        (source_predecessor == predecessor_id).then_some(source)
                    })
                else {
                    return Err(CssaError::new(
                        "CSSA.PHI_EDGE_COVERAGE",
                        Some(func.blocks[successor].id),
                        None,
                        None,
                        vec![phi.dst],
                        format!("phi lacks source for predecessor {predecessor_id}"),
                    ));
                };
                phi_edge_uses[predecessor][edge].insert(source);
            }
        }
    }

    let mut live_in = vec![HashSet::<VReg>::default(); blocks];
    let mut live_out = vec![HashSet::<VReg>::default(); blocks];
    let mut worklist = (0..blocks).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; blocks];
    while let Some(block) = worklist.pop_front() {
        queued[block] = false;
        let mut next_out = HashSet::<VReg>::default();
        for (edge, &successor) in cfg.successors[block].iter().enumerate() {
            next_out.extend(live_in[successor].iter().copied());
            next_out.extend(phi_edge_uses[block][edge].iter().copied());
        }
        let mut next_in = upward_uses[block].clone();
        next_in.extend(
            next_out
                .iter()
                .filter(|value| !definitions[block].contains(value))
                .copied(),
        );

        let entry_changed = next_in != live_in[block];
        live_out[block] = next_out;
        if entry_changed {
            live_in[block] = next_in;
            for &predecessor in &cfg.predecessors[block] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    worklist.push_back(predecessor);
                }
            }
        }
    }

    Ok(BoundaryLiveness { live_in, live_out })
}

struct LiveClasses<'a> {
    info: &'a CssaInfo,
    values: HashSet<VReg>,
    member_for_class: HashMap<CssaClass, VReg>,
}

impl<'a> LiveClasses<'a> {
    fn new(info: &'a CssaInfo) -> Self {
        Self {
            info,
            values: HashSet::default(),
            member_for_class: HashMap::default(),
        }
    }

    fn add(&mut self, value: VReg, block: BlockId, instruction: usize) -> Result<(), CssaError> {
        let class = self.info.class(value);
        if !self.info.is_nontrivial(class) {
            return Ok(());
        }
        if !self.values.insert(value) {
            return Ok(());
        }
        if let Some(&other) = self.member_for_class.get(&class) {
            if other != value {
                return Err(CssaError::interference(
                    block,
                    instruction,
                    class,
                    other,
                    value,
                ));
            }
        } else {
            self.member_for_class.insert(class, value);
        }
        Ok(())
    }

    fn remove(&mut self, value: VReg) {
        let class = self.info.class(value);
        if !self.info.is_nontrivial(class) {
            return;
        }
        if !self.values.remove(&value) {
            return;
        }
        if self.member_for_class.get(&class) == Some(&value) {
            self.member_for_class.remove(&class);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, SpillDesc};

    fn diamond_with_phi(use_source_after_join: bool) -> (MFunction, VReg, VReg, VReg) {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let left = vregs.alloc();
        let right = vregs.alloc();
        let merged = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, (0..5).map(|_| SpillDesc::transient()).collect());

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        // Defining this source above the branch makes it a legal dominated use
        // after the join in the intentionally non-conventional test case.
        entry.push(MInst::LoadImm {
            dst: left,
            value: 10,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left_block = MBlock::new(BlockId(1));
        left_block.push(MInst::Jump { target: BlockId(3) });

        let mut right_block = MBlock::new(BlockId(2));
        right_block.push(MInst::LoadImm {
            dst: right,
            value: 20,
        });
        right_block.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), left), (BlockId(2), right)],
        });
        join.push(MInst::Mov {
            dst: result,
            src: merged,
        });
        if use_source_after_join {
            // Keep both the phi result and one incoming member live at the
            // same instruction point.
            join.insts[0] = MInst::Add {
                dst: result,
                lhs: merged,
                rhs: left,
            };
        }
        join.push(MInst::Return);
        func.blocks = vec![entry, left_block, right_block, join];
        (func, left, right, merged)
    }

    #[test]
    fn method_i_inserts_fresh_edge_and_entry_copies() {
        let (mut func, left, right, original_destination) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let old_count = func.vregs.count();

        let info = normalize_to_cssa(&mut func, &cfg).unwrap();

        assert_eq!(func.vregs.count(), old_count + 3);
        let join = &func.blocks[cfg.block_index[&BlockId(3)]];
        let phi = &join.phis[0];
        assert_ne!(phi.dst, original_destination);
        assert!(matches!(
            join.insts.first(),
            Some(MInst::Mov { dst, src })
                if *dst == original_destination && *src == phi.dst
        ));
        for (predecessor, source_copy) in &phi.sources {
            let predecessor = &func.blocks[cfg.block_index[predecessor]];
            assert!(matches!(
                predecessor.insts.as_slice(),
                [.., MInst::Mov { dst, src }, MInst::Jump { target: BlockId(3) }]
                    if dst == source_copy && (*src == left || *src == right)
            ));
        }
        let members = info.members(info.class(phi.dst)).collect::<HashSet<_>>();
        assert_eq!(info.nontrivial_classes().count(), 1);
        assert_eq!(
            members,
            phi.sources
                .iter()
                .map(|(_, source)| *source)
                .chain(std::iter::once(phi.dst))
                .collect()
        );
        assert!(!members.contains(&left));
        assert!(!members.contains(&right));
        assert!(!members.contains(&original_destination));
        func.verify();
        verify_cssa(&func, &cfg, &info).unwrap();
    }

    #[test]
    fn verifier_accepts_disjoint_phi_edge_live_ranges() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let info = CssaInfo::from_function(&func).unwrap();

        verify_cssa(&func, &cfg, &info).unwrap();
    }

    #[test]
    fn verifier_rejects_actual_congruence_interference() {
        let (mut func, left, right, merged) = diamond_with_phi(true);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        func.verify();
        let info = CssaInfo::from_function(&func).unwrap();

        let error = verify_cssa(&func, &cfg, &info).unwrap_err();

        assert_eq!(error.rule, "CSSA.CONGRUENCE_MEMBERS_DO_NOT_INTERFERE");
        let [first, second] = error.values.as_slice() else {
            panic!("interference error must identify two values")
        };
        let (first, second) = (*first, *second);
        assert_ne!(first, second);
        assert_eq!(info.class(first), info.class(second));
        assert!(first == left || second == left);
        assert!([left, right, merged].contains(&first));
        assert!([left, right, merged].contains(&second));
    }

    #[test]
    fn method_i_repairs_interfering_phi_congruence() {
        let (mut func, _, _, _) = diamond_with_phi(true);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();

        let info = normalize_to_cssa(&mut func, &cfg).unwrap();

        func.verify();
        verify_cssa(&func, &cfg, &info).unwrap();
    }

    #[test]
    fn normalization_reports_vreg_exhaustion_without_panicking() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        func.vregs.set_next_for_test(u32::MAX);

        let error = normalize_to_cssa(&mut func, &cfg).unwrap_err();

        assert_eq!(error.rule, "CSSA.VREG_EXHAUSTED");
        assert_eq!(func.vregs.count(), u32::MAX);
    }

    #[test]
    fn normalization_reports_side_table_misalignment() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        func.spill_descs.pop();

        let error = normalize_to_cssa(&mut func, &cfg).unwrap_err();

        assert_eq!(error.rule, "CSSA.SIDETABLE_APPEND_POSITION");
    }

    #[test]
    fn normalization_reports_missing_edge_block_terminator() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let join = cfg.block_index[&BlockId(3)];
        let predecessor = cfg.block_index[&func.blocks[join].phis[0].sources[0].0];
        func.blocks[predecessor].insts.clear();

        let error = normalize_to_cssa(&mut func, &cfg).unwrap_err();

        assert_eq!(error.rule, "CSSA.EDGE_BLOCK_TERMINATED");
        assert_eq!(error.block, Some(func.blocks[predecessor].id));
    }

    #[test]
    fn liveness_reports_phi_missing_predecessor_source() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let join = cfg.block_index[&BlockId(3)];
        func.blocks[join].phis[0].sources.pop();
        let info = CssaInfo::from_function(&func).unwrap();

        let error = verify_cssa(&func, &cfg, &info).unwrap_err();

        assert_eq!(error.rule, "CSSA.PHI_EDGE_COVERAGE");
        assert_eq!(error.block, Some(BlockId(3)));
    }

    #[test]
    fn partition_reports_out_of_range_phi_member() {
        let (mut func, _, _, _) = diamond_with_phi(false);
        let invalid = VReg(func.vregs.count());
        func.blocks[3].phis[0].sources[0].1 = invalid;

        let error = CssaInfo::from_function(&func).unwrap_err();

        assert_eq!(error.rule, "CSSA.VREG_RANGE");
        assert_eq!(error.values, vec![invalid]);
    }

    #[test]
    fn descending_large_phi_web_has_stable_minimum_without_recursion() {
        const MEMBERS: u32 = 50_000;

        let mut vregs = VRegAllocator::new();
        for _ in 0..MEMBERS {
            vregs.alloc();
        }
        let mut func = MFunction::new(vregs, Vec::new());
        let mut block = MBlock::new(BlockId(0));
        // With "minimum VReg is the parent" unioning, this exact order builds
        // v49999 -> v49998 -> ... -> v0 and the final class enumeration makes
        // a recursive find overflow.  Stable naming must not dictate tree shape.
        for destination in (1..MEMBERS).rev() {
            block.phis.push(PhiNode {
                dst: VReg(destination),
                sources: vec![(BlockId(0), VReg(destination - 1))],
            });
        }
        func.blocks.push(block);

        let info = CssaInfo::from_function(&func).unwrap();

        assert_eq!(info.class(VReg(0)), CssaClass(0));
        assert_eq!(info.class(VReg(MEMBERS - 1)), CssaClass(0));
        assert_eq!(info.members(CssaClass(0)).count(), MEMBERS as usize);
    }
}
