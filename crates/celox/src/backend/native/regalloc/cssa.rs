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
    pub(super) fn from_function(func: &MFunction) -> Self {
        let count = func.vregs.count() as usize;
        let mut classes = DisjointSets::new(count);

        for block in &func.blocks {
            for phi in &block.phis {
                assert!(
                    phi.dst.0 < func.vregs.count(),
                    "CSSA phi destination {} is not allocated",
                    phi.dst
                );
                for &(_, source) in &phi.sources {
                    assert!(
                        source.0 < func.vregs.count(),
                        "CSSA phi source {source} is not allocated"
                    );
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

        Self {
            class_for_vreg,
            nontrivial_members,
        }
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

/// A semantic CSSA verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CssaVerifyError {
    pub invariant: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub class: Option<CssaClass>,
    pub values: Option<(VReg, VReg)>,
    pub message: String,
}

impl CssaVerifyError {
    fn function(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            block: None,
            instruction: None,
            class: None,
            values: None,
            message: message.into(),
        }
    }

    fn interference(
        block: BlockId,
        instruction: usize,
        class: CssaClass,
        left: VReg,
        right: VReg,
    ) -> Self {
        Self {
            invariant: "CSSA.CONGRUENCE_MEMBERS_DO_NOT_INTERFERE",
            block: Some(block),
            instruction: Some(instruction),
            class: Some(class),
            values: Some((left, right)),
            message: format!(
                "{left} and {right} are distinct live members of phi congruence class {}",
                class.0
            ),
        }
    }
}

impl fmt::Display for CssaVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "CSSA verify [{}]", self.invariant)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for CssaVerifyError {}

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
pub(super) fn normalize_to_cssa(func: &mut MFunction, cfg: &NormalizedCfg) -> CssaInfo {
    assert_eq!(func.blocks.len(), cfg.predecessors.len());
    assert_eq!(func.blocks.len(), cfg.successors.len());

    let mut edge_copies = vec![Vec::<MInst>::new(); func.blocks.len()];
    for block_index in 0..func.blocks.len() {
        let original_phis = std::mem::take(&mut func.blocks[block_index].phis);
        if original_phis.is_empty() {
            continue;
        }

        let mut rewritten_phis = Vec::with_capacity(original_phis.len());
        let mut entry_copies = Vec::with_capacity(original_phis.len());
        for phi in original_phis {
            let original_destination = phi.dst;
            let fresh_destination = alloc_snapshot(func, original_destination);
            let mut fresh_sources = Vec::with_capacity(phi.sources.len());

            for (predecessor_id, source) in phi.sources {
                let &predecessor = cfg.block_index.get(&predecessor_id).unwrap_or_else(|| {
                    panic!("CSSA phi names missing predecessor {predecessor_id}")
                });
                assert_eq!(
                    cfg.successors[predecessor].as_slice(),
                    [block_index],
                    "CSSA source copy for edge {predecessor_id} -> {} is not edge-local",
                    func.blocks[block_index].id
                );
                let fresh_source = alloc_snapshot(func, source);
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
        let terminator = block
            .insts
            .pop()
            .expect("CSSA edge-copy block has a terminator");
        assert!(
            terminator.is_terminator(),
            "CSSA edge-copy block {} does not end in a terminator",
            block.id
        );
        block.insts.extend(copies);
        block.insts.push(terminator);
    }

    CssaInfo::from_function(func)
}

fn alloc_snapshot(func: &mut MFunction, source: VReg) -> VReg {
    let (vregs, spill_descs, value_widths) = (
        &mut func.vregs,
        &mut func.spill_descs,
        &mut func.value_widths,
    );
    alloc_snapshot_from_parts(vregs, spill_descs, value_widths, source)
}

fn alloc_snapshot_from_parts(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    value_widths: &mut Vec<Option<u8>>,
    source: VReg,
) -> VReg {
    let descriptor = spill_descs
        .get(source.0 as usize)
        .map(SpillDesc::copy_for_snapshot)
        .unwrap_or_else(SpillDesc::transient);
    let width = value_widths.get(source.0 as usize).copied().flatten();
    let fresh = vregs.alloc();
    assert_eq!(fresh.0 as usize, spill_descs.len());
    spill_descs.push(descriptor);
    if !value_widths.is_empty() {
        assert_eq!(fresh.0 as usize, value_widths.len());
        value_widths.push(width);
    }
    fresh
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
) -> Result<(), CssaVerifyError> {
    verify_partition(func, info)?;
    if cfg.predecessors.len() != func.blocks.len()
        || cfg.successors.len() != func.blocks.len()
        || cfg.block_index.len() != func.blocks.len()
    {
        return Err(CssaVerifyError::function(
            "CSSA.CFG_MATCHES_FUNCTION",
            "normalized CFG dimensions do not match the MIR function",
        ));
    }

    let liveness = compute_boundary_liveness(func, cfg, info);
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
        debug_assert_eq!(live.values, liveness.live_in[block_index]);
    }
    Ok(())
}

fn verify_partition(func: &MFunction, info: &CssaInfo) -> Result<(), CssaVerifyError> {
    if info.class_for_vreg.len() != func.vregs.count() as usize {
        return Err(CssaVerifyError::function(
            "CSSA.PARTITION_COVERS_VREGS",
            format!(
                "partition has {} VRegs but function allocated {}",
                info.class_for_vreg.len(),
                func.vregs.count()
            ),
        ));
    }
    let expected = CssaInfo::from_function(func);
    if info.class_for_vreg != expected.class_for_vreg
        || info.nontrivial_members != expected.nontrivial_members
    {
        return Err(CssaVerifyError::function(
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
) -> BoundaryLiveness {
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
                let source = phi
                    .sources
                    .iter()
                    .find_map(|&(source_predecessor, source)| {
                        (source_predecessor == predecessor_id).then_some(source)
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "CSSA liveness: phi {} in {} lacks predecessor {predecessor_id}",
                            phi.dst, func.blocks[successor].id
                        )
                    });
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

    BoundaryLiveness { live_in, live_out }
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

    fn add(
        &mut self,
        value: VReg,
        block: BlockId,
        instruction: usize,
    ) -> Result<(), CssaVerifyError> {
        let class = self.info.class(value);
        if !self.info.is_nontrivial(class) {
            return Ok(());
        }
        if !self.values.insert(value) {
            return Ok(());
        }
        if let Some(&other) = self.member_for_class.get(&class) {
            if other != value {
                return Err(CssaVerifyError::interference(
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
        let cfg = super::super::cfg::normalize(&mut func);
        let old_count = func.vregs.count();

        let info = normalize_to_cssa(&mut func, &cfg);

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
        let cfg = super::super::cfg::normalize(&mut func);
        let info = CssaInfo::from_function(&func);

        verify_cssa(&func, &cfg, &info).unwrap();
    }

    #[test]
    fn verifier_rejects_actual_congruence_interference() {
        let (mut func, left, right, merged) = diamond_with_phi(true);
        let cfg = super::super::cfg::normalize(&mut func);
        func.verify();
        let info = CssaInfo::from_function(&func);

        let error = verify_cssa(&func, &cfg, &info).unwrap_err();

        assert_eq!(error.invariant, "CSSA.CONGRUENCE_MEMBERS_DO_NOT_INTERFERE");
        let (first, second) = error.values.unwrap();
        assert_ne!(first, second);
        assert_eq!(info.class(first), info.class(second));
        assert!(first == left || second == left);
        assert!([left, right, merged].contains(&first));
        assert!([left, right, merged].contains(&second));
    }

    #[test]
    fn method_i_repairs_interfering_phi_congruence() {
        let (mut func, _, _, _) = diamond_with_phi(true);
        let cfg = super::super::cfg::normalize(&mut func);

        let info = normalize_to_cssa(&mut func, &cfg);

        func.verify();
        verify_cssa(&func, &cfg, &info).unwrap();
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

        let info = CssaInfo::from_function(&func);

        assert_eq!(info.class(VReg(0)), CssaClass(0));
        assert_eq!(info.class(VReg(MEMBERS - 1)), CssaClass(0));
        assert_eq!(info.members(CssaClass(0)).count(), MEMBERS as usize);
    }
}
