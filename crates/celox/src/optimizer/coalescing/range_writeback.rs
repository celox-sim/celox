//! Allocation-facing residency and lazy-writeback graph for range StateSSA.
//!
//! A range version is not promoted globally. Each concrete load fragment is
//! an optional packed-state use, while terminal observations are mandatory
//! packed-state uses. The allocator may group any subset of one version's
//! uses and request one writeback at their latest shared dominator. Splitting
//! the subset produces independent path/use clusters instead of one long
//! whole-function live range.

use super::range_state_ssa::{RangeAtomId, RangeStateSsa, RangeVersionId, RangeVersionKind};
use crate::ir::cfg::SirCfg;
use crate::ir::{BlockId, RegisterId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeHomeUseId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeInheritanceId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum RangeHomeUseKind {
    Load {
        block: BlockId,
        instruction: usize,
        destination: RegisterId,
        destination_bit_offset: usize,
    },
    /// Packed state is externally visible when this execution unit exits.
    Boundary { block: BlockId },
}

impl RangeHomeUseKind {
    fn block(self) -> BlockId {
        match self {
            Self::Load { block, .. } | Self::Boundary { block } => block,
        }
    }

    pub fn packed_state_is_required(self) -> bool {
        matches!(self, Self::Boundary { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeHomeUse {
    pub id: RangeHomeUseId,
    pub atom: RangeAtomId,
    pub version: RangeVersionId,
    pub kind: RangeHomeUseKind,
}

/// One incoming edge needed to prove that a phi SCC already resides in packed
/// state. Incoming edges within the same SCC are omitted: they preserve the
/// established packed value around a loop and are not independent writebacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeIncomingHome {
    pub phi: RangeVersionId,
    pub predecessor: BlockId,
    pub version: RangeVersionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeInheritanceComponent {
    pub id: RangeInheritanceId,
    pub atom: RangeAtomId,
    pub versions: Vec<RangeVersionId>,
    pub incoming: Vec<RangeIncomingHome>,
    pub cyclic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum RangePackedAlternative {
    /// The value has not been changed since function entry.
    Preexisting,
    /// Create a packed home for one allocator-selected use cluster.
    DeferredWriteback,
    /// Every external incoming edge of this phi SCC already has a packed home.
    Inherited(RangeInheritanceId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeVersionResidency {
    pub version: RangeVersionId,
    pub atom: RangeAtomId,
    pub uses: Vec<RangeHomeUseId>,
    pub packed_alternatives: Vec<RangePackedAlternative>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum RangeWritebackPlacement {
    /// Immediately before an instruction in the shared dominator block.
    BeforeInstruction { block: BlockId, instruction: usize },
    /// After the block's ordinary instructions and before its terminator.
    BlockExit { block: BlockId },
}

impl RangeWritebackPlacement {
    fn block(self) -> BlockId {
        match self {
            Self::BeforeInstruction { block, .. } | Self::BlockExit { block } => block,
        }
    }
}

/// One optional packed home creation. Its use set is a real allocation split:
/// another disjoint cluster of the same version may choose register, stack,
/// inherited state, or a separate path-local writeback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeWritebackCluster {
    pub version: RangeVersionId,
    pub atom: RangeAtomId,
    pub uses: Vec<RangeHomeUseId>,
    pub placement: RangeWritebackPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeResidencyPlan {
    pub versions: Vec<RangeVersionResidency>,
    pub uses: Vec<RangeHomeUse>,
    pub inheritance: Vec<RangeInheritanceComponent>,
}

impl RangeResidencyPlan {
    /// Build an allocation-facing graph without rewriting executable SIR.
    ///
    /// Let V be range versions, U actual load/boundary uses, and E phi inputs.
    /// Construction and storage are O(V + U + E); there is no use-by-version,
    /// atom-by-block, or candidate-cluster power set.
    pub fn analyze(state: &RangeStateSsa, cfg: &SirCfg) -> Result<Self, &'static str> {
        state.verify(cfg)?;
        let (inheritance, component_for_version) = inheritance_components(state)?;

        let mut raw_uses = Vec::<(RangeAtomId, RangeVersionId, RangeHomeUseKind)>::new();
        for load in &state.loads {
            for part in &load.parts {
                raw_uses.push((
                    part.atom,
                    part.reaching,
                    RangeHomeUseKind::Load {
                        block: load.block,
                        instruction: load.instruction,
                        destination: load.destination,
                        destination_bit_offset: part.destination_bit_offset,
                    },
                ));
            }
        }
        for boundary in &state.boundaries {
            for part in &boundary.parts {
                raw_uses.push((
                    part.atom,
                    part.reaching,
                    RangeHomeUseKind::Boundary {
                        block: boundary.block,
                    },
                ));
            }
        }
        let mut versions = state
            .versions
            .iter()
            .map(|version| {
                let mut packed_alternatives = match version.kind {
                    RangeVersionKind::LiveOnEntry => vec![RangePackedAlternative::Preexisting],
                    RangeVersionKind::Store { .. } => {
                        vec![RangePackedAlternative::DeferredWriteback]
                    }
                    RangeVersionKind::Phi { .. } => vec![
                        RangePackedAlternative::DeferredWriteback,
                        RangePackedAlternative::Inherited(
                            component_for_version[version.id.0]
                                .ok_or("range phi has no inheritance component")?,
                        ),
                    ],
                };
                packed_alternatives.sort_unstable();
                Ok(RangeVersionResidency {
                    version: version.id,
                    atom: version.atom,
                    uses: Vec::new(),
                    packed_alternatives,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        let mut uses = Vec::with_capacity(raw_uses.len());
        for (atom, version, kind) in raw_uses {
            let id = RangeHomeUseId(uses.len());
            let Some(row) = versions.get_mut(version.0) else {
                return Err("range home use names an absent version");
            };
            if row.atom != atom {
                return Err("range home use and version name different atoms");
            }
            row.uses.push(id);
            uses.push(RangeHomeUse {
                id,
                atom,
                version,
                kind,
            });
        }

        let result = Self {
            versions,
            uses,
            inheritance,
        };
        result.verify(state, cfg)?;
        Ok(result)
    }

    /// Return one latest shared writeback point for an allocator-selected use
    /// cluster. The allocator controls clustering; this routine never merges
    /// all uses of a version by default.
    pub fn writeback_cluster(
        &self,
        state: &RangeStateSsa,
        cfg: &SirCfg,
        version: RangeVersionId,
        selected: &[RangeHomeUseId],
    ) -> Result<RangeWritebackCluster, &'static str> {
        let Some(version_row) = self.versions.get(version.0) else {
            return Err("writeback version is outside the residency graph");
        };
        if version_row.version != version {
            return Err("writeback version identity differs from its dense row");
        }
        if matches!(
            state.versions.get(version.0).map(|version| &version.kind),
            Some(RangeVersionKind::LiveOnEntry)
        ) {
            return Err("live-on-entry state does not need a writeback");
        }
        let mut uses = selected.to_vec();
        uses.sort_unstable();
        if uses.is_empty() || uses.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("writeback cluster is empty or contains duplicate uses");
        }
        let mut common = None::<usize>;
        for &id in &uses {
            let Some(use_) = self.uses.get(id.0) else {
                return Err("writeback cluster use is outside the residency graph");
            };
            if use_.id != id || use_.version != version {
                return Err("writeback cluster mixes use identities or versions");
            }
            let block = cfg
                .block_index(use_.kind.block())
                .ok_or("writeback use block is outside the CFG")?;
            common = Some(match common {
                None => block,
                Some(current) => cfg
                    .dominators
                    .lca(current, block)
                    .ok_or("writeback uses have no common dominator")?,
            });
        }
        let common = common.ok_or("writeback cluster has no use block")?;
        let common_id = cfg.block_ids[common];
        let mut earliest_instruction = None::<usize>;
        for &id in &uses {
            match self.uses[id.0].kind {
                RangeHomeUseKind::Load {
                    block, instruction, ..
                } if block == common_id => {
                    earliest_instruction = Some(
                        earliest_instruction
                            .map_or(instruction, |current| current.min(instruction)),
                    );
                }
                _ => {}
            }
        }
        let placement = earliest_instruction.map_or(
            RangeWritebackPlacement::BlockExit { block: common_id },
            |instruction| RangeWritebackPlacement::BeforeInstruction {
                block: common_id,
                instruction,
            },
        );
        let cluster = RangeWritebackCluster {
            version,
            atom: version_row.atom,
            uses,
            placement,
        };
        self.verify_cluster(state, cfg, &cluster)?;
        Ok(cluster)
    }

    pub fn verify(&self, state: &RangeStateSsa, cfg: &SirCfg) -> Result<(), &'static str> {
        if self.versions.len() != state.versions.len() {
            return Err("range residency rows do not cover every version");
        }
        let mut actual_uses = vec![Vec::<RangeHomeUseId>::new(); self.versions.len()];
        let mut expected = Vec::new();
        for load in &state.loads {
            for part in &load.parts {
                expected.push((
                    part.atom,
                    part.reaching,
                    RangeHomeUseKind::Load {
                        block: load.block,
                        instruction: load.instruction,
                        destination: load.destination,
                        destination_bit_offset: part.destination_bit_offset,
                    },
                ));
            }
        }
        for boundary in &state.boundaries {
            for part in &boundary.parts {
                expected.push((
                    part.atom,
                    part.reaching,
                    RangeHomeUseKind::Boundary {
                        block: boundary.block,
                    },
                ));
            }
        }
        if self.uses.len() != expected.len() {
            return Err("range residency use count differs from StateSSA");
        }
        for (row, (use_, expected)) in self.uses.iter().zip(&expected).enumerate() {
            if use_.id != RangeHomeUseId(row)
                || use_.version.0 >= self.versions.len()
                || self.versions[use_.version.0].atom != use_.atom
                || cfg.block_index(use_.kind.block()).is_none()
                || (use_.atom, use_.version, use_.kind) != *expected
            {
                return Err("range residency use identity or coverage is invalid");
            }
            actual_uses[use_.version.0].push(use_.id);
        }

        let (expected_inheritance, component_for_version) = inheritance_components(state)?;
        if self.inheritance != expected_inheritance {
            return Err("range phi inheritance components are stale or invalid");
        }
        for (row, version) in self.versions.iter().enumerate() {
            let state_version = &state.versions[row];
            if version.version != RangeVersionId(row)
                || state_version.id != version.version
                || state_version.atom != version.atom
                || version.uses != actual_uses[row]
            {
                return Err("range residency version row is inconsistent");
            }
            let expected_alternatives = match state_version.kind {
                RangeVersionKind::LiveOnEntry => vec![RangePackedAlternative::Preexisting],
                RangeVersionKind::Store { .. } => {
                    vec![RangePackedAlternative::DeferredWriteback]
                }
                RangeVersionKind::Phi { .. } => vec![
                    RangePackedAlternative::DeferredWriteback,
                    RangePackedAlternative::Inherited(
                        component_for_version[row]
                            .ok_or("range phi lacks an inheritance component")?,
                    ),
                ],
            };
            if version.packed_alternatives != expected_alternatives {
                return Err("range residency packed alternatives are inconsistent");
            }
        }
        Ok(())
    }

    pub fn verify_cluster(
        &self,
        state: &RangeStateSsa,
        cfg: &SirCfg,
        cluster: &RangeWritebackCluster,
    ) -> Result<(), &'static str> {
        let Some(version) = state.versions.get(cluster.version.0) else {
            return Err("writeback cluster version is absent");
        };
        if version.id != cluster.version
            || version.atom != cluster.atom
            || matches!(version.kind, RangeVersionKind::LiveOnEntry)
            || cluster.uses.is_empty()
            || cluster.uses.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("writeback cluster identity or use ordering is invalid");
        }
        let placement_block = cfg
            .block_index(cluster.placement.block())
            .ok_or("writeback placement block is outside the CFG")?;
        let (definition_block, definition_instruction) = match version.kind {
            RangeVersionKind::LiveOnEntry => unreachable!("rejected above"),
            RangeVersionKind::Store {
                block, instruction, ..
            } => (
                cfg.block_index(block)
                    .ok_or("writeback definition block is outside the CFG")?,
                Some(instruction),
            ),
            RangeVersionKind::Phi { block, .. } => (
                cfg.block_index(block)
                    .ok_or("writeback phi block is outside the CFG")?,
                None,
            ),
        };
        if !cfg.dominators.dominates(definition_block, placement_block) {
            return Err("writeback placement is not dominated by its definition");
        }
        if definition_block == placement_block
            && let (
                Some(definition),
                RangeWritebackPlacement::BeforeInstruction { instruction, .. },
            ) = (definition_instruction, cluster.placement)
            && instruction <= definition
        {
            return Err("writeback placement precedes its store-version definition");
        }

        for &id in &cluster.uses {
            let Some(use_) = self.uses.get(id.0) else {
                return Err("writeback cluster contains an absent use");
            };
            if use_.id != id || use_.version != cluster.version || use_.atom != cluster.atom {
                return Err("writeback cluster contains a use of another range version");
            }
            let use_block = cfg
                .block_index(use_.kind.block())
                .ok_or("writeback cluster use block is outside the CFG")?;
            if !cfg.dominators.dominates(placement_block, use_block) {
                return Err("writeback placement does not dominate every selected use");
            }
            if placement_block == use_block {
                match (cluster.placement, use_.kind) {
                    (
                        RangeWritebackPlacement::BeforeInstruction {
                            instruction: placement,
                            ..
                        },
                        RangeHomeUseKind::Load {
                            instruction: use_instruction,
                            ..
                        },
                    ) if placement <= use_instruction => {}
                    (
                        RangeWritebackPlacement::BeforeInstruction { .. },
                        RangeHomeUseKind::Boundary { .. },
                    )
                    | (
                        RangeWritebackPlacement::BlockExit { .. },
                        RangeHomeUseKind::Boundary { .. },
                    ) => {}
                    (RangeWritebackPlacement::BlockExit { .. }, RangeHomeUseKind::Load { .. }) => {
                        return Err(
                            "block-exit writeback follows a selected load in the same block",
                        );
                    }
                    _ => {
                        return Err("writeback placement follows a selected use in the same block");
                    }
                }
            }
        }
        Ok(())
    }
}

fn inheritance_components(
    state: &RangeStateSsa,
) -> Result<
    (
        Vec<RangeInheritanceComponent>,
        Vec<Option<RangeInheritanceId>>,
    ),
    &'static str,
> {
    let count = state.versions.len();
    let mut forward = vec![Vec::<usize>::new(); count];
    let mut reverse = vec![Vec::<usize>::new(); count];
    let mut is_phi = vec![false; count];
    for version in &state.versions {
        let RangeVersionKind::Phi { incoming, .. } = &version.kind else {
            continue;
        };
        is_phi[version.id.0] = true;
        for &(_, input) in incoming {
            let Some(input_version) = state.versions.get(input.0) else {
                return Err("range phi inheritance names an absent version");
            };
            if input_version.atom != version.atom {
                return Err("range phi inheritance crosses range atoms");
            }
            if matches!(input_version.kind, RangeVersionKind::Phi { .. }) {
                forward[version.id.0].push(input.0);
                reverse[input.0].push(version.id.0);
            }
        }
    }
    // Iterative Kosaraju over phi versions only. RTL-generated CFG depth must
    // not become Rust call-stack depth.
    let mut seen = vec![false; count];
    let mut postorder = Vec::new();
    for root in 0..count {
        if !is_phi[root] || seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next)) = stack.last_mut() {
            if *next == forward[*node].len() {
                postorder.push(*node);
                stack.pop();
                continue;
            }
            let successor = forward[*node][*next];
            *next += 1;
            if !seen[successor] {
                seen[successor] = true;
                stack.push((successor, 0));
            }
        }
    }
    let mut assigned = vec![false; count];
    let mut members = Vec::<Vec<usize>>::new();
    for &root in postorder.iter().rev() {
        if assigned[root] {
            continue;
        }
        assigned[root] = true;
        let mut component = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            component.push(node);
            for &predecessor in reverse[node].iter().rev() {
                if !assigned[predecessor] {
                    assigned[predecessor] = true;
                    stack.push(predecessor);
                }
            }
        }
        members.push(component);
    }

    let mut component_for_version = vec![None; count];
    for (row, component) in members.iter().enumerate() {
        let id = RangeInheritanceId(row);
        for &version in component {
            component_for_version[version] = Some(id);
        }
    }
    let mut result = Vec::with_capacity(members.len());
    for (row, component) in members.into_iter().enumerate() {
        let id = RangeInheritanceId(row);
        let atom = state.versions[component[0]].atom;
        let mut incoming = Vec::new();
        let mut cyclic = component.len() > 1;
        for &version_index in &component {
            let version = &state.versions[version_index];
            if version.atom != atom {
                return Err("range phi SCC crosses range atoms");
            }
            let RangeVersionKind::Phi {
                incoming: phi_inputs,
                ..
            } = &version.kind
            else {
                return Err("range inheritance component contains a non-phi version");
            };
            for &(predecessor, input) in phi_inputs {
                if component_for_version[input.0] == Some(id) {
                    cyclic = true;
                } else {
                    incoming.push(RangeIncomingHome {
                        phi: version.id,
                        predecessor,
                        version: input,
                    });
                }
            }
        }
        if incoming.is_empty() {
            return Err("range phi SCC has no external packed-state seed");
        }
        result.push(RangeInheritanceComponent {
            id,
            atom,
            versions: component.into_iter().map(RangeVersionId).collect(),
            incoming,
            cyclic,
        });
    }
    Ok((result, component_for_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashSet;
    use crate::ir::*;
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        }
    }

    fn block(
        id: usize,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) -> BasicBlock<RegionedAbsoluteAddr> {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            instructions,
            terminator,
        }
    }

    fn unit(
        blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: registers.into_iter().collect(),
        }
    }

    fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> (SirCfg, RangeStateSsa, RangeResidencyPlan) {
        let cfg = SirCfg::analyze(eu).unwrap();
        let state = RangeStateSsa::analyze(eu, &cfg, STABLE_REGION, &HashSet::default()).unwrap();
        let plan = RangeResidencyPlan::analyze(&state, &cfg).unwrap();
        (cfg, state, plan)
    }

    fn store(source: RegisterId) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            32,
            source,
            Vec::new(),
            Vec::new(),
        )
    }

    fn load(destination: RegisterId) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Load(destination, address(), SIROffset::Static(0), 32)
    }

    #[test]
    fn one_store_home_can_cover_a_load_and_terminal_boundary() {
        let eu = unit(
            vec![block(
                0,
                vec![store(RegisterId(0)), load(RegisterId(1))],
                SIRTerminator::Return,
            )],
            [(RegisterId(0), bit(32)), (RegisterId(1), bit(32))],
        );
        let (cfg, state, plan) = analyze(&eu);
        let version = state.stores[0].parts[0].version;
        let uses = &plan.versions[version.0].uses;

        assert_eq!(uses.len(), 2);
        assert!(!plan.uses[uses[0].0].kind.packed_state_is_required());
        assert!(plan.uses[uses[1].0].kind.packed_state_is_required());
        assert_eq!(
            plan.writeback_cluster(&state, &cfg, version, uses)
                .unwrap()
                .placement,
            RangeWritebackPlacement::BeforeInstruction {
                block: BlockId(0),
                instruction: 1,
            }
        );
        assert_eq!(
            plan.writeback_cluster(&state, &cfg, version, &uses[1..])
                .unwrap()
                .placement,
            RangeWritebackPlacement::BlockExit { block: BlockId(0) }
        );
    }

    #[test]
    fn diamond_phi_can_write_locally_or_inherit_edge_homes() {
        let eu = unit(
            vec![
                block(
                    0,
                    Vec::new(),
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(
                    1,
                    vec![store(RegisterId(1))],
                    SIRTerminator::Jump(BlockId(3), Vec::new()),
                ),
                block(
                    2,
                    vec![store(RegisterId(2))],
                    SIRTerminator::Jump(BlockId(3), Vec::new()),
                ),
                block(3, Vec::new(), SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
            ],
        );
        let (cfg, state, plan) = analyze(&eu);
        let phi = state
            .versions
            .iter()
            .find(|version| {
                matches!(
                    version.kind,
                    RangeVersionKind::Phi {
                        block: BlockId(3),
                        ..
                    }
                )
            })
            .unwrap();
        let component = plan
            .inheritance
            .iter()
            .find(|component| component.versions == [phi.id])
            .unwrap();

        assert!(!component.cyclic);
        assert_eq!(component.incoming.len(), 2);
        assert!(component.incoming.iter().all(|incoming| matches!(
            state.versions[incoming.version.0].kind,
            RangeVersionKind::Store { .. }
        )));
        assert_eq!(
            plan.versions[phi.id.0].packed_alternatives,
            vec![
                RangePackedAlternative::DeferredWriteback,
                RangePackedAlternative::Inherited(component.id),
            ]
        );
        let cluster = plan
            .writeback_cluster(&state, &cfg, phi.id, &plan.versions[phi.id.0].uses)
            .unwrap();
        assert_eq!(
            cluster.placement,
            RangeWritebackPlacement::BlockExit { block: BlockId(3) }
        );
    }

    #[test]
    fn loop_phi_inheritance_removes_internal_self_edge() {
        let eu = unit(
            vec![
                block(0, Vec::new(), SIRTerminator::Jump(BlockId(1), Vec::new())),
                block(
                    1,
                    Vec::new(),
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                ),
                block(
                    2,
                    Vec::new(),
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(4), Vec::new()),
                        false_block: (BlockId(5), Vec::new()),
                    },
                ),
                block(3, Vec::new(), SIRTerminator::Return),
                block(
                    4,
                    vec![store(RegisterId(2))],
                    SIRTerminator::Jump(BlockId(1), Vec::new()),
                ),
                block(5, Vec::new(), SIRTerminator::Jump(BlockId(1), Vec::new())),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), bit(32)),
            ],
        );
        let (_, state, plan) = analyze(&eu);
        let phi = state
            .versions
            .iter()
            .find(|version| {
                matches!(
                    version.kind,
                    RangeVersionKind::Phi {
                        block: BlockId(1),
                        ..
                    }
                )
            })
            .unwrap();
        let component = &plan.inheritance[match plan.versions[phi.id.0].packed_alternatives[1] {
            RangePackedAlternative::Inherited(id) => id.0,
            _ => panic!("phi lacks inherited packed-state alternative"),
        }];

        assert!(component.cyclic);
        assert_eq!(component.versions, [phi.id]);
        assert_eq!(component.incoming.len(), 2);
        assert!(
            component
                .incoming
                .iter()
                .all(|incoming| incoming.version != phi.id)
        );
        assert!(component.incoming.iter().any(|incoming| matches!(
            state.versions[incoming.version.0].kind,
            RangeVersionKind::LiveOnEntry
        )));
        assert!(component.incoming.iter().any(|incoming| matches!(
            state.versions[incoming.version.0].kind,
            RangeVersionKind::Store { .. }
        )));
    }

    #[test]
    fn allocator_use_clusters_choose_path_local_or_shared_writebacks() {
        let eu = unit(
            vec![
                block(
                    0,
                    vec![store(RegisterId(1))],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(1, vec![load(RegisterId(2))], SIRTerminator::Return),
                block(2, vec![load(RegisterId(3))], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
                (RegisterId(3), bit(32)),
            ],
        );
        let (cfg, state, plan) = analyze(&eu);
        let version = state.stores[0].parts[0].version;
        let load_uses = plan.versions[version.0]
            .uses
            .iter()
            .copied()
            .filter(|use_| matches!(plan.uses[use_.0].kind, RangeHomeUseKind::Load { .. }))
            .collect::<Vec<_>>();

        let shared = plan
            .writeback_cluster(&state, &cfg, version, &load_uses)
            .unwrap();
        let local = plan
            .writeback_cluster(&state, &cfg, version, &load_uses[..1])
            .unwrap();
        assert_eq!(
            shared.placement,
            RangeWritebackPlacement::BlockExit { block: BlockId(0) }
        );
        assert_eq!(
            local.placement,
            RangeWritebackPlacement::BeforeInstruction {
                block: plan.uses[load_uses[0].0].kind.block(),
                instruction: 0,
            }
        );
    }

    #[test]
    fn cluster_rejects_cross_version_and_live_on_entry_requests() {
        let eu = unit(
            vec![block(
                0,
                vec![
                    load(RegisterId(0)),
                    store(RegisterId(1)),
                    load(RegisterId(2)),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(32)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
            ],
        );
        let (cfg, state, plan) = analyze(&eu);
        let entry_use = plan
            .uses
            .iter()
            .find(|use_| {
                matches!(
                    state.versions[use_.version.0].kind,
                    RangeVersionKind::LiveOnEntry
                )
            })
            .unwrap();
        let stored_use = plan
            .uses
            .iter()
            .find(|use_| {
                matches!(
                    state.versions[use_.version.0].kind,
                    RangeVersionKind::Store { .. }
                )
            })
            .unwrap();

        assert!(
            plan.writeback_cluster(&state, &cfg, entry_use.version, &[entry_use.id])
                .is_err()
        );
        assert!(
            plan.writeback_cluster(
                &state,
                &cfg,
                stored_use.version,
                &[stored_use.id, entry_use.id],
            )
            .is_err()
        );
    }

    #[test]
    fn verifier_rejects_a_use_moved_to_another_range_version() {
        let eu = unit(
            vec![block(
                0,
                vec![
                    load(RegisterId(0)),
                    store(RegisterId(1)),
                    load(RegisterId(2)),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(32)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
            ],
        );
        let (cfg, state, mut plan) = analyze(&eu);
        let entry = state
            .versions
            .iter()
            .find(|version| matches!(version.kind, RangeVersionKind::LiveOnEntry))
            .unwrap()
            .id;
        let stored_use = plan
            .uses
            .iter_mut()
            .find(|use_| use_.version != entry)
            .unwrap();
        stored_use.version = entry;

        assert!(plan.verify(&state, &cfg).is_err());
    }
}
