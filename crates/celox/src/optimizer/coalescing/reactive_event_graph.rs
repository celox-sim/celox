//! Sparse RTL-event projection over a fused comb/FF SIR function.
//!
//! This is deliberately an analysis oracle. It retains the executable SIR as
//! the reference and records which value/state dependencies a future
//! clock-event lowering must reproduce.

use std::fmt::Write as _;

use crate::ir::cfg::SirCfg;
use crate::ir::{
    BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction, SIROffset,
    SIRTerminator, SPARSE_WORKING_REGION, STABLE_REGION, SirMergeProvenance, WORKING_REGION,
};
use crate::{HashMap, HashSet};

use super::reactive_phase::FusedPhaseCut;
use super::state_ssa::{MemoryAccessId, MemoryAccessKind, StateFragment, StatePhaseMap, StateSsa};

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct InstructionSite {
    pub block: BlockId,
    pub instruction: usize,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum RootKind {
    CommitFfState,
    ObservableStore,
    RuntimeEvent,
    CombCapture,
    Error,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionRoot {
    kind: RootKind,
    site: Option<InstructionSite>,
    block: BlockId,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum FrontierKind {
    LiveOnEntry,
    MemoryKill,
    UnsupportedStateAccess,
    UnresolvedSsaValue,
    LoopCarriedControl,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionFrontier {
    kind: FrontierKind,
    consumer: InstructionSite,
    fragment: Option<StateFragment>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct UnitStateFlow {
    producer: usize,
    consumer: usize,
    fragment: StateFragment,
}

#[derive(Debug, Clone, Default)]
struct UnitSummary {
    blocks: usize,
    loads: usize,
    stores: usize,
    commits: usize,
    effects: usize,
}

#[derive(Debug)]
pub(crate) struct ReactiveEventProjection {
    unit_summaries: Vec<UnitSummary>,
    roots: Vec<ProjectionRoot>,
    retained_instructions: HashSet<InstructionSite>,
    retained_blocks: HashSet<BlockId>,
    retained_units: HashSet<usize>,
    frontiers: HashSet<ProjectionFrontier>,
    unit_flows: HashSet<UnitStateFlow>,
    phase_by_unit: Vec<bool>,
}

#[derive(Debug, Clone)]
enum ValueDefinition {
    Instruction(InstructionSite),
    BlockParameter(Vec<RegisterId>),
}

#[derive(Debug, Clone, Copy)]
struct StateUse {
    model: usize,
    version: MemoryAccessId,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum Work {
    Instruction(InstructionSite),
    Register(RegisterId),
    Memory {
        model: usize,
        version: MemoryAccessId,
        consumer: InstructionSite,
    },
    Control(BlockId),
}

impl ReactiveEventProjection {
    pub(crate) fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        provenance: &SirMergeProvenance,
        cut: &FusedPhaseCut,
        four_state: bool,
    ) -> Result<Self, String> {
        let cfg = SirCfg::analyze_forward(eu).map_err(|error| error.to_string())?;
        let phases =
            StatePhaseMap::fused(eu, &cfg, cut.ff_entry()).map_err(|error| error.to_string())?;
        let mut state_models = Vec::new();
        for region in [STABLE_REGION, WORKING_REGION, SPARSE_WORKING_REGION] {
            state_models.push(
                StateSsa::analyze_event_projection(eu, &cfg, region, &phases, !four_state)
                    .map_err(|error| error.to_string())?,
            );
        }

        let mut unit_summaries = vec![UnitSummary::default(); provenance.unit_entries.len()];
        for (&block_id, block) in &eu.blocks {
            let unit = provenance.block_units[&block_id];
            unit_summaries[unit].blocks += 1;
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Load(..) => unit_summaries[unit].loads += 1,
                    SIRInstruction::Store(_, _, _, _, triggers, captures) => {
                        unit_summaries[unit].stores += 1;
                        unit_summaries[unit].effects +=
                            usize::from(!triggers.is_empty() || !captures.is_empty());
                    }
                    SIRInstruction::Commit(_, _, _, _, triggers) => {
                        unit_summaries[unit].commits += 1;
                        unit_summaries[unit].effects += usize::from(!triggers.is_empty());
                    }
                    SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                        unit_summaries[unit].effects += 1;
                    }
                    _ => {}
                }
            }
        }

        let definitions = value_definitions(eu, &cfg)?;
        let (state_loads, state_uses) = state_uses(&state_models);
        let mut roots = projection_roots(eu, cut);
        roots.sort_unstable();
        roots.dedup();

        let mut projection = Self {
            unit_summaries,
            roots,
            retained_instructions: HashSet::default(),
            retained_blocks: HashSet::default(),
            retained_units: HashSet::default(),
            frontiers: HashSet::default(),
            unit_flows: HashSet::default(),
            phase_by_unit: provenance
                .unit_entries
                .iter()
                .map(|&entry| cut.is_ff_block(entry))
                .collect(),
        };
        let mut work = Vec::new();
        for root in &projection.roots {
            if let Some(site) = root.site {
                work.push(Work::Instruction(site));
            } else {
                projection.retained_blocks.insert(root.block);
                projection
                    .retained_units
                    .insert(provenance.block_units[&root.block]);
                work.push(Work::Control(root.block));
            }
        }

        let mut seen = HashSet::default();
        while let Some(item) = work.pop() {
            if !seen.insert(item) {
                continue;
            }
            match item {
                Work::Instruction(site) => {
                    let instruction = &eu.blocks[&site.block].instructions[site.instruction];
                    projection.retain_site(site, provenance);
                    for value in super::sir_analysis::instruction_uses(instruction) {
                        work.push(Work::Register(value));
                    }
                    if let Some(uses) = state_uses.get(&site) {
                        for state_use in uses {
                            work.push(Work::Memory {
                                model: state_use.model,
                                version: state_use.version,
                                consumer: site,
                            });
                        }
                    }
                    if let SIRInstruction::Load(destination, address, offset, width) = instruction {
                        if !state_loads.contains(&(site, *destination)) {
                            projection.frontiers.insert(ProjectionFrontier {
                                kind: FrontierKind::UnsupportedStateAccess,
                                consumer: site,
                                fragment: static_fragment(
                                    eu,
                                    *destination,
                                    *address,
                                    offset,
                                    *width,
                                ),
                            });
                        }
                    } else if let SIRInstruction::Commit(_, _, _, _, _) = instruction
                        && !state_uses.contains_key(&site)
                    {
                        projection.frontiers.insert(ProjectionFrontier {
                            kind: FrontierKind::UnsupportedStateAccess,
                            consumer: site,
                            fragment: None,
                        });
                        // The source object is included in the SIR dump; do
                        // not invent a range for a dynamic or rejected recipe.
                    }
                    work.push(Work::Control(site.block));
                }
                Work::Register(value) => match definitions.get(&value) {
                    Some(ValueDefinition::Instruction(definition)) => {
                        work.push(Work::Instruction(*definition));
                    }
                    Some(ValueDefinition::BlockParameter(incoming)) => {
                        for &value in incoming {
                            work.push(Work::Register(value));
                        }
                    }
                    None => {
                        projection.frontiers.insert(ProjectionFrontier {
                            kind: FrontierKind::UnresolvedSsaValue,
                            consumer: InstructionSite {
                                block: eu.entry_block_id,
                                instruction: 0,
                            },
                            fragment: None,
                        });
                    }
                },
                Work::Memory {
                    model,
                    version,
                    consumer,
                } => {
                    let access = &state_models[model].accesses[version.0];
                    let fragment = state_models[model].slots[access.slot].fragment;
                    match &access.kind {
                        MemoryAccessKind::LiveOnEntry => {
                            projection.frontiers.insert(ProjectionFrontier {
                                kind: FrontierKind::LiveOnEntry,
                                consumer,
                                fragment: Some(fragment),
                            });
                        }
                        MemoryAccessKind::Def { .. } => {
                            let definition = InstructionSite {
                                block: access.block.ok_or(
                                    "Reactive StateSSA definition has no containing block",
                                )?,
                                instruction: access.instruction.ok_or(
                                    "Reactive StateSSA definition has no instruction index",
                                )?,
                            };
                            let producer = provenance.block_units[&definition.block];
                            let consumer_unit = provenance.block_units[&consumer.block];
                            if producer != consumer_unit {
                                projection.unit_flows.insert(UnitStateFlow {
                                    producer,
                                    consumer: consumer_unit,
                                    fragment,
                                });
                            }
                            work.push(Work::Instruction(definition));
                        }
                        MemoryAccessKind::Phi { incoming } => {
                            for &(_, version) in incoming {
                                work.push(Work::Memory {
                                    model,
                                    version,
                                    consumer,
                                });
                            }
                        }
                        MemoryAccessKind::Kill => {
                            let definition = access
                                .block
                                .zip(access.instruction)
                                .map(|(block, instruction)| InstructionSite { block, instruction });
                            if definition.is_some_and(|site| {
                                matches!(
                                    eu.blocks[&site.block].instructions[site.instruction],
                                    SIRInstruction::Commit(..)
                                )
                            }) {
                                // A cross-region Commit is an exact state-copy
                                // node. Visiting the instruction follows its
                                // source-region MemorySSA use.
                                work.push(Work::Instruction(definition.unwrap()));
                            } else {
                                projection.frontiers.insert(ProjectionFrontier {
                                    kind: FrontierKind::MemoryKill,
                                    consumer,
                                    fragment: Some(fragment),
                                });
                                if let Some(definition) = definition {
                                    work.push(Work::Instruction(definition));
                                }
                            }
                        }
                        MemoryAccessKind::Use { reaching, .. } => {
                            work.push(Work::Memory {
                                model,
                                version: *reaching,
                                consumer,
                            });
                        }
                    }
                }
                Work::Control(block) => {
                    let current = cfg.index[&block];
                    let consumer = InstructionSite {
                        block,
                        instruction: 0,
                    };
                    if cfg.sccs[cfg.scc_for_block[current]].cyclic {
                        projection.frontiers.insert(ProjectionFrontier {
                            kind: FrontierKind::LoopCarriedControl,
                            consumer,
                            fragment: None,
                        });
                    }
                    if let Some(parent) = cfg.dominators.idom[current] {
                        let parent_id = cfg.block_ids[parent];
                        projection.retained_blocks.insert(parent_id);
                        projection
                            .retained_units
                            .insert(provenance.block_units[&parent_id]);
                        match &eu.blocks[&parent_id].terminator {
                            SIRTerminator::Branch { cond, .. } => {
                                work.push(Work::Register(*cond));
                            }
                            SIRTerminator::Switch { selector, .. } => {
                                work.push(Work::Register(*selector));
                            }
                            _ => {}
                        }
                        work.push(Work::Control(parent_id));
                    }
                }
            }
        }
        Ok(projection)
    }

    fn retain_site(&mut self, site: InstructionSite, provenance: &SirMergeProvenance) {
        self.retained_instructions.insert(site);
        self.retained_blocks.insert(site.block);
        self.retained_units
            .insert(provenance.block_units[&site.block]);
    }

    pub(crate) fn format_report(&self) -> String {
        let mut output = String::new();
        let comb_units = self.phase_by_unit.iter().filter(|&&ff| !ff).count();
        let ff_units = self.phase_by_unit.len() - comb_units;
        writeln!(
            output,
            "Reactive clock-event projection oracle: units={} comb_units={} ff_units={}",
            self.phase_by_unit.len(),
            comb_units,
            ff_units
        )
        .unwrap();
        writeln!(
            output,
            "roots={} retained_units={} retained_blocks={} retained_instructions={} frontiers={} cross_unit_state_flows={}",
            self.roots.len(),
            self.retained_units.len(),
            self.retained_blocks.len(),
            self.retained_instructions.len(),
            self.frontiers.len(),
            self.unit_flows.len()
        )
        .unwrap();
        writeln!(output, "Units:").unwrap();
        for (unit, summary) in self.unit_summaries.iter().enumerate() {
            writeln!(
                output,
                "  u{unit} phase={} blocks={} loads={} stores={} commits={} effects={}",
                if self.phase_by_unit[unit] {
                    "ff"
                } else {
                    "comb"
                },
                summary.blocks,
                summary.loads,
                summary.stores,
                summary.commits,
                summary.effects
            )
            .unwrap();
        }
        writeln!(output, "Roots:").unwrap();
        for root in &self.roots {
            match root.site {
                Some(site) => writeln!(
                    output,
                    "  {:?} b{}.i{}",
                    root.kind, site.block.0, site.instruction
                )
                .unwrap(),
                None => writeln!(output, "  {:?} b{}", root.kind, root.block.0).unwrap(),
            }
        }
        let mut flows = self.unit_flows.iter().copied().collect::<Vec<_>>();
        flows.sort_unstable();
        writeln!(output, "Cross-unit state flows:").unwrap();
        for flow in flows {
            writeln!(
                output,
                "  u{} -> u{} {} offset={} width={} dynamic={} plane={:?}",
                flow.producer,
                flow.consumer,
                flow.fragment.addr,
                flow.fragment.bit_offset,
                flow.fragment.width,
                flow.fragment.dynamic,
                flow.fragment.plane
            )
            .unwrap();
        }
        let mut frontiers = self.frontiers.iter().copied().collect::<Vec<_>>();
        frontiers.sort_unstable();
        writeln!(output, "Materialization/control frontiers:").unwrap();
        for frontier in frontiers {
            write!(
                output,
                "  {:?} at b{}.i{}",
                frontier.kind, frontier.consumer.block.0, frontier.consumer.instruction
            )
            .unwrap();
            if let Some(fragment) = frontier.fragment {
                write!(
                    output,
                    " {} offset={} width={} dynamic={} plane={:?}",
                    fragment.addr,
                    fragment.bit_offset,
                    fragment.width,
                    fragment.dynamic,
                    fragment.plane
                )
                .unwrap();
            }
            output.push('\n');
        }
        let mut retained = self
            .retained_instructions
            .iter()
            .copied()
            .collect::<Vec<_>>();
        retained.sort_unstable();
        writeln!(output, "Retained instruction sites:").unwrap();
        for site in retained {
            writeln!(output, "  b{}.i{}", site.block.0, site.instruction).unwrap();
        }
        output
    }
}

fn projection_roots(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cut: &FusedPhaseCut,
) -> Vec<ProjectionRoot> {
    let mut roots = Vec::new();
    for (&block_id, block) in &eu.blocks {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let site = InstructionSite {
                block: block_id,
                instruction: instruction_index,
            };
            let kind = match instruction {
                SIRInstruction::Store(_, _, _, _, triggers, captures)
                    if !triggers.is_empty() || !captures.is_empty() =>
                {
                    Some(RootKind::ObservableStore)
                }
                SIRInstruction::Commit(source, destination, _, _, _)
                    if cut.is_ff_block(block_id)
                        && destination.region == STABLE_REGION
                        && matches!(source.region, WORKING_REGION | SPARSE_WORKING_REGION) =>
                {
                    Some(RootKind::CommitFfState)
                }
                SIRInstruction::Commit(_, _, _, _, triggers) if !triggers.is_empty() => {
                    Some(RootKind::ObservableStore)
                }
                SIRInstruction::RuntimeEvent { .. } => Some(RootKind::RuntimeEvent),
                SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. } => Some(RootKind::CombCapture),
                _ => None,
            };
            if let Some(kind) = kind {
                roots.push(ProjectionRoot {
                    kind,
                    site: Some(site),
                    block: block_id,
                });
            }
        }
        if matches!(block.terminator, SIRTerminator::Error(_)) {
            roots.push(ProjectionRoot {
                kind: RootKind::Error,
                site: None,
                block: block_id,
            });
        }
    }
    roots
}

