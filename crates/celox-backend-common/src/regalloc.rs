//! Register-allocation data types and target-independent allocation helpers.

use std::collections::VecDeque;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::hash::Hash;

use fxhash::{FxHashMap, FxHashSet};

mod live_interval;
mod parallel_copy;
mod stack_color;

pub use live_interval::{
    LiveInterval, LiveIntervalError, LiveIntervals, LiveSegment, analyze_live_intervals,
};
pub use parallel_copy::{
    CopyDestination, CopyOperation, CopyResolution, CopyResolutionError, CopyResolutionWork,
    CopySource, ParallelCopy, resolve_parallel_copies,
};
pub use stack_color::{StackColorError, StackSlotColoring, color_stack_slots};

/// A physical register that can be stored in a compact register set.
pub trait MachineRegister: Copy + Eq + Hash + Ord + fmt::Debug {
    /// Stable target-defined register number in the range `0..64`.
    fn index(self) -> u8;
}

/// Compact set for targets with at most 64 physical registers per class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegisterSet(u64);

impl RegisterSet {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn insert<R: MachineRegister>(&mut self, register: R) {
        self.0 |= register_bit(register);
    }

    pub fn remove<R: MachineRegister>(&mut self, register: R) {
        self.0 &= !register_bit(register);
    }

    pub fn contains<R: MachineRegister>(&self, register: &R) -> bool {
        self.0 & register_bit(*register) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

fn register_bit<R: MachineRegister>(register: R) -> u64 {
    1_u64
        .checked_shl(u32::from(register.index()))
        .expect("physical register index must be below 64")
}

/// Constraint on one machine-instruction operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegConstraint<R> {
    Any,
    Fixed(R),
}

/// Physical location used while lowering SSA edge transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueLocation<R> {
    Register(R),
    Stack(i32),
    Immediate(u64),
}

/// Target constraints attached to one instruction after legalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionConstraints<V, R> {
    pub fixed_uses: Vec<(V, R)>,
    pub fixed_defs: Vec<(V, R)>,
    pub clobbers: Vec<R>,
}

impl<V, R> Default for InstructionConstraints<V, R> {
    fn default() -> Self {
        Self {
            fixed_uses: Vec::new(),
            fixed_defs: Vec::new(),
            clobbers: Vec::new(),
        }
    }
}

/// Register-allocation facts for one target-owned machine instruction.
///
/// This deliberately contains no opcode. Backends lower their own MIR into
/// these facts so allocation algorithms do not need a common machine IR or a
/// target instruction enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionAllocationFacts<V, R> {
    pub uses: Vec<V>,
    pub defs: Vec<V>,
    pub constraints: InstructionConstraints<V, R>,
    /// Hint used by allocators which perform copy coalescing.
    pub is_copy: bool,
}

impl<V, R> Default for InstructionAllocationFacts<V, R> {
    fn default() -> Self {
        Self {
            uses: Vec::new(),
            defs: Vec::new(),
            constraints: InstructionConstraints::default(),
            is_copy: false,
        }
    }
}

/// One source of a target-MIR phi node, identified by normalized predecessor
/// block index rather than a backend-specific block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhiSource<V> {
    pub predecessor: usize,
    pub value: V,
}

/// Register-allocation facts for one target-MIR phi node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhiAllocationFacts<V> {
    pub destination: V,
    pub sources: Vec<PhiSource<V>>,
}

/// Register-allocation facts for one normalized target-MIR basic block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAllocationFacts<V, R> {
    pub successors: Vec<usize>,
    pub phis: Vec<PhiAllocationFacts<V>>,
    pub instructions: Vec<InstructionAllocationFacts<V, R>>,
}

/// Target-independent input boundary for register-allocation analyses.
///
/// x86 and AArch64 retain separate MIR instruction types and optimization
/// pipelines. Each backend projects its final virtual-register MIR into this
/// representation immediately before allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionAllocationFacts<V, R> {
    pub entry: usize,
    pub blocks: Vec<BlockAllocationFacts<V, R>>,
}

