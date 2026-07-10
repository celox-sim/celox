//! Braun--Hack section 4.1: CFG-global next-use distances.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::cfg::NormalizedCfg;

/// Lexicographic next-use distance.
///
/// Exiting any loop region is more expensive than every instruction-only path,
/// independent of function size.  `Dead` sorts after every finite distance so
/// MIN naturally evicts values with no next use first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NextUseDistance {
    Finite {
        loop_exits: usize,
        instructions: usize,
    },
    Dead,
}

impl NextUseDistance {
    fn local(instructions: usize) -> Self {
        Self::Finite {
            loop_exits: 0,
            instructions,
        }
    }

    fn checked_across_edge(self, loop_exits: usize) -> Option<Self> {
        match self {
            Self::Finite {
                loop_exits: current,
                instructions,
            } => Some(Self::Finite {
                loop_exits: current.checked_add(loop_exits)?,
                instructions,
            }),
            Self::Dead => Some(Self::Dead),
        }
    }

    fn checked_prepend_instructions(self, instructions: usize) -> Option<Self> {
        match self {
            Self::Finite {
                loop_exits,
                instructions: current,
            } => Some(Self::Finite {
                loop_exits,
                instructions: current.checked_add(instructions)?,
            }),
            Self::Dead => Some(Self::Dead),
        }
    }

    pub(super) fn is_dead(self) -> bool {
        matches!(self, Self::Dead)
    }
}

impl Ord for NextUseDistance {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::Dead, Self::Dead) => Ordering::Equal,
            (Self::Dead, _) => Ordering::Greater,
            (_, Self::Dead) => Ordering::Less,
            (
                Self::Finite {
                    loop_exits: left_exits,
                    instructions: left_instructions,
                },
                Self::Finite {
                    loop_exits: right_exits,
                    instructions: right_instructions,
                },
            ) => (left_exits, left_instructions).cmp(&(right_exits, right_instructions)),
        }
    }
}

impl PartialOrd for NextUseDistance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub(super) struct NextUseAnalysis {
    pub entry: Vec<HashMap<VReg, NextUseDistance>>,
    pub exit: Vec<HashMap<VReg, NextUseDistance>>,
    pub block_max_pressure: Vec<usize>,
    pub loop_regions: Vec<LoopRegionFacts>,
    entry_region: Vec<Option<usize>>,
    region_uses: RegionUseIndex,
    use_positions: Vec<HashMap<VReg, Vec<usize>>>,
    #[cfg(test)]
    region_work: RegionAggregationWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NextUseError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl NextUseError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopRegionKind {
    Natural,
    IrreducibleScc,
}

/// Shape and maximum pressure for one loop region.  Region use membership is
/// queried lazily through [`NextUseAnalysis::used_in_region`]; a complete set
/// is deliberately not materialized here.
#[derive(Debug)]
pub(super) struct LoopRegionFacts {
    pub kind: LoopRegionKind,
    pub entries: Vec<usize>,
    pub max_pressure: usize,
}

pub(super) fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<NextUseAnalysis, NextUseError> {
    let block_count = func.blocks.len();
    if cfg.predecessors.len() != block_count || cfg.successors.len() != block_count {
        return Err(NextUseError::new(
            "NEXT_USE.MODEL_SHAPE",
            None,
            None,
            Vec::new(),
            format!(
                "normalized CFG has {} predecessor rows and {} successor rows for {block_count} MIR blocks",
                cfg.predecessors.len(),
                cfg.successors.len()
            ),
        ));
    }
    for (block, edges) in cfg.predecessors.iter().chain(&cfg.successors).enumerate() {
        if let Some(&target) = edges.iter().find(|&&target| target >= block_count) {
            let source = block % block_count.max(1);
            return Err(NextUseError::new(
                "NEXT_USE.MODEL_SHAPE",
                func.blocks.get(source).map(|block| block.id),
                None,
                Vec::new(),
                format!("normalized CFG references out-of-range block index {target}"),
            ));
        }
    }
    let transfers = block_transfers(func);
    let use_positions = func
        .blocks
        .iter()
        .map(|block| {
            let mut positions = HashMap::<VReg, Vec<usize>>::new();
            for (instruction, inst) in block.insts.iter().enumerate() {
                for value in inst.uses() {
                    let value_positions = positions.entry(value).or_default();
                    if value_positions.last().copied() != Some(instruction) {
                        value_positions.push(instruction);
                    }
                }
            }
            positions
        })
        .collect::<Vec<_>>();
    let phi_uses = phi_edge_uses(func, cfg)?;
    let region_topology = RegionTopology::build(cfg)?;
    let edge_loop_exits = &region_topology.edge_exits;
    let mut entry: Vec<HashMap<VReg, NextUseDistance>> = vec![HashMap::new(); func.blocks.len()];
    let mut exit: Vec<HashMap<VReg, NextUseDistance>> = vec![HashMap::new(); func.blocks.len()];
    let mut queue = (0..func.blocks.len()).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];
    while let Some(block) = queue.pop_front() {
        queued[block] = false;
        let mut next_exit = HashMap::<VReg, NextUseDistance>::new();
        for (edge, &successor) in cfg.successors[block].iter().enumerate() {
            let edge_exits = edge_loop_exits[block][edge];
            for (&value, &distance) in &entry[successor] {
                let Some(distance) = distance.checked_across_edge(edge_exits) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.DISTANCE_RANGE",
                        Some(func.blocks[block].id),
                        None,
                        vec![value],
                        "loop-region exit distance exceeds addressable CFG size",
                    ));
                };
                next_exit
                    .entry(value)
                    .and_modify(|current| *current = (*current).min(distance))
                    .or_insert(distance);
            }
            for &value in &phi_uses[block][edge] {
                let distance = NextUseDistance::Finite {
                    loop_exits: edge_exits,
                    instructions: 0,
                };
                next_exit
                    .entry(value)
                    .and_modify(|current| *current = (*current).min(distance))
                    .or_insert(distance);
            }
        }
        let transfer = &transfers[block];
        let mut next_entry = HashMap::new();
        for (&value, &distance) in &next_exit {
            if !transfer.definitions.contains(&value) {
                let Some(distance) = distance.checked_prepend_instructions(transfer.length) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.DISTANCE_RANGE",
                        Some(func.blocks[block].id),
                        Some(0),
                        vec![value],
                        "next-use instruction distance exceeds addressable MIR size",
                    ));
                };
                next_entry.insert(value, distance);
            }
        }
        for &(value, position) in &transfer.local_uses {
            next_entry.insert(value, NextUseDistance::local(position));
        }
        if next_entry != entry[block] || next_exit != exit[block] {
            entry[block] = next_entry;
            exit[block] = next_exit;
            for &predecessor in &cfg.predecessors[block] {
                if !queued[predecessor] {
                    queue.push_back(predecessor);
                    queued[predecessor] = true;
                }
            }
        }
    }

    let (block_summaries, mut region_work) = block_region_summaries(func, &exit)?;
    let block_max_pressure = block_summaries
        .iter()
        .map(|summary| summary.max_pressure)
        .collect::<Vec<_>>();
    let region_uses = RegionUseIndex::build(
        func.vregs.count() as usize,
        &region_topology,
        &block_summaries,
        &mut region_work,
    )?;
    let region_pressure =
        aggregate_region_pressure(&region_topology, &block_summaries, &mut region_work)?;
    let loop_regions = region_topology
        .regions
        .iter()
        .zip(region_pressure)
        .map(|(shape, max_pressure)| LoopRegionFacts {
            kind: shape.kind,
            entries: shape.entries.clone(),
            max_pressure,
        })
        .collect();
    Ok(NextUseAnalysis {
        entry,
        exit,
        block_max_pressure,
        loop_regions,
        entry_region: region_topology.entry_region,
        region_uses,
        use_positions,
        #[cfg(test)]
        region_work,
    })
}