fn value_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
) -> Result<HashMap<RegisterId, ValueDefinition>, String> {
    let mut definitions = HashMap::default();
    for (&block_id, block) in &eu.blocks {
        for (instruction, value) in
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| {
                    instruction.defined_register().map(|value| (index, value))
                })
        {
            if definitions
                .insert(
                    value,
                    ValueDefinition::Instruction(InstructionSite {
                        block: block_id,
                        instruction,
                    }),
                )
                .is_some()
            {
                return Err(format!("r{} has more than one SIR definition", value.0));
            }
        }
    }
    for &block_id in &cfg.block_ids {
        let block = &eu.blocks[&block_id];
        for (parameter_index, &parameter) in block.params.iter().enumerate() {
            let mut incoming = Vec::new();
            for &predecessor in &cfg.predecessors[cfg.index[&block_id]] {
                let predecessor = cfg.block_ids[predecessor];
                let arguments = edge_arguments(&eu.blocks[&predecessor].terminator, block_id)
                    .ok_or_else(|| {
                        format!(
                            "CFG edge b{} -> b{} has no SIR argument list",
                            predecessor.0, block_id.0
                        )
                    })?;
                let Some(&argument) = arguments.get(parameter_index) else {
                    return Err(format!(
                        "CFG edge b{} -> b{} omits argument {}",
                        predecessor.0, block_id.0, parameter_index
                    ));
                };
                incoming.push(argument);
            }
            if definitions
                .insert(parameter, ValueDefinition::BlockParameter(incoming))
                .is_some()
            {
                return Err(format!("r{} has more than one SIR definition", parameter.0));
            }
        }
    }
    Ok(definitions)
}