/// Structural error in backend-provided register-allocation facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationFactsError<V> {
    MissingEntry {
        entry: usize,
        block_count: usize,
    },
    MissingSuccessor {
        block: usize,
        successor: usize,
        block_count: usize,
    },
    MissingPhiPredecessor {
        block: usize,
        predecessor: usize,
        value: V,
    },
    FixedUseIsNotUse {
        block: usize,
        instruction: usize,
        value: V,
    },
    FixedDefIsNotDef {
        block: usize,
        instruction: usize,
        value: V,
    },
}

impl<V> fmt::Display for AllocationFactsError<V>
where
    V: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry { entry, block_count } => write!(
                formatter,
                "allocation entry block {entry} is outside {block_count} blocks"
            ),
            Self::MissingSuccessor {
                block,
                successor,
                block_count,
            } => write!(
                formatter,
                "allocation block {block} has successor {successor} outside {block_count} blocks"
            ),
            Self::MissingPhiPredecessor {
                block,
                predecessor,
                value,
            } => write!(
                formatter,
                "allocation phi for {value:?} in block {block} names non-predecessor block {predecessor}"
            ),
            Self::FixedUseIsNotUse {
                block,
                instruction,
                value,
            } => write!(
                formatter,
                "fixed-use value {value:?} is not a use of allocation block {block} instruction {instruction}"
            ),
            Self::FixedDefIsNotDef {
                block,
                instruction,
                value,
            } => write!(
                formatter,
                "fixed-def value {value:?} is not a definition of allocation block {block} instruction {instruction}"
            ),
        }
    }
}

impl<V> std::error::Error for AllocationFactsError<V> where V: fmt::Debug {}