impl NextUseAnalysis {
    pub(super) fn verify(&self, func: &MFunction, cfg: &NormalizedCfg) -> Result<(), NextUseError> {
        let block_count = func.blocks.len();
        if self.entry.len() != block_count
            || self.exit.len() != block_count
            || self.block_max_pressure.len() != block_count
            || self.entry_region.len() != block_count
            || self.use_positions.len() != block_count
        {
            return Err(NextUseError::new(
                "NEXT_USE.MODEL_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "next-use block tables must all contain {block_count} rows (entry={}, exit={}, pressure={}, entry_region={}, use_positions={})",
                    self.entry.len(),
                    self.exit.len(),
                    self.block_max_pressure.len(),
                    self.entry_region.len(),
                    self.use_positions.len()
                ),
            ));
        }
        if cfg.predecessors.len() != block_count || cfg.successors.len() != block_count {
            return Err(NextUseError::new(
                "NEXT_USE.MODEL_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "normalized CFG has {} predecessor rows and {} successor rows for {block_count} MIR blocks",
                    cfg.predecessors.len(),
                    cfg.successors.len()
                ),
            ));
        }
        for (block, edges) in cfg.predecessors.iter().enumerate() {
            if let Some(&target) = edges.iter().find(|&&target| target >= block_count) {
                return Err(NextUseError::new(
                    "NEXT_USE.MODEL_SHAPE",
                    func.blocks.get(block).map(|block| block.id),
                    None,
                    Vec::new(),
                    format!("normalized CFG references out-of-range predecessor {target}"),
                ));
            }
        }
        for (block, edges) in cfg.successors.iter().enumerate() {
            if let Some(&target) = edges.iter().find(|&&target| target >= block_count) {
                return Err(NextUseError::new(
                    "NEXT_USE.MODEL_SHAPE",
                    func.blocks.get(block).map(|block| block.id),
                    None,
                    Vec::new(),
                    format!("normalized CFG references out-of-range successor {target}"),
                ));
            }
        }

        let value_count = func.vregs.count();
        for (block, mir_block) in func.blocks.iter().enumerate() {
            for &value in self.entry[block]
                .keys()
                .chain(self.exit[block].keys())
                .chain(self.use_positions[block].keys())
            {
                if value.0 >= value_count {
                    return Err(NextUseError::new(
                        "NEXT_USE.MODEL_SHAPE",
                        Some(mir_block.id),
                        None,
                        vec![value],
                        format!(
                            "next-use table references v{} but the function has {value_count} virtual registers",
                            value.0
                        ),
                    ));
                }
            }
            for (&value, &distance) in self.entry[block].iter().chain(&self.exit[block]) {
                if distance.is_dead() {
                    return Err(NextUseError::new(
                        "NEXT_USE.DATAFLOW_EQUATION",
                        Some(mir_block.id),
                        None,
                        vec![value],
                        "live next-use maps must omit dead values instead of storing Dead",
                    ));
                }
            }
        }

        verify_use_positions_match_mir(self, func)?;
        let topology = RegionTopology::build(cfg)?;
        verify_dataflow_equations(self, func, cfg, &topology)?;
        if self.entry_region != topology.entry_region {
            let block = self
                .entry_region
                .iter()
                .zip(&topology.entry_region)
                .position(|(actual, expected)| actual != expected)
                .map(|block| func.blocks[block].id);
            return Err(NextUseError::new(
                "NEXT_USE.ENTRY_REGION",
                block,
                None,
                Vec::new(),
                "next-use entry-region ownership does not match the normalized CFG",
            ));
        }
        if self.loop_regions.len() != topology.regions.len() {
            return Err(NextUseError::new(
                "NEXT_USE.REGION_FACTS",
                None,
                None,
                Vec::new(),
                format!(
                    "next-use analysis has {} loop regions but the CFG has {}",
                    self.loop_regions.len(),
                    topology.regions.len()
                ),
            ));
        }
        let (summaries, mut work) = block_region_summaries(func, &self.exit)?;
        let expected_pressure = summaries
            .iter()
            .map(|summary| summary.max_pressure)
            .collect::<Vec<_>>();
        if self.block_max_pressure != expected_pressure {
            let block = self
                .block_max_pressure
                .iter()
                .zip(&expected_pressure)
                .position(|(actual, expected)| actual != expected)
                .map(|block| func.blocks[block].id);
            return Err(NextUseError::new(
                "NEXT_USE.REGION_FACTS",
                block,
                None,
                Vec::new(),
                "recorded block pressure does not match next-use liveness",
            ));
        }
        let uses = RegionUseIndex::build(
            func.vregs.count() as usize,
            &topology,
            &summaries,
            &mut work,
        )?;
        if self.region_uses != uses {
            return Err(NextUseError::new(
                "NEXT_USE.REGION_FACTS",
                None,
                None,
                Vec::new(),
                "loop-region use membership does not match next-use liveness",
            ));
        }
        let pressure = aggregate_region_pressure(&topology, &summaries, &mut work)?;
        for (region, facts) in self.loop_regions.iter().enumerate() {
            if facts.kind != topology.regions[region].kind
                || facts.entries != topology.regions[region].entries
                || facts.max_pressure != pressure[region]
            {
                let block = topology.regions[region]
                    .entries
                    .first()
                    .map(|&block| func.blocks[block].id);
                return Err(NextUseError::new(
                    "NEXT_USE.REGION_FACTS",
                    block,
                    None,
                    Vec::new(),
                    format!("loop-region facts for region {region} do not match the CFG"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn distance_at(
        &self,
        func: &MFunction,
        block: usize,
        instruction: usize,
        value: VReg,
    ) -> NextUseDistance {
        if let Some(positions) = self.use_positions[block].get(&value) {
            let next = positions.partition_point(|position| *position < instruction);
            if let Some(&position) = positions.get(next) {
                return NextUseDistance::local(position - instruction);
            }
        }
        let remaining = func.blocks[block].insts.len() - instruction;
        self.exit[block]
            .get(&value)
            .and_then(|distance| distance.checked_prepend_instructions(remaining))
            .unwrap_or(NextUseDistance::Dead)
    }

    pub(super) fn region_at_entry(&self, block: usize) -> Option<usize> {
        self.entry_region[block]
    }

    pub(super) fn used_in_region(&self, region: usize, value: VReg) -> bool {
        self.region_uses.contains(
            value,
            self.region_uses.region_start[region],
            self.region_uses.region_end[region],
        )
    }
}

/// Independently derive operand positions from MIR.  A repeated operand is one
/// use position, because distance is measured to an instruction rather than to
/// an operand slot within that instruction.
fn verify_use_positions_match_mir(
    analysis: &NextUseAnalysis,
    func: &MFunction,
) -> Result<(), NextUseError> {
    for (block, mir_block) in func.blocks.iter().enumerate() {
        let actual = &analysis.use_positions[block];
        let mut expected_count = HashMap::<VReg, usize>::new();
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let uses = inst.uses();
            for (operand, &value) in uses.iter().enumerate() {
                if uses[..operand].contains(&value) {
                    continue;
                }
                let occurrence = expected_count.entry(value).or_default();
                let recorded = actual
                    .get(&value)
                    .and_then(|positions| positions.get(*occurrence))
                    .copied();
                if recorded != Some(instruction) {
                    return Err(NextUseError::new(
                        "NEXT_USE.USE_POSITIONS_MATCH_MIR",
                        Some(mir_block.id),
                        Some(instruction),
                        vec![value],
                        format!(
                            "use occurrence {occurrence} of {value} records {recorded:?}, expected instruction {instruction}"
                        ),
                    ));
                }
                *occurrence += 1;
            }
        }
        for (&value, positions) in actual {
            let count = expected_count.get(&value).copied().unwrap_or(0);
            if positions.len() != count {
                return Err(NextUseError::new(
                    "NEXT_USE.USE_POSITIONS_MATCH_MIR",
                    Some(mir_block.id),
                    positions.get(count).copied(),
                    vec![value],
                    format!(
                        "{value} records {} instruction positions, expected {count}",
                        positions.len()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Check the Bellman equations for every value at every block boundary.  This
/// does not call `analyze` or consume its transfer/phi tables: block definitions,
/// upward-exposed uses, and phi-edge uses are reconstructed directly from MIR.
/// MIR blocks always contain a terminator, so the instruction component grows
/// strictly around every CFG cycle.  Consequently the complete equations have
/// no unsupported finite cyclic solution: checking all of them is a certificate
/// for the unique finite distances anchored at real instruction/phi uses.
fn verify_dataflow_equations(
    analysis: &NextUseAnalysis,
    func: &MFunction,
    cfg: &NormalizedCfg,
    topology: &RegionTopology,
) -> Result<(), NextUseError> {
    let phi_uses = verifier_phi_edge_uses(func, cfg)?;
    for (block, mir_block) in func.blocks.iter().enumerate() {
        let Some(edge_loop_exits) = topology.edge_exits.get(block) else {
            return Err(NextUseError::new(
                "NEXT_USE.DATAFLOW_EQUATION",
                Some(mir_block.id),
                None,
                Vec::new(),
                "loop-exit facts do not cover this MIR block",
            ));
        };
        if edge_loop_exits.len() != cfg.successors[block].len() {
            return Err(NextUseError::new(
                "NEXT_USE.DATAFLOW_EQUATION",
                Some(mir_block.id),
                None,
                Vec::new(),
                "loop-exit facts do not cover every successor edge",
            ));
        }

        let mut expected_exit = HashMap::<VReg, NextUseDistance>::new();
        for (edge, &successor) in cfg.successors[block].iter().enumerate() {
            let loop_exits = edge_loop_exits[edge];
            for (&value, &successor_distance) in &analysis.entry[successor] {
                let Some(distance) = successor_distance.checked_across_edge(loop_exits) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.DISTANCE_RANGE",
                        Some(mir_block.id),
                        Some(mir_block.insts.len()),
                        vec![value],
                        "loop-region exit distance exceeds addressable CFG size",
                    ));
                };
                insert_min_distance(&mut expected_exit, value, distance);
            }

            for &source in &phi_uses[block][edge] {
                insert_min_distance(
                    &mut expected_exit,
                    source,
                    NextUseDistance::Finite {
                        loop_exits,
                        instructions: 0,
                    },
                );
            }
        }
        verify_distance_map(
            mir_block.id,
            mir_block.insts.len(),
            "exit",
            &analysis.exit[block],
            &expected_exit,
        )?;

        let mut definitions = mir_block
            .phis
            .iter()
            .map(|phi| phi.dst)
            .collect::<HashSet<_>>();
        let mut local_uses = HashMap::<VReg, usize>::new();
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            for value in inst.uses() {
                if !definitions.contains(&value) {
                    local_uses.entry(value).or_insert(instruction);
                }
            }
            if let Some(definition) = inst.def() {
                definitions.insert(definition);
            }
        }

        let mut expected_entry = HashMap::<VReg, NextUseDistance>::new();
        for (&value, &exit_distance) in &expected_exit {
            if definitions.contains(&value) {
                continue;
            }
            let Some(distance) = exit_distance.checked_prepend_instructions(mir_block.insts.len())
            else {
                return Err(NextUseError::new(
                    "NEXT_USE.DISTANCE_RANGE",
                    Some(mir_block.id),
                    Some(0),
                    vec![value],
                    "next-use instruction distance exceeds addressable MIR size",
                ));
            };
            expected_entry.insert(value, distance);
        }
        for (value, instruction) in local_uses {
            expected_entry.insert(value, NextUseDistance::local(instruction));
        }
        verify_distance_map(
            mir_block.id,
            0,
            "entry",
            &analysis.entry[block],
            &expected_entry,
        )?;
    }
    Ok(())
}

/// Index verifier-side phi uses in one pass over phi operands.  This deliberately
/// traverses successor phis to their predecessors, unlike the producer's
/// predecessor-edge scan, and avoids quadratic rescans at large joins.
fn verifier_phi_edge_uses(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<Vec<Vec<Vec<VReg>>>, NextUseError> {
    let mut edge_uses = cfg
        .successors
        .iter()
        .map(|successors| vec![Vec::new(); successors.len()])
        .collect::<Vec<_>>();
    for (successor, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            let mut covered = HashSet::<usize>::new();
            for &(predecessor_id, source) in &phi.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.PHI_EDGE_COVERAGE",
                        Some(block.id),
                        None,
                        vec![phi.dst, source],
                        format!("phi names predecessor {predecessor_id} outside the CFG"),
                    ));
                };
                let Some(predecessor_successors) = cfg.successors.get(predecessor) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.PHI_EDGE_COVERAGE",
                        Some(block.id),
                        None,
                        vec![phi.dst, source],
                        format!(
                            "phi predecessor {predecessor_id} maps to out-of-range index {predecessor}"
                        ),
                    ));
                };
                let Some(edge) = predecessor_successors
                    .iter()
                    .position(|target| *target == successor)
                else {
                    return Err(NextUseError::new(
                        "NEXT_USE.PHI_EDGE_COVERAGE",
                        Some(block.id),
                        None,
                        vec![phi.dst, source],
                        format!("phi source {predecessor_id} does not name an incoming CFG edge"),
                    ));
                };
                if !covered.insert(predecessor) {
                    return Err(NextUseError::new(
                        "NEXT_USE.PHI_EDGE_COVERAGE",
                        Some(block.id),
                        None,
                        vec![phi.dst, source],
                        format!("phi has duplicate sources for predecessor {predecessor_id}"),
                    ));
                }
                edge_uses[predecessor][edge].push(source);
            }
            if covered.len() != cfg.predecessors[successor].len() {
                let missing = cfg.predecessors[successor]
                    .iter()
                    .copied()
                    .find(|predecessor| !covered.contains(predecessor));
                return Err(NextUseError::new(
                    "NEXT_USE.PHI_EDGE_COVERAGE",
                    Some(block.id),
                    None,
                    vec![phi.dst],
                    missing.map_or_else(
                        || "phi source coverage differs from CFG predecessors".to_string(),
                        |predecessor| {
                            format!(
                                "phi has no source for predecessor {}",
                                func.blocks[predecessor].id
                            )
                        },
                    ),
                ));
            }
        }
    }
    Ok(edge_uses)
}