fn edge_arguments(terminator: &SIRTerminator, target: BlockId) -> Option<&[RegisterId]> {
    match terminator {
        SIRTerminator::Jump(block, arguments) if *block == target => Some(arguments),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } if true_block.0 == target => Some(&true_block.1),
        SIRTerminator::Branch { false_block, .. } if false_block.0 == target => {
            Some(&false_block.1)
        }
        SIRTerminator::Switch { cases, default, .. }
            if *default == target || cases.iter().any(|case| case.target == target) =>
        {
            Some(&[])
        }
        _ => None,
    }
}

fn state_uses(
    models: &[StateSsa],
) -> (
    HashSet<(InstructionSite, RegisterId)>,
    HashMap<InstructionSite, Vec<StateUse>>,
) {
    let mut loads = HashSet::default();
    let mut uses = HashMap::<InstructionSite, Vec<StateUse>>::default();
    for (model, state_ssa) in models.iter().enumerate() {
        for access in &state_ssa.accesses {
            let MemoryAccessKind::Use {
                destination,
                reaching,
            } = access.kind
            else {
                continue;
            };
            let (Some(block), Some(instruction)) = (access.block, access.instruction) else {
                continue;
            };
            let site = InstructionSite { block, instruction };
            if let Some(destination) = destination {
                loads.insert((site, destination));
            }
            uses.entry(site).or_default().push(StateUse {
                model,
                version: reaching,
            });
        }
    }
    (loads, uses)
}