impl<V, R> FunctionAllocationFacts<V, R>
where
    V: Copy + Eq + fmt::Debug,
{
    /// Verify only target-independent shape invariants. Opcode legality and
    /// register-class rules remain the responsibility of the owning backend.
    pub fn verify(&self) -> Result<(), AllocationFactsError<V>> {
        let block_count = self.blocks.len();
        if self.entry >= block_count {
            return Err(AllocationFactsError::MissingEntry {
                entry: self.entry,
                block_count,
            });
        }

        let mut predecessors = vec![Vec::new(); block_count];
        for (block_index, block) in self.blocks.iter().enumerate() {
            for &successor in &block.successors {
                let Some(successor_predecessors) = predecessors.get_mut(successor) else {
                    return Err(AllocationFactsError::MissingSuccessor {
                        block: block_index,
                        successor,
                        block_count,
                    });
                };
                successor_predecessors.push(block_index);
            }
        }

        for (block_index, block) in self.blocks.iter().enumerate() {
            for phi in &block.phis {
                for source in &phi.sources {
                    if !predecessors[block_index].contains(&source.predecessor) {
                        return Err(AllocationFactsError::MissingPhiPredecessor {
                            block: block_index,
                            predecessor: source.predecessor,
                            value: source.value,
                        });
                    }
                }
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                for &(value, _) in &instruction.constraints.fixed_uses {
                    if !instruction.uses.contains(&value) {
                        return Err(AllocationFactsError::FixedUseIsNotUse {
                            block: block_index,
                            instruction: instruction_index,
                            value,
                        });
                    }
                }
                for &(value, _) in &instruction.constraints.fixed_defs {
                    if !instruction.defs.contains(&value) {
                        return Err(AllocationFactsError::FixedDefIsNotDef {
                            block: block_index,
                            instruction: instruction_index,
                            value,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Next-use distances at target-MIR block boundaries.
///
/// Distances are measured in target instructions. Loop-exit paths receive a
/// deliberately large penalty so an allocator prefers uses within the loop.
#[derive(Debug, Clone)]
pub struct NextUseAnalysis<V> {
    pub entry_distances: Vec<FxHashMap<V, u32>>,
    pub exit_distances: Vec<FxHashMap<V, u32>>,
    pub predecessors: Vec<Vec<usize>>,
    pub backedge_successors: Vec<Vec<usize>>,
}

/// Large edge length for a DFS backedge. This matches the established native
/// allocator's heuristic and keeps a use behind a loop farther away than uses
/// in the loop body.
const LOOP_EXIT_LENGTH: u32 = 100_000;

/// Compute liveness and global next-use distances solely from allocator facts.
pub fn analyze_next_uses<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
) -> Result<NextUseAnalysis<V>, AllocationFactsError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
{
    facts.verify()?;
    let block_count = facts.blocks.len();
    let successors = facts
        .blocks
        .iter()
        .map(|block| block.successors.clone())
        .collect::<Vec<_>>();
    let mut predecessors = vec![Vec::new(); block_count];
    for (block, block_successors) in successors.iter().enumerate() {
        for &successor in block_successors {
            predecessors[successor].push(block);
        }
    }
    let backedge_successors = compute_backedge_successors(&successors);
    let backedge_edges = successors
        .iter()
        .enumerate()
        .map(|(block, block_successors)| {
            block_successors
                .iter()
                .map(|successor| backedge_successors[block].contains(successor))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let phi_edge_uses = compute_phi_edge_uses(facts, &successors);
    let transfers = compute_block_transfers(facts);

    let mut entry_distances = vec![FxHashMap::default(); block_count];
    let mut exit_distances = vec![FxHashMap::default(); block_count];
    let mut worklist = (0..block_count).rev().collect::<VecDeque<_>>();
    let mut in_worklist = vec![true; block_count];
    while let Some(block) = worklist.pop_front() {
        in_worklist[block] = false;
        let (new_entry, new_exit) = compute_block_distances(
            block,
            &successors,
            &backedge_edges,
            &phi_edge_uses,
            &transfers,
            &entry_distances,
        );
        if new_entry != entry_distances[block] || new_exit != exit_distances[block] {
            entry_distances[block] = new_entry;
            exit_distances[block] = new_exit;
            for &predecessor in &predecessors[block] {
                if !in_worklist[predecessor] {
                    worklist.push_back(predecessor);
                    in_worklist[predecessor] = true;
                }
            }
        }
    }

    Ok(NextUseAnalysis {
        entry_distances,
        exit_distances,
        predecessors,
        backedge_successors,
    })
}

struct BlockTransfer<V> {
    block_len: u32,
    defs: FxHashSet<V>,
    local_uses: Vec<(V, u32)>,
}

fn compute_block_transfers<V, R>(facts: &FunctionAllocationFacts<V, R>) -> Vec<BlockTransfer<V>>
where
    V: Copy + Eq + Hash + Ord,
{
    facts
        .blocks
        .iter()
        .map(|block| {
            let mut defs = FxHashSet::default();
            defs.reserve(block.phis.len() + block.instructions.len());
            defs.extend(block.phis.iter().map(|phi| phi.destination));
            let mut local_uses = FxHashMap::default();
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                for &definition in &instruction.defs {
                    defs.insert(definition);
                }
                let position = instruction_index as u32;
                for &used in &instruction.uses {
                    if !defs.contains(&used) {
                        local_uses.entry(used).or_insert(position);
                    }
                }
            }
            let mut local_uses = local_uses.into_iter().collect::<Vec<_>>();
            local_uses.sort_by_key(|(value, _)| *value);
            BlockTransfer {
                block_len: block.instructions.len() as u32,
                defs,
                local_uses,
            }
        })
        .collect()
}

fn compute_phi_edge_uses<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
    successors: &[Vec<usize>],
) -> Vec<Vec<Vec<V>>>
where
    V: Copy,
{
    successors
        .iter()
        .enumerate()
        .map(|(predecessor, block_successors)| {
            block_successors
                .iter()
                .map(|&successor| {
                    facts.blocks[successor]
                        .phis
                        .iter()
                        .flat_map(|phi| {
                            phi.sources
                                .iter()
                                .filter(move |source| source.predecessor == predecessor)
                                .map(|source| source.value)
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

fn compute_block_distances<V>(
    block: usize,
    successors: &[Vec<usize>],
    backedge_edges: &[Vec<bool>],
    phi_edge_uses: &[Vec<Vec<V>>],
    transfers: &[BlockTransfer<V>],
    entry_distances: &[FxHashMap<V, u32>],
) -> (FxHashMap<V, u32>, FxHashMap<V, u32>)
where
    V: Copy + Eq + Hash,
{
    let transfer = &transfers[block];
    let mut new_exit = FxHashMap::default();
    let exit_capacity = successors[block]
        .iter()
        .map(|&successor| entry_distances[successor].len())
        .sum::<usize>()
        + phi_edge_uses[block].iter().map(Vec::len).sum::<usize>();
    new_exit.reserve(exit_capacity);
    for (edge, &successor) in successors[block].iter().enumerate() {
        let edge_length = if backedge_edges[block][edge] {
            LOOP_EXIT_LENGTH
        } else {
            0
        };
        for (&value, &distance) in &entry_distances[successor] {
            let distance = distance.saturating_add(edge_length);
            let entry = new_exit.entry(value).or_insert(u32::MAX);
            *entry = (*entry).min(distance);
        }
        for &value in &phi_edge_uses[block][edge] {
            let entry = new_exit.entry(value).or_insert(u32::MAX);
            *entry = (*entry).min(edge_length);
        }
    }

    let mut new_entry = FxHashMap::default();
    new_entry.reserve(new_exit.len() + transfer.local_uses.len());
    for (&value, &distance) in &new_exit {
        if !transfer.defs.contains(&value) {
            new_entry.insert(value, transfer.block_len.saturating_add(distance));
        }
    }
    for &(value, position) in &transfer.local_uses {
        new_entry.insert(value, position);
    }
    (new_entry, new_exit)
}

fn compute_backedge_successors(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut colors = vec![Color::White; successors.len()];
    let mut backedges = vec![Vec::new(); successors.len()];
    for root in 0..successors.len() {
        if colors[root] != Color::White {
            continue;
        }
        colors[root] = Color::Gray;
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next_successor)) = stack.last_mut() {
            if *next_successor == successors[*node].len() {
                colors[*node] = Color::Black;
                stack.pop();
                continue;
            }
            let successor = successors[*node][*next_successor];
            *next_successor += 1;
            match colors[successor] {
                Color::White => {
                    colors[successor] = Color::Gray;
                    stack.push((successor, 0));
                }
                Color::Gray => backedges[*node].push(successor),
                Color::Black => {}
            }
        }
    }
    backedges
}

/// One inclusive live range in a linearized machine function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRange<V> {
    pub value: V,
    pub start: u32,
    pub end: u32,
}

/// Register assignment returned by [`allocate_linear_scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation<V: Eq + Hash, R> {
    assignments: HashMap<V, R>,
}

impl<V: Eq + Hash, R: Copy> Allocation<V, R> {
    pub fn get(&self, value: V) -> Option<R> {
        self.assignments.get(&value).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&V, &R)> {
        self.assignments.iter()
    }
}

/// Failure from target-independent linear-scan allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearScanError<V> {
    EmptyRegisterFile,
    DuplicateValue(V),
    InvalidRange(LiveRange<V>),
    RegisterPressure { value: V, point: u32 },
}

impl<V: fmt::Debug> fmt::Display for LinearScanError<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRegisterFile => formatter.write_str("target has no allocatable registers"),
            Self::DuplicateValue(value) => write!(formatter, "duplicate live range for {value:?}"),
            Self::InvalidRange(range) => write!(
                formatter,
                "invalid live range {:?}: {}..{}",
                range.value, range.start, range.end
            ),
            Self::RegisterPressure { value, point } => write!(
                formatter,
                "no register available for {value:?} at program point {point}"
            ),
        }
    }
}

impl<V: fmt::Debug> std::error::Error for LinearScanError<V> {}

/// Allocate non-spilling live ranges using a deterministic linear scan.
///
/// This is the bootstrap allocator for new native targets. The mature SSA
/// splitter remains available to x86 while its target hooks are separated;
/// both allocators share the physical-register and constraint model here.
pub fn allocate_linear_scan<V, R>(
    ranges: &[LiveRange<V>],
    allocatable: &[R],
) -> Result<Allocation<V, R>, LinearScanError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
    R: MachineRegister,
{
    if allocatable.is_empty() {
        return Err(LinearScanError::EmptyRegisterFile);
    }

    let mut ordered = ranges.to_vec();
    ordered.sort_unstable_by_key(|range| (range.start, range.end, range.value));
    let mut seen = BTreeSet::new();
    for range in &ordered {
        if range.start > range.end {
            return Err(LinearScanError::InvalidRange(*range));
        }
        if !seen.insert(range.value) {
            return Err(LinearScanError::DuplicateValue(range.value));
        }
    }

    let mut active = Vec::<(u32, V, R)>::new();
    let mut assignments = HashMap::with_capacity(ordered.len());
    for range in ordered {
        active.retain(|(end, _, _)| *end >= range.start);
        let register = allocatable
            .iter()
            .copied()
            .find(|candidate| active.iter().all(|(_, _, used)| used != candidate))
            .ok_or(LinearScanError::RegisterPressure {
                value: range.value,
                point: range.start,
            })?;
        assignments.insert(range.value, register);
        active.push((range.end, range.value, register));
        active.sort_unstable_by_key(|(end, value, register)| (*end, *value, register.index()));
    }

    Ok(Allocation { assignments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Reg(u8);

    impl MachineRegister for Reg {
        fn index(self) -> u8 {
            self.0
        }
    }

    #[test]
    fn register_set_supports_registers_above_x86s_range() {
        let mut set = RegisterSet::new();
        set.insert(Reg(30));
        assert!(set.contains(&Reg(30)));
        set.remove(Reg(30));
        assert!(set.is_empty());
    }

    #[test]
    fn linear_scan_reuses_register_after_last_use() {
        let ranges = [
            LiveRange {
                value: 0,
                start: 0,
                end: 1,
            },
            LiveRange {
                value: 1,
                start: 2,
                end: 3,
            },
        ];
        let allocation = allocate_linear_scan(&ranges, &[Reg(9)]).unwrap();
        assert_eq!(allocation.get(0), Some(Reg(9)));
        assert_eq!(allocation.get(1), Some(Reg(9)));
    }

    #[test]
    fn linear_scan_reports_pressure_without_hidden_spills() {
        let ranges = [
            LiveRange {
                value: 0,
                start: 0,
                end: 2,
            },
            LiveRange {
                value: 1,
                start: 1,
                end: 2,
            },
        ];
        assert_eq!(
            allocate_linear_scan(&ranges, &[Reg(9)]),
            Err(LinearScanError::RegisterPressure { value: 1, point: 1 })
        );
    }

    #[test]
    fn allocation_facts_verify_without_knowing_target_opcodes() {
        let facts = FunctionAllocationFacts {
            entry: 0,
            blocks: vec![
                BlockAllocationFacts {
                    successors: vec![1],
                    phis: Vec::new(),
                    instructions: vec![InstructionAllocationFacts {
                        uses: vec![0],
                        defs: vec![1],
                        constraints: InstructionConstraints {
                            fixed_uses: vec![(0, Reg(3))],
                            fixed_defs: vec![(1, Reg(4))],
                            clobbers: vec![Reg(5)],
                        },
                        is_copy: false,
                    }],
                },
                BlockAllocationFacts {
                    successors: Vec::new(),
                    phis: vec![PhiAllocationFacts {
                        destination: 2,
                        sources: vec![PhiSource {
                            predecessor: 0,
                            value: 1,
                        }],
                    }],
                    instructions: Vec::new(),
                },
            ],
        };

        assert_eq!(facts.verify(), Ok(()));
        let analysis = analyze_next_uses(&facts).unwrap();
        assert_eq!(analysis.exit_distances[0].get(&1), Some(&0));
        assert_eq!(analysis.entry_distances[0].get(&0), Some(&0));
    }

    #[test]
    fn allocation_facts_reject_constraints_detached_from_operands() {
        let facts = FunctionAllocationFacts {
            entry: 0,
            blocks: vec![BlockAllocationFacts {
                successors: Vec::new(),
                phis: Vec::new(),
                instructions: vec![InstructionAllocationFacts {
                    uses: vec![0],
                    defs: Vec::new(),
                    constraints: InstructionConstraints {
                        fixed_uses: vec![(1, Reg(3))],
                        ..InstructionConstraints::default()
                    },
                    is_copy: false,
                }],
            }],
        };

        assert_eq!(
            facts.verify(),
            Err(AllocationFactsError::FixedUseIsNotUse {
                block: 0,
                instruction: 0,
                value: 1,
            })
        );
    }
}