fn insert_min_distance(
    distances: &mut HashMap<VReg, NextUseDistance>,
    value: VReg,
    distance: NextUseDistance,
) {
    distances
        .entry(value)
        .and_modify(|current| *current = (*current).min(distance))
        .or_insert(distance);
}

fn verify_distance_map(
    block: BlockId,
    instruction: usize,
    boundary: &'static str,
    actual: &HashMap<VReg, NextUseDistance>,
    expected: &HashMap<VReg, NextUseDistance>,
) -> Result<(), NextUseError> {
    if actual == expected {
        return Ok(());
    }
    let values = actual
        .keys()
        .chain(expected.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let value = values
        .into_iter()
        .find(|value| actual.get(value) != expected.get(value));
    let detail = value.map_or_else(
        || "no differing value could be identified".to_string(),
        |value| {
            format!(
                "{value} has {:?}, expected {:?}",
                actual.get(&value),
                expected.get(&value)
            )
        },
    );
    Err(NextUseError::new(
        "NEXT_USE.DATAFLOW_EQUATION",
        Some(block),
        Some(instruction),
        value.into_iter().collect(),
        format!("next-use {boundary} equation failed: {detail}"),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegionShape {
    kind: LoopRegionKind,
    parent: Option<usize>,
    entries: Vec<usize>,
}

/// One forest containing both natural loops and the additional cyclic SCC
/// regions required for irreducible control flow.  Region indices are in
/// child-before-parent order, which permits bottom-up aggregation without a
/// second graph traversal.
struct RegionTopology {
    regions: Vec<RegionShape>,
    entry_region: Vec<Option<usize>>,
    direct_region: Vec<Option<usize>>,
    edge_exits: Vec<Vec<usize>>,
}

impl RegionTopology {
    fn build(cfg: &NormalizedCfg) -> Result<Self, NextUseError> {
        let nesting = LoopNesting::new(cfg)?;
        let scc = SccInfo::build(cfg);
        let natural_regions = cfg
            .loops
            .iter()
            .map(|natural_loop| natural_loop.blocks.iter().copied().collect::<Vec<_>>())
            .collect::<HashSet<_>>();

        let mut regions = cfg
            .loops
            .iter()
            .map(|natural_loop| RegionShape {
                kind: LoopRegionKind::Natural,
                parent: natural_loop.parent,
                entries: vec![natural_loop.header],
            })
            .collect::<Vec<_>>();
        let mut component_region = vec![None; scc.components.len()];
        for (component, members) in scc.components.iter().enumerate() {
            if !scc.cyclic[component] || natural_regions.contains(members) {
                continue;
            }
            let mut entries = members
                .iter()
                .copied()
                .filter(|&block| {
                    cfg.predecessors[block]
                        .iter()
                        .any(|&predecessor| scc.component_of[predecessor] != component)
                })
                .collect::<Vec<_>>();
            // A reachable cyclic SCC normally has an outside entry.  The CFG
            // entry itself is the one exception.
            if entries.is_empty() && scc.component_of.first() == Some(&component) {
                entries.push(0);
            }
            entries.sort_unstable();
            entries.dedup();
            let region = regions.len();
            component_region[component] = Some(region);
            regions.push(RegionShape {
                kind: LoopRegionKind::IrreducibleScc,
                parent: None,
                entries,
            });
        }

        // The maximal SCC contains every natural loop nested inside an
        // irreducible region.  Attach only the outermost natural loops; their
        // existing parent links already connect all descendants.
        for (natural, natural_loop) in cfg.loops.iter().enumerate() {
            if regions[natural].parent.is_some() {
                continue;
            }
            if let Some(&component) = scc.component_of.get(natural_loop.header)
                && let Some(parent) = component_region[component]
            {
                regions[natural].parent = Some(parent);
            }
        }
        if let Some((child, parent)) = regions.iter().enumerate().find_map(|(child, region)| {
            region
                .parent
                .filter(|&parent| parent <= child || parent >= regions.len())
                .map(|parent| (child, parent))
        }) {
            return Err(NextUseError::new(
                "NEXT_USE.REGION_FACTS",
                None,
                None,
                Vec::new(),
                format!("loop-region parent {parent} must follow child {child} in bottom-up order"),
            ));
        }

        // A block contributes its local summary only to its innermost natural
        // loop, or directly to its irreducible SCC when it is not in a natural
        // child.  Euler intervals answer ancestor use membership; only scalar
        // pressure is propagated by the bottom-up pass below.
        let direct_region = (0..cfg.successors.len())
            .map(|block| {
                let natural = nesting.block_loop[block];
                (natural != nesting.root)
                    .then_some(natural)
                    .or_else(|| component_region[scc.component_of[block]])
            })
            .collect::<Vec<_>>();

        let mut entry_region = vec![None; cfg.successors.len()];
        for (region, shape) in regions.iter().enumerate() {
            if shape.kind == LoopRegionKind::Natural {
                for &entry in &shape.entries {
                    entry_region[entry] = Some(region);
                }
            }
        }
        // A multi-entry SCC boundary takes precedence when one of its entries
        // also happens to be a natural-loop header.  All entries of that SCC
        // must make the same region-level W-entry decision.
        for (region, shape) in regions.iter().enumerate() {
            if shape.kind == LoopRegionKind::IrreducibleScc {
                for &entry in &shape.entries {
                    entry_region[entry] = Some(region);
                }
            }
        }

        // Number of loop regions left by each CFG edge.  Natural loops use an
        // LCA query; an extra cyclic SCC contributes one more component when
        // the edge leaves its maximal component.
        let mut edge_exits = Vec::with_capacity(cfg.successors.len());
        for (block, successors) in cfg.successors.iter().enumerate() {
            let mut block_exits = Vec::with_capacity(successors.len());
            for &successor in successors {
                let natural_exits = nesting.exits(block, successor);
                let component = scc.component_of[block];
                let Some(exits) = natural_exits.checked_add(usize::from(
                    component_region[component].is_some()
                        && component != scc.component_of[successor],
                )) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.DISTANCE_RANGE",
                        None,
                        None,
                        Vec::new(),
                        "loop-region exit count exceeds addressable CFG size",
                    ));
                };
                block_exits.push(exits);
            }
            edge_exits.push(block_exits);
        }

        Ok(Self {
            regions,
            entry_region,
            direct_region,
            edge_exits,
        })
    }
}

#[derive(Debug)]
struct BlockRegionSummary {
    used: HashSet<VReg>,
    max_pressure: usize,
}

#[derive(Debug, Default)]
struct RegionAggregationWork {
    block_scans: usize,
    instruction_visits: usize,
    indexed_block_values: usize,
    unique_use_positions: usize,
    pressure_propagations: usize,
}

/// Scan every block once to obtain both inputs of loop-region aggregation.
fn block_region_summaries(
    func: &MFunction,
    exit: &[HashMap<VReg, NextUseDistance>],
) -> Result<(Vec<BlockRegionSummary>, RegionAggregationWork), NextUseError> {
    if exit.len() != func.blocks.len() {
        return Err(NextUseError::new(
            "NEXT_USE.MODEL_SHAPE",
            None,
            None,
            Vec::new(),
            format!(
                "next-use exit table contains {} rows for {} MIR blocks",
                exit.len(),
                func.blocks.len()
            ),
        ));
    }
    let mut work = RegionAggregationWork::default();
    let mut summaries = Vec::with_capacity(func.blocks.len());
    for (block, mir_block) in func.blocks.iter().enumerate() {
        work.block_scans = work.block_scans.checked_add(1).ok_or_else(|| {
            NextUseError::new(
                "NEXT_USE.WORK_RANGE",
                Some(mir_block.id),
                None,
                Vec::new(),
                "block-summary work exceeds addressable size",
            )
        })?;
        work.instruction_visits = work
            .instruction_visits
            .checked_add(mir_block.insts.len())
            .ok_or_else(|| {
                NextUseError::new(
                    "NEXT_USE.WORK_RANGE",
                    Some(mir_block.id),
                    None,
                    Vec::new(),
                    "instruction-summary work exceeds addressable size",
                )
            })?;
        let mut used = HashSet::new();
        for phi in &mir_block.phis {
            used.insert(phi.dst);
            used.extend(phi.sources.iter().map(|(_, source)| *source));
        }
        let mut live = exit[block].keys().copied().collect::<HashSet<_>>();
        let mut maximum = live.len();
        for inst in mir_block.insts.iter().rev() {
            let instruction_uses = inst.uses();
            used.extend(instruction_uses.iter().copied());
            if let Some(definition) = inst.def() {
                live.remove(&definition);
            }
            live.extend(instruction_uses);
            maximum = maximum.max(live.len());
        }
        summaries.push(BlockRegionSummary {
            used,
            max_pressure: maximum,
        });
    }
    Ok((summaries, work))
}

/// Flat value-to-direct-region index.  A region's descendants occupy one Euler
/// interval, so membership needs no set copied into an ancestor.
#[derive(Debug, PartialEq, Eq)]
struct RegionUseIndex {
    offsets: Vec<usize>,
    positions: Vec<usize>,
    region_start: Vec<usize>,
    region_end: Vec<usize>,
}

impl RegionUseIndex {
    fn build(
        value_count: usize,
        topology: &RegionTopology,
        blocks: &[BlockRegionSummary],
        work: &mut RegionAggregationWork,
    ) -> Result<Self, NextUseError> {
        if topology.direct_region.len() != blocks.len() {
            return Err(NextUseError::new(
                "NEXT_USE.MODEL_SHAPE",
                None,
                None,
                Vec::new(),
                "loop-region ownership does not cover every block summary",
            ));
        }
        let (region_start, region_end) = region_euler_intervals(&topology.regions)?;
        let mut counts = vec![0usize; value_count];
        for (block, summary) in blocks.iter().enumerate() {
            if topology.direct_region[block].is_none() {
                continue;
            }
            for &value in &summary.used {
                let raw_value = value;
                let value = raw_value.0 as usize;
                if value >= value_count {
                    return Err(NextUseError::new(
                        "NEXT_USE.VALUE_RANGE",
                        None,
                        None,
                        vec![raw_value],
                        format!(
                            "loop-region use references v{} but only {value_count} virtual registers exist",
                            raw_value.0
                        ),
                    ));
                }
                counts[value] = counts[value].checked_add(1).ok_or_else(|| {
                    NextUseError::new(
                        "NEXT_USE.WORK_RANGE",
                        None,
                        None,
                        vec![raw_value],
                        "region-use index exceeds addressable size",
                    )
                })?;
                work.indexed_block_values =
                    work.indexed_block_values.checked_add(1).ok_or_else(|| {
                        NextUseError::new(
                            "NEXT_USE.WORK_RANGE",
                            None,
                            None,
                            vec![raw_value],
                            "region-use work exceeds addressable size",
                        )
                    })?;
            }
        }
        let mut offsets = vec![0usize; value_count + 1];
        for value in 0..value_count {
            offsets[value + 1] = offsets[value].checked_add(counts[value]).ok_or_else(|| {
                NextUseError::new(
                    "NEXT_USE.WORK_RANGE",
                    None,
                    None,
                    vec![VReg(value as u32)],
                    "region-use index exceeds addressable size",
                )
            })?;
        }
        let mut positions = vec![0usize; offsets[value_count]];
        let mut cursors = offsets[..value_count].to_vec();
        for (block, summary) in blocks.iter().enumerate() {
            let Some(region) = topology.direct_region[block] else {
                continue;
            };
            let Some(&position) = region_start.get(region) else {
                return Err(NextUseError::new(
                    "NEXT_USE.REGION_FACTS",
                    None,
                    None,
                    Vec::new(),
                    format!("block {block} references absent loop region {region}"),
                ));
            };
            for &value in &summary.used {
                let value = value.0 as usize;
                positions[cursors[value]] = position;
                cursors[value] += 1;
            }
        }
        for value in 0..value_count {
            positions[offsets[value]..offsets[value + 1]].sort_unstable();
        }

        // Multiple blocks in one direct region produce one index entry.  The
        // compaction is in-place and `write <= read`, so peak storage remains
        // linear in block-local use occurrences.
        let raw_offsets = offsets;
        let mut offsets = vec![0usize; value_count + 1];
        let mut write = 0usize;
        for value in 0..value_count {
            offsets[value] = write;
            let mut previous = None;
            for read in raw_offsets[value]..raw_offsets[value + 1] {
                let position = positions[read];
                if previous == Some(position) {
                    continue;
                }
                positions[write] = position;
                write += 1;
                previous = Some(position);
            }
        }
        offsets[value_count] = write;
        positions.truncate(write);
        work.unique_use_positions =
            work.unique_use_positions
                .checked_add(write)
                .ok_or_else(|| {
                    NextUseError::new(
                        "NEXT_USE.WORK_RANGE",
                        None,
                        None,
                        Vec::new(),
                        "region-use work exceeds addressable size",
                    )
                })?;
        Ok(Self {
            offsets,
            positions,
            region_start,
            region_end,
        })
    }

    fn contains(&self, value: VReg, start: usize, end: usize) -> bool {
        let value = value.0 as usize;
        let Some((&begin, &finish)) = self.offsets.get(value).zip(self.offsets.get(value + 1))
        else {
            return false;
        };
        let positions = &self.positions[begin..finish];
        let candidate = positions.partition_point(|position| *position < start);
        positions
            .get(candidate)
            .is_some_and(|position| *position < end)
    }
}

/// Iterative DFS numbering of the region forest.  Every subtree is a half-open
/// interval, including an irreducible SCC and its natural-loop children.
fn region_euler_intervals(
    regions: &[RegionShape],
) -> Result<(Vec<usize>, Vec<usize>), NextUseError> {
    let mut children = vec![Vec::new(); regions.len()];
    let mut roots = Vec::new();
    for (region, shape) in regions.iter().enumerate() {
        if let Some(parent) = shape.parent {
            if parent >= regions.len() || parent <= region {
                return Err(NextUseError::new(
                    "NEXT_USE.REGION_FACTS",
                    None,
                    None,
                    Vec::new(),
                    format!(
                        "loop-region parent {parent} must be a later valid region than child {region}"
                    ),
                ));
            }
            children[parent].push(region);
        } else {
            roots.push(region);
        }
    }
    enum Event {
        Enter(usize),
        Exit(usize),
    }
    let mut start = vec![usize::MAX; regions.len()];
    let mut end = vec![usize::MAX; regions.len()];
    let mut next = 0usize;
    let mut stack = roots
        .into_iter()
        .rev()
        .map(Event::Enter)
        .collect::<Vec<_>>();
    while let Some(event) = stack.pop() {
        match event {
            Event::Enter(region) => {
                if start[region] != usize::MAX {
                    return Err(NextUseError::new(
                        "NEXT_USE.REGION_FACTS",
                        None,
                        None,
                        Vec::new(),
                        format!("loop-region forest revisits region {region}"),
                    ));
                }
                start[region] = next;
                next = next.checked_add(1).ok_or_else(|| {
                    NextUseError::new(
                        "NEXT_USE.WORK_RANGE",
                        None,
                        None,
                        Vec::new(),
                        "loop-region count exceeds addressable size",
                    )
                })?;
                stack.push(Event::Exit(region));
                stack.extend(children[region].iter().rev().copied().map(Event::Enter));
            }
            Event::Exit(region) => end[region] = next,
        }
    }
    if next != regions.len() {
        return Err(NextUseError::new(
            "NEXT_USE.REGION_FACTS",
            None,
            None,
            Vec::new(),
            "loop-region forest is disconnected",
        ));
    }
    Ok((start, end))
}

/// Propagate only scalar pressure through the region forest.  Unlike a used
/// set, one scalar per edge cannot grow quadratically with nesting depth.
fn aggregate_region_pressure(
    topology: &RegionTopology,
    blocks: &[BlockRegionSummary],
    work: &mut RegionAggregationWork,
) -> Result<Vec<usize>, NextUseError> {
    if topology.direct_region.len() != blocks.len() {
        return Err(NextUseError::new(
            "NEXT_USE.MODEL_SHAPE",
            None,
            None,
            Vec::new(),
            "loop-region ownership does not cover every pressure summary",
        ));
    }
    let mut pressure = vec![0usize; topology.regions.len()];
    for (block, summary) in blocks.iter().enumerate() {
        let Some(region) = topology.direct_region[block] else {
            continue;
        };
        pressure[region] = pressure[region].max(summary.max_pressure);
    }
    for child in 0..topology.regions.len() {
        let Some(parent) = topology.regions[child].parent else {
            continue;
        };
        if parent <= child || parent >= pressure.len() {
            return Err(NextUseError::new(
                "NEXT_USE.REGION_FACTS",
                None,
                None,
                Vec::new(),
                format!("loop-region parent {parent} must follow child {child} in bottom-up order"),
            ));
        }
        pressure[parent] = pressure[parent].max(pressure[child]);
        work.pressure_propagations =
            work.pressure_propagations.checked_add(1).ok_or_else(|| {
                NextUseError::new(
                    "NEXT_USE.WORK_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "loop-region pressure work exceeds addressable size",
                )
            })?;
    }
    Ok(pressure)
}

struct LoopNesting {
    root: usize,
    block_loop: Vec<usize>,
    depth: Vec<usize>,
    ancestors: Vec<Vec<usize>>,
}

impl LoopNesting {
    fn new(cfg: &NormalizedCfg) -> Result<Self, NextUseError> {
        let root = cfg.loops.len();
        let nodes = root.checked_add(1).ok_or_else(|| {
            NextUseError::new(
                "NEXT_USE.WORK_RANGE",
                None,
                None,
                Vec::new(),
                "loop nesting exceeds addressable CFG size",
            )
        })?;
        let mut parent = cfg
            .loops
            .iter()
            .map(|natural_loop| natural_loop.parent.unwrap_or(root))
            .chain(std::iter::once(root))
            .collect::<Vec<_>>();
        parent[root] = root;

        for (child, &parent) in parent.iter().take(root).enumerate() {
            if parent > root || (parent != root && parent <= child) {
                return Err(NextUseError::new(
                    "NEXT_USE.REGION_FACTS",
                    None,
                    None,
                    Vec::new(),
                    format!(
                        "natural-loop parent {parent} must be a later valid loop than child {child}"
                    ),
                ));
            }
        }

        let mut depth = vec![usize::MAX; nodes];
        depth[root] = 0;
        for start in 0..root {
            if depth[start] != usize::MAX {
                continue;
            }
            let mut path = Vec::new();
            let mut node = start;
            while depth[node] == usize::MAX {
                path.push(node);
                node = parent[node];
            }
            let mut current_depth = depth[node];
            for node in path.into_iter().rev() {
                current_depth = current_depth.checked_add(1).ok_or_else(|| {
                    NextUseError::new(
                        "NEXT_USE.WORK_RANGE",
                        None,
                        None,
                        Vec::new(),
                        "loop nesting exceeds addressable CFG size",
                    )
                })?;
                depth[node] = current_depth;
            }
        }

        let levels = (usize::BITS - nodes.leading_zeros()) as usize;
        let mut ancestors = Vec::with_capacity(levels);
        ancestors.push(parent);
        for level in 1..levels {
            let previous = &ancestors[level - 1];
            ancestors.push(previous.iter().map(|&node| previous[node]).collect());
        }

        // cfg.loops is ordered from inner/smaller regions to outer regions.
        let mut block_loop = vec![root; cfg.successors.len()];
        for (loop_index, natural_loop) in cfg.loops.iter().enumerate() {
            for &block in &natural_loop.blocks {
                if block >= block_loop.len() {
                    return Err(NextUseError::new(
                        "NEXT_USE.REGION_FACTS",
                        None,
                        None,
                        Vec::new(),
                        format!(
                            "natural loop {loop_index} contains out-of-range block index {block}"
                        ),
                    ));
                }
                if block_loop[block] == root {
                    block_loop[block] = loop_index;
                }
            }
        }
        Ok(Self {
            root,
            block_loop,
            depth,
            ancestors,
        })
    }

    fn exits(&self, block: usize, successor: usize) -> usize {
        let source_loop = self.block_loop[block];
        let common = self.lca(source_loop, self.block_loop[successor]);
        self.depth[source_loop] - self.depth[common]
    }

    fn lca(&self, mut left: usize, mut right: usize) -> usize {
        if left == self.root || right == self.root {
            return self.root;
        }
        if self.depth[left] < self.depth[right] {
            std::mem::swap(&mut left, &mut right);
        }
        let difference = self.depth[left] - self.depth[right];
        for level in 0..self.ancestors.len() {
            if difference & (1usize << level) != 0 {
                left = self.ancestors[level][left];
            }
        }
        if left == right {
            return left;
        }
        for level in (0..self.ancestors.len()).rev() {
            if self.ancestors[level][left] != self.ancestors[level][right] {
                left = self.ancestors[level][left];
                right = self.ancestors[level][right];
            }
        }
        self.ancestors[0][left]
    }
}

struct SccInfo {
    component_of: Vec<usize>,
    components: Vec<Vec<usize>>,
    cyclic: Vec<bool>,
}

impl SccInfo {
    /// Iterative Kosaraju decomposition; large branchified functions must not
    /// consume the native call stack while discovering irreducible regions.
    fn build(cfg: &NormalizedCfg) -> Self {
        let blocks = cfg.successors.len();
        let mut visited = vec![false; blocks];
        let mut finish = Vec::with_capacity(blocks);
        for start in 0..blocks {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![(start, 0usize)];
            while let Some((block, next_successor)) = stack.pop() {
                if next_successor == cfg.successors[block].len() {
                    finish.push(block);
                    continue;
                }
                stack.push((block, next_successor + 1));
                let successor = cfg.successors[block][next_successor];
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            }
        }

        let mut component_of = vec![usize::MAX; blocks];
        let mut components = Vec::<Vec<usize>>::new();
        for &start in finish.iter().rev() {
            if component_of[start] != usize::MAX {
                continue;
            }
            let component = components.len();
            let mut members = Vec::new();
            let mut stack = vec![start];
            component_of[start] = component;
            while let Some(block) = stack.pop() {
                members.push(block);
                for &predecessor in &cfg.predecessors[block] {
                    if component_of[predecessor] == usize::MAX {
                        component_of[predecessor] = component;
                        stack.push(predecessor);
                    }
                }
            }
            members.sort_unstable();
            components.push(members);
        }
        let cyclic = components
            .iter()
            .map(|members| {
                members.len() > 1
                    || cfg.successors[members[0]]
                        .iter()
                        .any(|successor| *successor == members[0])
            })
            .collect();
        Self {
            component_of,
            components,
            cyclic,
        }
    }
}

struct BlockTransfer {
    length: usize,
    definitions: HashSet<VReg>,
    local_uses: Vec<(VReg, usize)>,
}

fn block_transfers(func: &MFunction) -> Vec<BlockTransfer> {
    func.blocks
        .iter()
        .map(|block| {
            let mut definitions = block.phis.iter().map(|phi| phi.dst).collect::<HashSet<_>>();
            let mut local_uses = HashMap::<VReg, usize>::new();
            for (position, inst) in block.insts.iter().enumerate() {
                for used in inst.uses() {
                    if !definitions.contains(&used) {
                        local_uses.entry(used).or_insert(position);
                    }
                }
                if let Some(definition) = inst.def() {
                    definitions.insert(definition);
                }
            }
            let mut local_uses = local_uses.into_iter().collect::<Vec<_>>();
            local_uses.sort_by_key(|(value, _)| *value);
            BlockTransfer {
                length: block.insts.len(),
                definitions,
                local_uses,
            }
        })
        .collect()
}

fn phi_edge_uses(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<Vec<Vec<Vec<VReg>>>, NextUseError> {
    let mut edge_uses = Vec::with_capacity(cfg.successors.len());
    for (predecessor, successors) in cfg.successors.iter().enumerate() {
        let Some(predecessor_block) = func.blocks.get(predecessor) else {
            return Err(NextUseError::new(
                "NEXT_USE.MODEL_SHAPE",
                None,
                None,
                Vec::new(),
                format!("CFG predecessor index {predecessor} is outside the MIR function"),
            ));
        };
        let mut predecessor_uses = Vec::with_capacity(successors.len());
        for &successor in successors {
            let Some(successor_block) = func.blocks.get(successor) else {
                return Err(NextUseError::new(
                    "NEXT_USE.MODEL_SHAPE",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("CFG successor index {successor} is outside the MIR function"),
                ));
            };
            let mut uses = Vec::with_capacity(successor_block.phis.len());
            for phi in &successor_block.phis {
                let Some(source) = phi.sources.iter().find_map(|(source_predecessor, source)| {
                    (*source_predecessor == predecessor_block.id).then_some(*source)
                }) else {
                    return Err(NextUseError::new(
                        "NEXT_USE.PHI_EDGE_COVERAGE",
                        Some(successor_block.id),
                        None,
                        vec![phi.dst],
                        format!(
                            "phi destination v{} has no source for predecessor {}",
                            phi.dst.0, predecessor_block.id
                        ),
                    ));
                };
                uses.push(source);
            }
            predecessor_uses.push(uses);
        }
        edge_uses.push(predecessor_uses);
    }
    Ok(edge_uses)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::backend::native::mir::{BlockId, MBlock, MInst, PhiNode, SpillDesc, VRegAllocator};

    #[test]
    fn missing_phi_edge_source_is_a_structured_analysis_error() {
        let mut vregs = VRegAllocator::new();
        let destination = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis.push(PhiNode {
            dst: destination,
            sources: Vec::new(),
        });
        successor.push(MInst::Return);
        func.blocks = vec![predecessor, successor];
        let cfg = NormalizedCfg {
            block_index: HashMap::from([(BlockId(0), 0), (BlockId(1), 1)]),
            predecessors: vec![vec![], vec![0]],
            successors: vec![vec![1], vec![]],
            idom: vec![None, Some(0)],
            dominance_frontier: vec![BTreeSet::new(); 2],
            loops: Vec::new(),
            loop_for_header: HashMap::new(),
        };

        let error = analyze(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.PHI_EDGE_COVERAGE");
        assert_eq!(error.block, Some(BlockId(1)));
        assert_eq!(error.values, vec![destination]);
    }

    #[test]
    fn stale_block_table_is_a_structured_error() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut analysis = analyze(&func, &cfg).unwrap();
        analysis.entry.pop();

        let error = analysis.verify(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.MODEL_SHAPE");
        assert_eq!(error.block, None);
    }

    #[test]
    fn repeated_operand_has_one_instruction_position() {
        let mut vregs = VRegAllocator::new();
        let input = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: input,
            value: 1,
        });
        block.push(MInst::Add {
            dst: output,
            lhs: input,
            rhs: input,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();

        let analysis = analyze(&func, &cfg).unwrap();

        analysis.verify(&func, &cfg).unwrap();
        assert_eq!(analysis.use_positions[0][&input], vec![1]);
    }

    #[test]
    fn duplicate_operand_position_is_a_structured_verifier_error() {
        let mut vregs = VRegAllocator::new();
        let input = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: input,
            value: 1,
        });
        block.push(MInst::Add {
            dst: output,
            lhs: input,
            rhs: input,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut analysis = analyze(&func, &cfg).unwrap();
        analysis.use_positions[0].get_mut(&input).unwrap().push(1);

        let error = analysis.verify(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.USE_POSITIONS_MATCH_MIR");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(1));
        assert_eq!(error.values, vec![input]);
    }

    #[test]
    fn stale_exit_value_is_a_structured_dataflow_error() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut analysis = analyze(&func, &cfg).unwrap();
        analysis.exit[0].insert(value, NextUseDistance::local(0));

        let error = analysis.verify(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.values, vec![value]);
    }

    #[test]
    fn stale_entry_distance_is_a_structured_dataflow_error() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let used = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.push(MInst::Mov {
            dst: used,
            src: value,
        });
        successor.push(MInst::Return);
        func.blocks = vec![entry, successor];
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let predecessor = cfg.block_index[&BlockId(0)];
        let successor = cfg.block_index[&BlockId(1)];
        let mut analysis = analyze(&func, &cfg).unwrap();
        analysis.entry[successor].insert(value, NextUseDistance::local(1));
        analysis.exit[predecessor].insert(value, NextUseDistance::local(1));

        let error = analysis.verify(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
        assert_eq!(error.block, Some(BlockId(1)));
        assert_eq!(error.instruction, Some(0));
        assert_eq!(error.values, vec![value]);
    }

    #[test]
    fn missing_phi_edge_distance_is_a_structured_dataflow_error() {
        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let destination = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: source,
            value: 1,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis.push(PhiNode {
            dst: destination,
            sources: vec![(BlockId(0), source)],
        });
        successor.push(MInst::Return);
        func.blocks = vec![predecessor, successor];
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let predecessor = cfg.block_index[&BlockId(0)];
        let mut analysis = analyze(&func, &cfg).unwrap();
        analysis.exit[predecessor].remove(&source);

        let error = analysis.verify(&func, &cfg).unwrap_err();

        assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.values, vec![source]);
    }

    #[test]
    fn irreducible_cyclic_scc_exit_is_a_loop_component() {
        // Blocks 1 and 2 form a cyclic SCC with two entries from block 0, so
        // neither is a natural-loop header.  Exiting the SCC must nevertheless
        // increment the lexicographic loop component.
        let cfg = NormalizedCfg {
            block_index: HashMap::new(),
            predecessors: vec![vec![], vec![0, 2], vec![0, 1], vec![1, 2]],
            successors: vec![vec![1, 2], vec![2, 3], vec![1, 3], vec![]],
            idom: vec![None, Some(0), Some(0), Some(0)],
            dominance_frontier: vec![BTreeSet::new(); 4],
            loops: Vec::new(),
            loop_for_header: HashMap::new(),
        };
        let topology = RegionTopology::build(&cfg).unwrap();
        assert_eq!(topology.edge_exits[1], vec![0, 1]);
        assert_eq!(topology.edge_exits[2], vec![0, 1]);
        assert_eq!(topology.regions.len(), 1);
        assert_eq!(topology.regions[0].kind, LoopRegionKind::IrreducibleScc);
        assert_eq!(topology.regions[0].entries, vec![1, 2]);
        assert_eq!(topology.entry_region[1], Some(0));
        assert_eq!(topology.entry_region[2], Some(0));
    }

    #[test]
    fn nested_region_facts_scan_blocks_once_and_aggregate_bottom_up() {
        let mut vregs = VRegAllocator::new();
        let outer_value = vregs.alloc();
        let inner_value = vregs.alloc();
        let outer_condition = vregs.alloc();
        let inner_condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: outer_value,
            value: 10,
        });
        entry.push(MInst::LoadImm {
            dst: inner_value,
            value: 20,
        });
        entry.push(MInst::LoadImm {
            dst: outer_condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: inner_condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut outer_header = MBlock::new(BlockId(1));
        outer_header.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 0,
            src: outer_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        outer_header.push(MInst::Branch {
            cond: outer_condition,
            true_bb: BlockId(2),
            false_bb: BlockId(5),
        });

        let mut inner_header = MBlock::new(BlockId(2));
        inner_header.push(MInst::Branch {
            cond: inner_condition,
            true_bb: BlockId(3),
            false_bb: BlockId(4),
        });
        let mut inner_body = MBlock::new(BlockId(3));
        inner_body.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 8,
            src: inner_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        inner_body.push(MInst::Jump { target: BlockId(2) });
        let mut outer_latch = MBlock::new(BlockId(4));
        outer_latch.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(5));
        exit.push(MInst::Return);
        func.blocks = vec![
            entry,
            outer_header,
            inner_header,
            inner_body,
            outer_latch,
            exit,
        ];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let analysis = analyze(&func, &cfg).unwrap();
        analysis.verify(&func, &cfg).unwrap();
        let outer_header = cfg.block_index[&BlockId(1)];
        let inner_header = cfg.block_index[&BlockId(2)];
        let outer_region = analysis.region_at_entry(outer_header).unwrap();
        let inner_region = analysis.region_at_entry(inner_header).unwrap();
        assert_ne!(outer_region, inner_region);
        let outer = &analysis.loop_regions[outer_region];
        let inner = &analysis.loop_regions[inner_region];
        assert_eq!(outer.kind, LoopRegionKind::Natural);
        assert_eq!(inner.kind, LoopRegionKind::Natural);
        assert!(analysis.used_in_region(inner_region, inner_value));
        assert!(!analysis.used_in_region(inner_region, outer_value));
        assert!(analysis.used_in_region(outer_region, inner_value));
        assert!(analysis.used_in_region(outer_region, outer_value));
        for (header, facts) in [(outer_header, outer), (inner_header, inner)] {
            let natural_loop = cfg
                .loops
                .iter()
                .find(|natural_loop| natural_loop.header == header)
                .unwrap();
            assert_eq!(
                facts.max_pressure,
                natural_loop
                    .blocks
                    .iter()
                    .map(|&block| analysis.block_max_pressure[block])
                    .max()
                    .unwrap()
            );
        }
        assert_eq!(analysis.region_work.block_scans, func.blocks.len());
        assert_eq!(
            analysis.region_work.instruction_visits,
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>()
        );
        assert_eq!(analysis.region_work.indexed_block_values, 4);
        assert_eq!(analysis.region_work.unique_use_positions, 4);
        assert_eq!(analysis.region_work.pressure_propagations, 1);
    }

    #[test]
    fn deep_nesting_does_not_copy_value_membership_to_ancestors() {
        const DEPTH: usize = 20_000;
        const VALUES: usize = 20_000;

        let regions = (0..DEPTH)
            .map(|region| RegionShape {
                kind: LoopRegionKind::Natural,
                parent: (region + 1 < DEPTH).then_some(region + 1),
                entries: Vec::new(),
            })
            .collect::<Vec<_>>();
        let topology = RegionTopology {
            regions,
            entry_region: vec![Some(0)],
            direct_region: vec![Some(0)],
            edge_exits: vec![Vec::new()],
        };
        let blocks = vec![BlockRegionSummary {
            used: (0..VALUES as u32).map(VReg).collect(),
            max_pressure: VALUES,
        }];
        let mut work = RegionAggregationWork::default();
        let uses = RegionUseIndex::build(VALUES, &topology, &blocks, &mut work).unwrap();
        let pressure = aggregate_region_pressure(&topology, &blocks, &mut work).unwrap();

        // One direct occurrence per value is sufficient for every ancestor
        // query.  A complete-set representation would require DEPTH * VALUES
        // memberships (400 million for this fixture).
        assert_eq!(uses.positions.len(), VALUES);
        assert_eq!(work.indexed_block_values, VALUES);
        assert_eq!(work.unique_use_positions, VALUES);
        assert_eq!(work.pressure_propagations, DEPTH - 1);
        assert!(pressure.iter().all(|maximum| *maximum == VALUES));
        for region in [0, DEPTH / 2, DEPTH - 1] {
            for value in [VReg(0), VReg(VALUES as u32 - 1)] {
                assert!(uses.contains(value, uses.region_start[region], uses.region_end[region]));
            }
        }
        assert!(!uses.contains(
            VReg(VALUES as u32),
            uses.region_start[DEPTH - 1],
            uses.region_end[DEPTH - 1]
        ));
    }

    #[test]
    fn loop_exit_use_is_farther_than_loop_use() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let condition = vregs.alloc();
        let inside = vregs.alloc();
        let outside = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Add {
            dst: inside,
            lhs: condition,
            rhs: condition,
        });
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Add {
            dst: outside,
            lhs: value,
            rhs: value,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, exit];
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut analysis = analyze(&func, &cfg).unwrap();
        let header = cfg.block_index[&BlockId(1)];
        analysis.verify(&func, &cfg).unwrap();
        assert!(matches!(
            analysis.exit[header][&value],
            NextUseDistance::Finite {
                loop_exits: 1..,
                ..
            }
        ));

        let NextUseDistance::Finite { instructions, .. } = analysis.exit[header][&value] else {
            unreachable!("loop-exit value must have a finite distance")
        };
        analysis.exit[header].insert(
            value,
            NextUseDistance::Finite {
                loop_exits: 0,
                instructions,
            },
        );
        let error = analysis.verify(&func, &cfg).unwrap_err();
        assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
        assert_eq!(error.block, Some(BlockId(1)));
        assert_eq!(error.values, vec![value]);
    }

    #[test]
    fn next_iteration_use_beats_exit_use_after_more_than_100k_instructions() {
        const LOOP_BODY: usize = 100_001;

        let mut vregs = VRegAllocator::new();
        let inside_value = vregs.alloc();
        let exit_value = vregs.alloc();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: inside_value,
            value: 11,
        });
        entry.push(MInst::LoadImm {
            dst: exit_value,
            value: 22,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut header = MBlock::new(BlockId(1));
        header.insts.extend((0..LOOP_BODY).map(|_| MInst::MemCopy {
            src_offset: 0,
            dst_offset: 8,
            byte_len: 8,
        }));
        header.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 0,
            src: inside_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 8,
            src: exit_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, exit];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let analysis = analyze(&func, &cfg).unwrap();
        let header = cfg.block_index[&BlockId(1)];
        // Query immediately after this iteration's inside-loop use.  Its next
        // occurrence is in the next iteration, beyond the 100,001-instruction
        // body; the other value is used immediately after leaving the loop.
        let point_after_inside_use = LOOP_BODY + 1;
        let inside = analysis.distance_at(&func, header, point_after_inside_use, inside_value);
        let outside = analysis.distance_at(&func, header, point_after_inside_use, exit_value);

        assert!(
            matches!(
                inside,
                NextUseDistance::Finite {
                    loop_exits: 0,
                    instructions
                } if instructions > 100_000
            ),
            "inside-loop distance was {inside:?}"
        );
        assert!(
            matches!(
                outside,
                NextUseDistance::Finite {
                    loop_exits: 1..,
                    ..
                }
            ),
            "loop-exit distance was {outside:?}"
        );
        assert!(
            inside < outside,
            "lexicographic distance must prefer the next loop iteration: inside={inside:?}, outside={outside:?}"
        );
    }
}