fn static_fragment(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    destination: RegisterId,
    address: RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) -> Option<StateFragment> {
    let SIROffset::Static(bit_offset) = offset else {
        return None;
    };
    Some(StateFragment::from_access(
        address,
        *bit_offset,
        width,
        eu.register_map.get(&destination)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BasicBlock, DomainKind, InstanceId, RegisterType, SIRValue, TriggerIdWithKind,
        WORKING_REGION,
    };
    use veryl_analyzer::ir::VarId;

    fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(variable),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        registers: &[(usize, usize)],
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: registers
                .iter()
                .map(|&(register, width)| {
                    (
                        RegisterId(register),
                        RegisterType::Bit {
                            width,
                            signed: false,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn clock_projection_reaches_comb_store_through_exact_state_version() {
        let signal = address(STABLE_REGION, 0);
        let next = address(WORKING_REGION, 1);
        let published = address(STABLE_REGION, 1);
        let comb = unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u32)),
                SIRInstruction::Store(
                    signal,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
            ],
            &[(0, 8)],
        );
        let ff = unit(
            vec![
                SIRInstruction::Load(RegisterId(0), signal, SIROffset::Static(0), 8),
                SIRInstruction::Store(next, SIROffset::Static(0), 8, RegisterId(0), vec![], vec![]),
                SIRInstruction::Commit(next, published, SIROffset::Static(0), 8, vec![]),
            ],
            &[(0, 8)],
        );
        let (merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);
        let cut = super::super::reactive_phase::verify(&merged, &provenance, 1).unwrap();

        let projection =
            ReactiveEventProjection::analyze(&merged, &provenance, &cut, false).unwrap();

        assert!(projection.unit_flows.iter().any(|flow| {
            flow.producer == 0
                && flow.consumer == 1
                && flow.fragment.addr == signal
                && flow.fragment.bit_offset == 0
                && flow.fragment.width == 8
        }));
        assert!(projection.retained_units.contains(&0));
        assert!(projection.retained_units.contains(&1));
    }

    #[test]
    fn unobserved_comb_store_is_not_a_projection_root() {
        let signal = address(STABLE_REGION, 0);
        let unrelated = address(STABLE_REGION, 1);
        let next = address(WORKING_REGION, 2);
        let published = address(STABLE_REGION, 2);
        let comb = unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u32)),
                SIRInstruction::Store(
                    signal,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(9u32)),
                SIRInstruction::Store(
                    unrelated,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    vec![],
                    vec![],
                ),
            ],
            &[(0, 8), (1, 8)],
        );
        let ff = unit(
            vec![
                SIRInstruction::Load(RegisterId(0), signal, SIROffset::Static(0), 8),
                SIRInstruction::Store(next, SIROffset::Static(0), 8, RegisterId(0), vec![], vec![]),
                SIRInstruction::Commit(next, published, SIROffset::Static(0), 8, vec![]),
            ],
            &[(0, 8)],
        );
        let (merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);
        let cut = super::super::reactive_phase::verify(&merged, &provenance, 1).unwrap();

        let projection =
            ReactiveEventProjection::analyze(&merged, &provenance, &cut, false).unwrap();
        let report = projection.format_report();

        assert!(report.contains("u0 -> u1"));
        assert_eq!(
            projection
                .roots
                .iter()
                .filter(|root| root.kind == RootKind::CommitFfState)
                .count(),
            1
        );
        assert!(
            !projection.retained_instructions.contains(&InstructionSite {
                block: provenance.unit_entries[0],
                instruction: 3,
            })
        );
    }

    #[test]
    fn effectful_comb_store_remains_a_root() {
        let output = address(STABLE_REGION, 0);
        let next = address(WORKING_REGION, 1);
        let comb = unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u32)),
                SIRInstruction::Store(
                    output,
                    SIROffset::Static(0),
                    1,
                    RegisterId(0),
                    vec![TriggerIdWithKind {
                        kind: DomainKind::Other,
                        id: 0,
                    }],
                    vec![],
                ),
            ],
            &[(0, 1)],
        );
        let ff = unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u32)),
                SIRInstruction::Store(next, SIROffset::Static(0), 1, RegisterId(0), vec![], vec![]),
            ],
            &[(0, 1)],
        );
        let (merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);
        let cut = super::super::reactive_phase::verify(&merged, &provenance, 1).unwrap();

        let projection =
            ReactiveEventProjection::analyze(&merged, &provenance, &cut, false).unwrap();

        assert!(
            projection
                .roots
                .iter()
                .any(|root| root.kind == RootKind::ObservableStore)
        );
        assert!(projection.retained_instructions.contains(&InstructionSite {
            block: provenance.unit_entries[0],
            instruction: 1,
        }));
    }

    #[test]
    fn stable_to_working_commit_is_a_state_copy_not_a_publication_root() {
        let stable = address(STABLE_REGION, 0);
        let working = address(WORKING_REGION, 0);
        let comb = unit(Vec::new(), &[]);
        let ff = unit(
            vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, vec![]),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, vec![]),
            ],
            &[],
        );
        let (merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);
        let cut = super::super::reactive_phase::verify(&merged, &provenance, 1).unwrap();

        let projection =
            ReactiveEventProjection::analyze(&merged, &provenance, &cut, false).unwrap();
        let ff_entry = provenance.unit_entries[1];

        assert_eq!(
            projection
                .roots
                .iter()
                .filter(|root| root.kind == RootKind::CommitFfState)
                .count(),
            1
        );
        assert!(projection.retained_instructions.contains(&InstructionSite {
            block: ff_entry,
            instruction: 0,
        }));
        assert!(
            projection
                .frontiers
                .iter()
                .any(|frontier| frontier.kind == FrontierKind::LiveOnEntry)
        );
        assert!(
            !projection
                .frontiers
                .iter()
                .any(|frontier| frontier.kind == FrontierKind::MemoryKill)
        );
    }
}
