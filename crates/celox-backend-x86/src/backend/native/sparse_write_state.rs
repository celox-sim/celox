//! CFG-exact write-state certificates for sparse next-state lowering.
//!
//! Sparse dirty metadata is clear at function entry when the function's full
//! commit run executes on every path following a sparse Store.  A Store may
//! therefore initialize its touched working chunk directly from stable state
//! when the Store's per-object MemorySSA use reaches LiveOnEntry.
//!
//! Two pruned MemorySSA partitions are built.  The object partition proves
//! whether active-list state already exists.  The chunk partition uses
//! `(AbsoluteAddr, physical 64-bit chunk)` variables and proves whether a
//! statically addressed chunk is clean or dirty.  Exact single-chunk objects
//! use a linear partitioned-SSA fast path.  Mixed objects use an access-chain
//! range walk: a dynamic Store becomes unknown only when it actually reaches
//! the queried chunk, and no wildcard is expanded over the object's width.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use celox_analysis::ssa::{self, Event, Version};

use crate::HashMap;
use crate::MemoryLayout;
use crate::cfg::SirCfg;
use crate::{
    AbsoluteAddr, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIROffset, SIRTerminator, collect_exact_zero_registers,
};

type StorePoint = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SparseWriteState {
    First,
    Active,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SparseChunkState {
    Clean,
    Dirty,
    Unknown,
}

/// Placement of a sparse dirty-word update relative to its SIR Store.
///
/// A batch is formed only from a straight-line run of stores whose clean
/// chunks were independently proved by MemorySSA.  Data stores keep their SIR
/// order; only simulator-private dirty metadata is delayed to the run's final
/// Store, where one mask covers the complete run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SparseMetadataAction {
    #[default]
    Immediate,
    Deferred,
    Batch {
        dirty_word: usize,
        dirty_mask: u64,
        initial_write_state: SparseWriteState,
        initial_dirty_word_state: SparseChunkState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryDefinitionKind {
    Store,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChunkFootprint {
    Exact { first: usize, last: usize },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MemoryDefinition {
    point: StorePoint,
    kind: MemoryDefinitionKind,
    footprint: ChunkFootprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SparseChunk {
    object: AbsoluteAddr,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ChunkRangeQuery {
    object: AbsoluteAddr,
    first: usize,
    last: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ChunkDefinition {
    point: StorePoint,
    kind: MemoryDefinitionKind,
}

#[derive(Debug)]
struct ChunkPlan {
    eligible: bool,
    chunks: BTreeSet<usize>,
    commit_count: usize,
}

impl Default for ChunkPlan {
    fn default() -> Self {
        Self {
            eligible: true,
            chunks: BTreeSet::new(),
            commit_count: 0,
        }
    }
}

const CLEAN: u8 = 1;
const DIRTY: u8 = 2;

#[derive(Debug, Clone, Copy, Default)]
struct ReachingState {
    possible: u8,
    depends_on_entry: bool,
}

#[derive(Debug, Default)]
pub(super) struct SparseWriteStates {
    states: HashMap<(BlockId, usize), SparseWriteState>,
    chunk_states: HashMap<(BlockId, usize), SparseChunkState>,
    dirty_word_states: HashMap<(BlockId, usize), SparseChunkState>,
    metadata_actions: HashMap<(BlockId, usize), SparseMetadataAction>,
    zero_fills: SparseZeroFillPlans,
}

#[derive(Debug, Default)]
struct SparseZeroFillPlans {
    roots: HashMap<(BlockId, usize), RegionedAbsoluteAddr>,
    members: crate::HashSet<(BlockId, usize)>,
    dead_zero_definitions: crate::HashSet<(BlockId, usize)>,
}

impl SparseZeroFillPlans {
    fn is_member(&self, block: BlockId, instruction: usize) -> bool {
        self.members.contains(&(block, instruction))
    }

    fn root(&self, block: BlockId, instruction: usize) -> Option<RegionedAbsoluteAddr> {
        self.roots.get(&(block, instruction)).copied()
    }

    fn is_dead_zero_definition(&self, block: BlockId, instruction: usize) -> bool {
        self.dead_zero_definitions.contains(&(block, instruction))
    }
}

#[derive(Debug, Clone, Copy)]
struct ZeroStoreCandidate {
    instruction: usize,
    start: usize,
    end: usize,
}

fn finish_zero_fill_group(
    block: BlockId,
    address: RegionedAbsoluteAddr,
    mut candidates: Vec<ZeroStoreCandidate>,
    layout: &MemoryLayout,
    plans: &mut SparseZeroFillPlans,
) {
    let object = address.absolute_addr();
    let Some(&logical_width) = layout.widths.get(&object) else {
        return;
    };
    let multiple_native_chunks = if address.region == crate::STABLE_REGION {
        layout.plane_size(&object) > 8
    } else {
        layout
            .sparse_layouts
            .get(&object)
            .is_some_and(|sparse| sparse.chunk_count > 1)
    };
    // A single native chunk already has a cheaper dedicated lowering.
    if logical_width == 0 || !multiple_native_chunks || candidates.is_empty() {
        return;
    }

    candidates.sort_unstable_by_key(|candidate| (candidate.start, candidate.end));
    let mut covered_end = 0usize;
    for candidate in &candidates {
        if candidate.start > covered_end || candidate.end > logical_width {
            return;
        }
        covered_end = covered_end.max(candidate.end);
    }
    if covered_end != logical_width {
        return;
    }

    let anchor = candidates
        .iter()
        .map(|candidate| candidate.instruction)
        .max()
        .expect("non-empty zero-fill group must have an anchor");
    for candidate in candidates {
        plans.members.insert((block, candidate.instruction));
    }
    plans.roots.insert((block, anchor), address);
}

fn seal_zero_fill_group(
    block: BlockId,
    address: RegionedAbsoluteAddr,
    open: &mut HashMap<RegionedAbsoluteAddr, Vec<ZeroStoreCandidate>>,
    layout: &MemoryLayout,
    plans: &mut SparseZeroFillPlans,
) {
    if let Some(candidates) = open.remove(&address) {
        finish_zero_fill_group(block, address, candidates, layout, plans);
    }
}

fn visit_instruction_uses<A>(instruction: &SIRInstruction<A>, mut visit: impl FnMut(RegisterId)) {
    let mut visit_offset = |offset: &SIROffset| {
        for register in offset.dynamic_registers().into_iter().flatten() {
            visit(register);
        }
    };
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            visit(*lhs);
            visit(*rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            visit(*source);
        }
        SIRInstruction::Load(_, _, offset, _) => visit_offset(offset),
        SIRInstruction::Store(_, offset, width, source, _, _) => {
            if *width != 0 {
                visit_offset(offset);
                visit(*source);
            }
        }
        SIRInstruction::Commit(_, _, offset, _, _) => visit_offset(offset),
        SIRInstruction::Concat(_, sources)
        | SIRInstruction::RuntimeEvent { args: sources, .. }
        | SIRInstruction::CombCaptureEvent { args: sources, .. } => {
            for &source in sources {
                visit(source);
            }
        }
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            visit(*condition);
            visit(*then_value);
            visit(*else_value);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            visit(*old);
            visit(*new);
        }
    }
}

fn visit_terminator_uses(terminator: &SIRTerminator, mut visit: impl FnMut(RegisterId)) {
    match terminator {
        SIRTerminator::Jump(_, arguments) => {
            for &argument in arguments {
                visit(argument);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            visit(*cond);
            for &argument in &true_block.1 {
                visit(argument);
            }
            for &argument in &false_block.1 {
                visit(argument);
            }
        }
        SIRTerminator::Switch { selector, .. } => {
            visit(*selector);
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn decrement_use(
    register: RegisterId,
    use_counts: &mut HashMap<RegisterId, usize>,
    work: &mut VecDeque<RegisterId>,
) {
    let Some(count) = use_counts.get_mut(&register) else {
        return;
    };
    debug_assert_ne!(*count, 0);
    *count -= 1;
    if *count == 0 {
        work.push_back(register);
    }
}

/// Definitions used only to construct a removed bulk-zero Store must not be
/// lowered either. This is ordinary backwards DCE over the already-proved
/// exact-zero subgraph and is linear in the actual operand edges.
fn find_dead_zero_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    exact_zeros: &crate::HashSet<RegisterId>,
    members: &crate::HashSet<(BlockId, usize)>,
) -> crate::HashSet<(BlockId, usize)> {
    let mut use_counts = HashMap::<RegisterId, usize>::default();
    let mut definitions = HashMap::<RegisterId, (BlockId, usize)>::default();
    for (&block_id, block) in &eu.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            if let Some(dst) = instruction.defined_register()
                && exact_zeros.contains(&dst)
            {
                definitions.insert(dst, (block_id, index));
            }
            visit_instruction_uses(instruction, |register| {
                *use_counts.entry(register).or_default() += 1;
            });
        }
        visit_terminator_uses(&block.terminator, |register| {
            *use_counts.entry(register).or_default() += 1;
        });
    }

    let mut work = VecDeque::new();
    for &(block_id, index) in members {
        let instruction = &eu.blocks[&block_id].instructions[index];
        visit_instruction_uses(instruction, |register| {
            decrement_use(register, &mut use_counts, &mut work);
        });
    }

    let mut dead = crate::HashSet::default();
    while let Some(register) = work.pop_front() {
        let Some(&point) = definitions.get(&register) else {
            continue;
        };
        if !dead.insert(point) {
            continue;
        }
        let instruction = &eu.blocks[&point.0].instructions[point.1];
        visit_instruction_uses(instruction, |dependency| {
            decrement_use(dependency, &mut use_counts, &mut work);
        });
    }
    dead
}

fn is_sparse_origin_zero_fill_region(region: u32) -> bool {
    region == crate::SPARSE_WORKING_REGION || region == crate::STABLE_REGION
}

/// Find a whole-object zero overwrite before lowering expands every logical
/// Store. This covers sparse next state and a sparse-origin object which a
/// complete-event proof redirected to STABLE. Stores to other objects and
/// unrelated observable events may be interleaved: an event which consumes an
/// RTL value has an explicit register/data dependency on the Load producing
/// that value. A same-object read, Commit, or non-zero/dynamic Store seals the
/// group.
fn find_sparse_zero_fills(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
) -> SparseZeroFillPlans {
    let mut zero_roots = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, offset, width, source, triggers, capture_sites)
                if is_sparse_origin_zero_fill_region(address.region)
                    && offset.constant_bit_offset().is_some()
                    && (address.region == crate::STABLE_REGION
                        || layout.sparse_layouts.contains_key(&address.absolute_addr()))
                    && *width != 0
                    && triggers.is_empty()
                    && capture_sites.is_empty() =>
            {
                Some(*source)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    zero_roots.sort_unstable();
    zero_roots.dedup();
    let zeros = collect_exact_zero_registers(eu, zero_roots);
    let mut plans = SparseZeroFillPlans::default();
    for (&block_id, block) in &eu.blocks {
        let mut open = HashMap::<RegionedAbsoluteAddr, Vec<ZeroStoreCandidate>>::default();
        for (instruction, inst) in block.instructions.iter().enumerate() {
            match inst {
                SIRInstruction::Store(address, offset, width, source, triggers, capture_sites)
                    if is_sparse_origin_zero_fill_region(address.region)
                        && offset.constant_bit_offset().is_some()
                        && (address.region == crate::STABLE_REGION
                            || layout.sparse_layouts.contains_key(&address.absolute_addr()))
                        && *width != 0
                        && triggers.is_empty()
                        && capture_sites.is_empty()
                        && zeros.contains(source) =>
                {
                    let start = offset.constant_bit_offset().unwrap();
                    let Some(end) = start.checked_add(*width) else {
                        seal_zero_fill_group(block_id, *address, &mut open, layout, &mut plans);
                        continue;
                    };
                    open.entry(*address).or_default().push(ZeroStoreCandidate {
                        instruction,
                        start,
                        end,
                    });
                }
                SIRInstruction::Store(address, ..) | SIRInstruction::Load(_, address, ..) => {
                    seal_zero_fill_group(block_id, *address, &mut open, layout, &mut plans);
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    seal_zero_fill_group(block_id, *source, &mut open, layout, &mut plans);
                    seal_zero_fill_group(block_id, *destination, &mut open, layout, &mut plans);
                }
                _ => {}
            }
        }
        for (address, candidates) in open {
            finish_zero_fill_group(block_id, address, candidates, layout, &mut plans);
        }
    }
    plans.dead_zero_definitions = find_dead_zero_definitions(eu, &zeros, &plans.members);
    plans
}

impl SparseWriteStates {
    /// Whole-object zero overwrites are independent of the sparse commit
    /// strategy.  In particular, eval-only FF functions may publish their
    /// working state from a separate apply function and therefore have no
    /// local worklist commit run at all.
    pub(super) fn zero_fills_only(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        layout: &MemoryLayout,
    ) -> Self {
        Self {
            zero_fills: find_sparse_zero_fills(eu, layout),
            ..Self::default()
        }
    }

    pub(super) fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        layout: &MemoryLayout,
        commit_block: BlockId,
        commit_start: usize,
    ) -> Option<Self> {
        let zero_fills = find_sparse_zero_fills(eu, layout);
        let cfg = SirCfg::analyze(eu).ok()?;
        let commit_index = cfg.block_index(commit_block)?;
        let mut events = vec![
            Vec::<Event<AbsoluteAddr, MemoryDefinition, StorePoint>>::new();
            cfg.block_ids.len()
        ];
        let mut stores = Vec::<(StorePoint, BlockId, AbsoluteAddr, Option<usize>)>::new();
        let mut chunk_plans = BTreeMap::<AbsoluteAddr, ChunkPlan>::new();

        for (block_index, &block_id) in cfg.block_ids.iter().enumerate() {
            for (instruction, inst) in eu.blocks[&block_id].instructions.iter().enumerate() {
                let point = (block_index, instruction);
                if zero_fills.is_member(block_id, instruction) {
                    if let Some(address) = zero_fills.root(block_id, instruction) {
                        let object = address.absolute_addr();
                        let footprint = ChunkFootprint::Unknown;
                        chunk_plans.entry(object).or_default().eligible = false;
                        events[block_index].push(Event::Use {
                            variable: object,
                            usage: point,
                        });
                        events[block_index].push(Event::Definition {
                            variable: object,
                            definition: MemoryDefinition {
                                point,
                                kind: MemoryDefinitionKind::Store,
                                footprint,
                            },
                        });
                        stores.push((point, block_id, object, None));
                    }
                    continue;
                }
                match inst {
                    SIRInstruction::Store(address, offset, width, _, _, _)
                        if address.region == crate::SPARSE_WORKING_REGION && *width != 0 =>
                    {
                        let object = address.absolute_addr();
                        let footprint = static_chunk_range(layout, object, offset, *width)
                            .map_or(ChunkFootprint::Unknown, |(first, last)| {
                                ChunkFootprint::Exact { first, last }
                            });
                        let chunk = match footprint {
                            ChunkFootprint::Exact { first, last } if first == last => Some(first),
                            ChunkFootprint::Exact { .. } | ChunkFootprint::Unknown => None,
                        };
                        let plan = chunk_plans.entry(object).or_default();
                        if let Some(chunk) = chunk {
                            plan.chunks.insert(chunk);
                        } else {
                            plan.eligible = false;
                        }
                        events[block_index].push(Event::Use {
                            variable: object,
                            usage: point,
                        });
                        events[block_index].push(Event::Definition {
                            variable: object,
                            definition: MemoryDefinition {
                                point,
                                kind: MemoryDefinitionKind::Store,
                                footprint,
                            },
                        });
                        stores.push((point, block_id, object, chunk));
                    }
                    SIRInstruction::Commit(source, destination, ..)
                        if source.region == crate::SPARSE_WORKING_REGION
                            && destination.region == crate::STABLE_REGION =>
                    {
                        let object = source.absolute_addr();
                        chunk_plans.entry(object).or_default().commit_count += 1;
                        events[block_index].push(Event::Use {
                            variable: object,
                            usage: point,
                        });
                        events[block_index].push(Event::Definition {
                            variable: object,
                            definition: MemoryDefinition {
                                point,
                                kind: MemoryDefinitionKind::Reset,
                                footprint: ChunkFootprint::Unknown,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        let memory_ssa = ssa::build(&cfg, &events).ok()?;
        let object_phis =
            resolve_phi_states(
                &memory_ssa,
                |definition: MemoryDefinition| match definition.kind {
                    MemoryDefinitionKind::Store => DIRTY,
                    MemoryDefinitionKind::Reset => CLEAN,
                },
            );

        // Expanding a wildcard Store or repeated reset over every candidate
        // chunk would make the event table quadratic.  Keep the linear
        // partitioned-SSA path for exact objects; mixed objects are handled by
        // the query-local range walker below without wildcard expansion.
        let partitioned_objects = chunk_plans
            .iter()
            .filter_map(|(&object, plan)| {
                (plan.eligible && !plan.chunks.is_empty() && plan.commit_count == 1)
                    .then_some(object)
            })
            .collect::<BTreeSet<_>>();
        let mut chunk_events = vec![
            Vec::<Event<SparseChunk, ChunkDefinition, StorePoint>>::new();
            cfg.block_ids.len()
        ];
        for (block_index, &block_id) in cfg.block_ids.iter().enumerate() {
            for (instruction, inst) in eu.blocks[&block_id].instructions.iter().enumerate() {
                let point = (block_index, instruction);
                if zero_fills.is_member(block_id, instruction) {
                    continue;
                }
                match inst {
                    SIRInstruction::Store(address, offset, width, _, _, _)
                        if address.region == crate::SPARSE_WORKING_REGION && *width != 0 =>
                    {
                        let object = address.absolute_addr();
                        if !partitioned_objects.contains(&object) {
                            continue;
                        }
                        let Some(index) = static_single_chunk(layout, object, offset, *width)
                        else {
                            continue;
                        };
                        let variable = SparseChunk { object, index };
                        chunk_events[block_index].push(Event::Use {
                            variable,
                            usage: point,
                        });
                        chunk_events[block_index].push(Event::Definition {
                            variable,
                            definition: ChunkDefinition {
                                point,
                                kind: MemoryDefinitionKind::Store,
                            },
                        });
                    }
                    SIRInstruction::Commit(source, destination, ..)
                        if source.region == crate::SPARSE_WORKING_REGION
                            && destination.region == crate::STABLE_REGION =>
                    {
                        let object = source.absolute_addr();
                        if !partitioned_objects.contains(&object) {
                            continue;
                        }
                        let plan = &chunk_plans[&object];
                        for &index in &plan.chunks {
                            chunk_events[block_index].push(Event::Definition {
                                variable: SparseChunk { object, index },
                                definition: ChunkDefinition {
                                    point,
                                    kind: MemoryDefinitionKind::Reset,
                                },
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        let chunk_ssa = ssa::build(&cfg, &chunk_events).ok()?;
        let chunk_phis = resolve_phi_states(&chunk_ssa, |definition: ChunkDefinition| {
            match definition.kind {
                MemoryDefinitionKind::Store => DIRTY,
                MemoryDefinitionKind::Reset => CLEAN,
            }
        });

        let range_graph = RangeWriteGraph::build(&memory_ssa)?;
        let mut range_solver = RangeWriteSolver::new(&range_graph);
        let mut range_queries = BTreeMap::<ChunkRangeQuery, Vec<(StorePoint, BlockId)>>::new();
        for &(point, block_id, object, chunk) in &stores {
            if partitioned_objects.contains(&object) {
                continue;
            }
            if let Some(index) = chunk {
                range_queries
                    .entry(ChunkRangeQuery {
                        object,
                        first: index,
                        last: index,
                    })
                    .or_default()
                    .push((point, block_id));
            }
        }
        let mut range_chunk_states = BTreeMap::<StorePoint, SparseChunkState>::new();
        for (query, points) in range_queries {
            range_solver.solve(query);
            for (point, block_id) in points {
                let Some(version) = memory_ssa.uses.get(&point).copied() else {
                    continue;
                };
                let block_index = cfg.block_index(block_id)?;
                let entry_is_clean = store_entry_is_clean(
                    &cfg,
                    block_id,
                    block_index,
                    point.1,
                    commit_block,
                    commit_index,
                    commit_start,
                );
                let state = range_solver.state(version);
                let possible = if state.possible == 0 || (state.depends_on_entry && !entry_is_clean)
                {
                    CLEAN | DIRTY
                } else {
                    state.possible
                };
                range_chunk_states.insert(point, classify_chunk_state(possible));
            }
        }

        let mut dirty_word_queries = BTreeMap::<ChunkRangeQuery, Vec<(StorePoint, BlockId)>>::new();
        for &(point, block_id, object, chunk) in &stores {
            let Some(chunk) = chunk else {
                continue;
            };
            let first = (chunk / 64) * 64;
            let last = first
                .saturating_add(63)
                .min(layout.sparse_layouts[&object].chunk_count.saturating_sub(1));
            dirty_word_queries
                .entry(ChunkRangeQuery {
                    object,
                    first,
                    last,
                })
                .or_default()
                .push((point, block_id));
        }
        let mut dirty_word_states = HashMap::default();
        for (query, points) in dirty_word_queries {
            range_solver.solve(query);
            for (point, block_id) in points {
                let Some(version) = memory_ssa.uses.get(&point).copied() else {
                    continue;
                };
                let block_index = cfg.block_index(block_id)?;
                let entry_is_clean = store_entry_is_clean(
                    &cfg,
                    block_id,
                    block_index,
                    point.1,
                    commit_block,
                    commit_index,
                    commit_start,
                );
                let state = range_solver.state(version);
                let possible = if state.possible == 0 || (state.depends_on_entry && !entry_is_clean)
                {
                    CLEAN | DIRTY
                } else {
                    state.possible
                };
                dirty_word_states.insert((block_id, point.1), classify_chunk_state(possible));
            }
        }

        let mut states = HashMap::default();
        let mut chunk_states = HashMap::default();
        for (point, block_id, object, chunk) in stores {
            let block_index = cfg.block_index(block_id)?;
            let entry_is_clean = store_entry_is_clean(
                &cfg,
                block_id,
                block_index,
                point.1,
                commit_block,
                commit_index,
                commit_start,
            );
            let object_state = memory_ssa
                .uses
                .get(&point)
                .copied()
                .map(|version| {
                    classify_object_state(version_state(
                        version,
                        entry_is_clean,
                        &object_phis,
                        |definition: MemoryDefinition| match definition.kind {
                            MemoryDefinitionKind::Store => DIRTY,
                            MemoryDefinitionKind::Reset => CLEAN,
                        },
                    ))
                })
                .unwrap_or(SparseWriteState::Unknown);
            let chunk_state = if partitioned_objects.contains(&object) {
                chunk
                    .and_then(|_| chunk_ssa.uses.get(&point).copied())
                    .map(|version| {
                        classify_chunk_state(version_state(
                            version,
                            entry_is_clean,
                            &chunk_phis,
                            |definition: ChunkDefinition| match definition.kind {
                                MemoryDefinitionKind::Store => DIRTY,
                                MemoryDefinitionKind::Reset => CLEAN,
                            },
                        ))
                    })
                    .unwrap_or(SparseChunkState::Unknown)
            } else {
                range_chunk_states
                    .get(&point)
                    .copied()
                    .unwrap_or(SparseChunkState::Unknown)
            };
            states.insert((block_id, point.1), object_state);
            chunk_states.insert((block_id, point.1), chunk_state);
        }
        let metadata_actions = plan_metadata_batches(
            eu,
            layout,
            &states,
            &chunk_states,
            &dirty_word_states,
            &zero_fills,
        );
        Some(Self {
            states,
            chunk_states,
            dirty_word_states,
            metadata_actions,
            zero_fills,
        })
    }

    pub(super) fn state(&self, block: BlockId, instruction: usize) -> SparseWriteState {
        self.states
            .get(&(block, instruction))
            .copied()
            .unwrap_or(SparseWriteState::Unknown)
    }

    pub(super) fn chunk_state(&self, block: BlockId, instruction: usize) -> SparseChunkState {
        self.chunk_states
            .get(&(block, instruction))
            .copied()
            .unwrap_or(SparseChunkState::Unknown)
    }

    pub(super) fn dirty_word_state(&self, block: BlockId, instruction: usize) -> SparseChunkState {
        self.dirty_word_states
            .get(&(block, instruction))
            .copied()
            .unwrap_or(SparseChunkState::Unknown)
    }

    pub(super) fn metadata_action(
        &self,
        block: BlockId,
        instruction: usize,
    ) -> SparseMetadataAction {
        self.metadata_actions
            .get(&(block, instruction))
            .copied()
            .unwrap_or_default()
    }

    /// Returns the address at a compact zero-fill anchor. Every other member
    /// of the same fill group returns `None` and is identified by
    /// [`Self::is_zero_fill_member`].
    pub(super) fn zero_fill_root(
        &self,
        block: BlockId,
        instruction: usize,
    ) -> Option<RegionedAbsoluteAddr> {
        self.zero_fills.root(block, instruction)
    }

    pub(super) fn is_zero_fill_member(&self, block: BlockId, instruction: usize) -> bool {
        self.zero_fills.is_member(block, instruction)
    }

    pub(super) fn is_dead_zero_definition(&self, block: BlockId, instruction: usize) -> bool {
        self.zero_fills.is_dead_zero_definition(block, instruction)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingMetadataBatch {
    object: AbsoluteAddr,
    dirty_word: usize,
    last: (BlockId, usize),
    count: usize,
    dirty_mask: u64,
    initial_write_state: SparseWriteState,
    initial_dirty_word_state: SparseChunkState,
}

fn finish_metadata_batch(
    pending: &mut Option<PendingMetadataBatch>,
    actions: &mut HashMap<(BlockId, usize), SparseMetadataAction>,
) {
    let Some(batch) = pending.take() else {
        return;
    };
    if batch.count > 1 {
        actions.insert(
            batch.last,
            SparseMetadataAction::Batch {
                dirty_word: batch.dirty_word,
                dirty_mask: batch.dirty_mask,
                initial_write_state: batch.initial_write_state,
                initial_dirty_word_state: batch.initial_dirty_word_state,
            },
        );
    }
}

/// Coalesce only a single straight-line `(object, dirty word)` run.  Changing
/// object/word or crossing any Store, Commit, or event which is not part of
/// the proved run closes it.  This keeps the scan linear and prevents batches
/// for different summary bits from being interleaved or reordered.
fn plan_metadata_batches(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    states: &HashMap<(BlockId, usize), SparseWriteState>,
    chunk_states: &HashMap<(BlockId, usize), SparseChunkState>,
    dirty_word_states: &HashMap<(BlockId, usize), SparseChunkState>,
    zero_fills: &SparseZeroFillPlans,
) -> HashMap<(BlockId, usize), SparseMetadataAction> {
    let mut actions = HashMap::default();
    for (&block_id, block) in &eu.blocks {
        let mut pending = None::<PendingMetadataBatch>;
        for (instruction, inst) in block.instructions.iter().enumerate() {
            let point = (block_id, instruction);
            if zero_fills.is_member(block_id, instruction) {
                finish_metadata_batch(&mut pending, &mut actions);
                continue;
            }
            let candidate = match inst {
                SIRInstruction::Store(address, offset, width, _, triggers, capture_sites)
                    if address.region == crate::SPARSE_WORKING_REGION
                        && *width != 0
                        && triggers.is_empty()
                        && capture_sites.is_empty() =>
                {
                    let object = address.absolute_addr();
                    let multi_chunk_object = layout
                        .sparse_layouts
                        .get(&object)
                        .is_some_and(|sparse| sparse.chunk_count > 1);
                    let write_state = states
                        .get(&point)
                        .copied()
                        .unwrap_or(SparseWriteState::Unknown);
                    let chunk_state = chunk_states
                        .get(&point)
                        .copied()
                        .unwrap_or(SparseChunkState::Unknown);
                    (multi_chunk_object
                        && matches!(
                            write_state,
                            SparseWriteState::First | SparseWriteState::Active
                        )
                        && chunk_state == SparseChunkState::Clean)
                        .then(|| {
                            let chunk = static_single_chunk(layout, object, offset, *width)?;
                            Some((
                                object,
                                chunk / 64,
                                chunk % 64,
                                write_state,
                                dirty_word_states
                                    .get(&point)
                                    .copied()
                                    .unwrap_or(SparseChunkState::Unknown),
                            ))
                        })
                        .flatten()
                }
                _ => None,
            };

            if let Some((object, dirty_word, bit, write_state, dirty_word_state)) = candidate {
                let same_run = pending
                    .is_some_and(|batch| batch.object == object && batch.dirty_word == dirty_word);
                if !same_run {
                    finish_metadata_batch(&mut pending, &mut actions);
                    pending = Some(PendingMetadataBatch {
                        object,
                        dirty_word,
                        last: point,
                        count: 1,
                        dirty_mask: 1u64 << bit,
                        initial_write_state: write_state,
                        initial_dirty_word_state: dirty_word_state,
                    });
                    continue;
                }

                let batch = pending
                    .as_mut()
                    .expect("same metadata run requires a pending batch");
                actions.insert(batch.last, SparseMetadataAction::Deferred);
                batch.last = point;
                batch.count += 1;
                batch.dirty_mask |= 1u64 << bit;
                continue;
            }

            if matches!(
                inst,
                SIRInstruction::Store(..)
                    | SIRInstruction::Commit(..)
                    | SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
            ) {
                finish_metadata_batch(&mut pending, &mut actions);
            }
        }
        finish_metadata_batch(&mut pending, &mut actions);
    }
    actions
}

fn static_single_chunk(
    layout: &MemoryLayout,
    object: AbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) -> Option<usize> {
    let (first, last) = static_chunk_range(layout, object, offset, width)?;
    (first == last).then_some(first)
}

fn static_chunk_range(
    layout: &MemoryLayout,
    object: AbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) -> Option<(usize, usize)> {
    let SIROffset::Static(bit_offset) = offset else {
        return None;
    };
    if width == 0 {
        return None;
    }
    if let Some(array) = layout.unpacked_arrays.get(&object) {
        let within_element = bit_offset % array.element_width;
        if width > array.element_width.checked_sub(within_element)? {
            return None;
        }
    }
    let (byte_offset, intra_byte) = layout.map_static_bit_offset(&object, *bit_offset);
    let start = byte_offset.checked_mul(8)?.checked_add(intra_byte)?;
    let end = start.checked_add(width - 1)?;
    let first = start / 64;
    let last = end / 64;
    (last < layout.sparse_layouts.get(&object)?.chunk_count).then_some((first, last))
}

fn store_entry_is_clean(
    cfg: &SirCfg,
    block_id: BlockId,
    block_index: usize,
    instruction: usize,
    commit_block: BlockId,
    commit_index: usize,
    commit_start: usize,
) -> bool {
    if block_id == commit_block {
        instruction < commit_start
    } else {
        cfg.scc_for_block[block_index] != cfg.scc_for_block[commit_index]
            && cfg.postdominates(commit_block, block_id)
    }
}

#[derive(Debug)]
enum RangeWriteNode {
    Entry {
        object: AbsoluteAddr,
    },
    Definition {
        object: AbsoluteAddr,
        definition: MemoryDefinition,
        predecessor: usize,
    },
    Phi {
        object: AbsoluteAddr,
        inputs: Vec<usize>,
    },
}

impl RangeWriteNode {
    fn object(&self) -> AbsoluteAddr {
        match self {
            Self::Entry { object } | Self::Definition { object, .. } | Self::Phi { object, .. } => {
                *object
            }
        }
    }
}

#[derive(Debug)]
struct RangeWriteGraph {
    versions: BTreeMap<Version<AbsoluteAddr, MemoryDefinition>, usize>,
    nodes: Vec<RangeWriteNode>,
    users: Vec<Vec<usize>>,
    nodes_by_object: BTreeMap<AbsoluteAddr, Vec<usize>>,
}

impl RangeWriteGraph {
    fn build(
        sparse_ssa: &ssa::SparseSsa<AbsoluteAddr, MemoryDefinition, StorePoint>,
    ) -> Option<Self> {
        let phis = sparse_ssa
            .phis
            .iter()
            .map(|phi| ((phi.variable, phi.block), phi))
            .collect::<BTreeMap<_, _>>();
        let mut versions = BTreeMap::new();
        let mut ordered = Vec::new();
        for &version in sparse_ssa.uses.values() {
            intern_range_version(&mut versions, &mut ordered, version);
        }
        for phi in &sparse_ssa.phis {
            intern_range_version(&mut versions, &mut ordered, phi.version);
            for &(_, input) in &phi.inputs {
                intern_range_version(&mut versions, &mut ordered, input);
            }
        }

        let mut nodes = Vec::new();
        let mut index = 0;
        while index < ordered.len() {
            let version = ordered[index];
            let node = match version {
                Version::Entry(object) => RangeWriteNode::Entry { object },
                Version::Definition {
                    variable: object,
                    definition,
                } => {
                    let predecessor = sparse_ssa.uses.get(&definition.point).copied()?;
                    let predecessor =
                        intern_range_version(&mut versions, &mut ordered, predecessor);
                    RangeWriteNode::Definition {
                        object,
                        definition,
                        predecessor,
                    }
                }
                Version::Phi {
                    variable: object,
                    block,
                } => {
                    let phi = phis.get(&(object, block))?;
                    let inputs = phi
                        .inputs
                        .iter()
                        .map(|&(_, input)| intern_range_version(&mut versions, &mut ordered, input))
                        .collect();
                    RangeWriteNode::Phi { object, inputs }
                }
            };
            nodes.push(node);
            index += 1;
        }

        let mut users = vec![Vec::new(); nodes.len()];
        let mut nodes_by_object = BTreeMap::<AbsoluteAddr, Vec<usize>>::new();
        for (node, value) in nodes.iter().enumerate() {
            nodes_by_object
                .entry(value.object())
                .or_default()
                .push(node);
            match value {
                RangeWriteNode::Entry { .. } => {}
                RangeWriteNode::Definition { predecessor, .. } => {
                    users[*predecessor].push(node);
                }
                RangeWriteNode::Phi { inputs, .. } => {
                    for &input in inputs {
                        users[input].push(node);
                    }
                }
            }
        }
        Some(Self {
            versions,
            nodes,
            users,
            nodes_by_object,
        })
    }
}

fn intern_range_version(
    versions: &mut BTreeMap<Version<AbsoluteAddr, MemoryDefinition>, usize>,
    ordered: &mut Vec<Version<AbsoluteAddr, MemoryDefinition>>,
    version: Version<AbsoluteAddr, MemoryDefinition>,
) -> usize {
    if let Some(&index) = versions.get(&version) {
        return index;
    }
    let index = ordered.len();
    ordered.push(version);
    versions.insert(version, index);
    index
}

struct RangeWriteSolver<'a> {
    graph: &'a RangeWriteGraph,
    states: Vec<ReachingState>,
    work: VecDeque<usize>,
}

impl<'a> RangeWriteSolver<'a> {
    fn new(graph: &'a RangeWriteGraph) -> Self {
        Self {
            graph,
            states: vec![ReachingState::default(); graph.nodes.len()],
            work: VecDeque::new(),
        }
    }

    fn solve(&mut self, query: ChunkRangeQuery) {
        self.work.clear();
        let Some(nodes) = self.graph.nodes_by_object.get(&query.object) else {
            return;
        };
        for &node in nodes {
            self.states[node] = self.initial_state(node, query);
            if self.states[node].possible != 0 || self.states[node].depends_on_entry {
                self.work.push_back(node);
            }
        }

        while let Some(node) = self.work.pop_front() {
            let reaching = self.states[node];
            for &user in &self.graph.users[node] {
                if !self.forwards_predecessor(user, query) {
                    continue;
                }
                let old = self.states[user];
                self.states[user].possible |= reaching.possible;
                self.states[user].depends_on_entry |= reaching.depends_on_entry;
                if self.states[user].possible != old.possible
                    || self.states[user].depends_on_entry != old.depends_on_entry
                {
                    self.work.push_back(user);
                }
            }
        }
    }

    fn state(&self, version: Version<AbsoluteAddr, MemoryDefinition>) -> ReachingState {
        self.graph
            .versions
            .get(&version)
            .map(|&node| self.states[node])
            .unwrap_or_default()
    }

    fn initial_state(&self, node: usize, query: ChunkRangeQuery) -> ReachingState {
        match self.graph.nodes[node] {
            RangeWriteNode::Entry { .. } => ReachingState {
                possible: CLEAN,
                depends_on_entry: true,
            },
            RangeWriteNode::Definition { definition, .. } => match definition.kind {
                MemoryDefinitionKind::Reset => ReachingState {
                    possible: CLEAN,
                    depends_on_entry: false,
                },
                MemoryDefinitionKind::Store => match definition.footprint {
                    ChunkFootprint::Exact { first, last }
                        if chunk_ranges_overlap(first, last, query.first, query.last) =>
                    {
                        ReachingState {
                            possible: DIRTY,
                            depends_on_entry: false,
                        }
                    }
                    ChunkFootprint::Unknown => ReachingState {
                        possible: CLEAN | DIRTY,
                        depends_on_entry: false,
                    },
                    ChunkFootprint::Exact { .. } => ReachingState::default(),
                },
            },
            RangeWriteNode::Phi { .. } => ReachingState::default(),
        }
    }

    fn forwards_predecessor(&self, node: usize, query: ChunkRangeQuery) -> bool {
        match self.graph.nodes[node] {
            RangeWriteNode::Phi { .. } => true,
            RangeWriteNode::Definition { definition, .. } => matches!(
                definition,
                MemoryDefinition {
                    kind: MemoryDefinitionKind::Store,
                    footprint: ChunkFootprint::Exact { first, last },
                    ..
                } if !chunk_ranges_overlap(first, last, query.first, query.last)
            ),
            RangeWriteNode::Entry { .. } => false,
        }
    }
}

fn chunk_ranges_overlap(first: usize, last: usize, other_first: usize, other_last: usize) -> bool {
    first <= other_last && other_first <= last
}

fn resolve_phi_states<V, D, U>(
    sparse_ssa: &ssa::SparseSsa<V, D, U>,
    definition_state: impl Fn(D) -> u8,
) -> BTreeMap<(V, usize), ReachingState>
where
    V: Copy + Ord,
    D: Copy + Ord,
{
    let phi_indices = sparse_ssa
        .phis
        .iter()
        .enumerate()
        .map(|(index, phi)| ((phi.variable, phi.block), index))
        .collect::<BTreeMap<_, _>>();
    let mut states = vec![ReachingState::default(); sparse_ssa.phis.len()];
    let mut users = vec![Vec::<usize>::new(); sparse_ssa.phis.len()];

    for (phi_index, phi) in sparse_ssa.phis.iter().enumerate() {
        for &(_, input) in &phi.inputs {
            match input {
                Version::Entry(_) => {
                    states[phi_index].possible |= CLEAN;
                    states[phi_index].depends_on_entry = true;
                }
                Version::Definition { definition, .. } => {
                    states[phi_index].possible |= definition_state(definition);
                }
                Version::Phi { variable, block } => {
                    let input_index = phi_indices[&(variable, block)];
                    users[input_index].push(phi_index);
                }
            }
        }
    }

    let mut work = (0..states.len()).collect::<VecDeque<_>>();
    while let Some(phi) = work.pop_front() {
        let reaching = states[phi];
        for &user in &users[phi] {
            let old = states[user];
            states[user].possible |= reaching.possible;
            states[user].depends_on_entry |= reaching.depends_on_entry;
            if states[user].possible != old.possible
                || states[user].depends_on_entry != old.depends_on_entry
            {
                work.push_back(user);
            }
        }
    }

    phi_indices
        .into_iter()
        .map(|(identity, index)| (identity, states[index]))
        .collect()
}

fn version_state<V, D>(
    version: Version<V, D>,
    entry_is_clean: bool,
    phis: &BTreeMap<(V, usize), ReachingState>,
    definition_state: impl Fn(D) -> u8,
) -> u8
where
    V: Copy + Ord,
    D: Copy + Ord,
{
    match version {
        Version::Entry(_) => {
            if entry_is_clean {
                CLEAN
            } else {
                CLEAN | DIRTY
            }
        }
        Version::Definition { definition, .. } => definition_state(definition),
        Version::Phi { variable, block } => {
            let state = phis.get(&(variable, block)).copied().unwrap_or_default();
            if state.possible == 0 || (state.depends_on_entry && !entry_is_clean) {
                CLEAN | DIRTY
            } else {
                state.possible
            }
        }
    }
}

fn classify_object_state(possible: u8) -> SparseWriteState {
    match possible {
        CLEAN => SparseWriteState::First,
        DIRTY => SparseWriteState::Active,
        _ => SparseWriteState::Unknown,
    }
}

fn classify_chunk_state(possible: u8) -> SparseChunkState {
    match possible {
        CLEAN => SparseChunkState::Clean,
        DIRTY => SparseChunkState::Dirty,
        _ => SparseChunkState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use crate::{
        BasicBlock, InstanceId, RegisterId, RegisterType, SIROffset, SIRTerminator, STABLE_REGION,
    };
    use celox_design::StateObjectId as VarId;
    use celox_state_layout::MemoryLayoutMode;

    fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(variable),
        }
    }

    fn store(variable: u32, source: RegisterId) -> SIRInstruction<RegionedAbsoluteAddr> {
        store_at(variable, 0, source)
    }

    fn store_at(
        variable: u32,
        bit_offset: usize,
        source: RegisterId,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(crate::SPARSE_WORKING_REGION, variable),
            SIROffset::Static(bit_offset),
            1,
            source,
            vec![],
            vec![],
        )
    }

    fn commit(variable: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Commit(
            address(crate::SPARSE_WORKING_REGION, variable),
            address(STABLE_REGION, variable),
            SIROffset::Static(0),
            1,
            vec![],
        )
    }

    fn eu(
        blocks: impl IntoIterator<Item = (BlockId, BasicBlock<RegionedAbsoluteAddr>)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().collect(),
            register_map: [(
                RegisterId(0),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            )]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        }
    }

    fn layout() -> MemoryLayout {
        let sparse_layouts = (0..=1)
            .map(|variable| {
                (
                    address(crate::SPARSE_WORKING_REGION, variable).absolute_addr(),
                    celox_state_layout::SparseWorkingLayout {
                        active_index: variable as usize,
                        chunk_count: 128,
                        dirty_words_offset: 0,
                        dirty_word_count: 1,
                        summary_words_offset: 0,
                        summary_word_count: 1,
                    },
                )
            })
            .collect();
        MemoryLayout {
            four_state: false,
            mode: MemoryLayoutMode::Packed,
            unpacked_arrays: HashMap::default(),
            offsets: HashMap::default(),
            widths: HashMap::default(),
            is_4states: HashMap::default(),
            total_size: 0,
            working_offsets: HashMap::default(),
            working_base_offset: 0,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: 0,
            sparse_layouts,
            sparse_active_bits_offset: 0,
            sparse_active_capacity: 0,
            merged_total_size: 0,
            triggered_bits_offset: 0,
            triggered_bits_total_size: 0,
            scratch_base_offset: 0,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        }
    }

    fn analyze(
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
        commit_block: BlockId,
        commit_start: usize,
    ) -> SparseWriteStates {
        SparseWriteStates::analyze(unit, &layout(), commit_block, commit_start).unwrap()
    }

    fn zero_fill_layout() -> MemoryLayout {
        let mut layout = layout();
        for variable in 0..=1 {
            let object = address(crate::SPARSE_WORKING_REGION, variable).absolute_addr();
            layout.widths.insert(object, 128);
            let sparse = layout.sparse_layouts.get_mut(&object).unwrap();
            sparse.chunk_count = 2;
            sparse.dirty_word_count = 1;
            sparse.summary_word_count = 1;
        }
        layout
    }

    fn zero_fill_unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [RegisterId(0), RegisterId(1)]
                .map(|register| (register, RegisterType::Logic { width: 64 }))
                .into_iter()
                .collect(),
        }
    }

    fn zero_store(variable: u32, bit_offset: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        zero_store_in_region(crate::SPARSE_WORKING_REGION, variable, bit_offset)
    }

    fn zero_store_in_region(
        region: u32,
        variable: u32,
        bit_offset: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(region, variable),
            SIROffset::Static(bit_offset),
            64,
            RegisterId(0),
            vec![],
            vec![],
        )
    }

    #[test]
    fn zero_fill_crosses_only_unrelated_operations() {
        let unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            zero_store(0, 0),
            zero_store(1, 0),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![],
            },
            zero_store(0, 64),
            zero_store(1, 64),
        ]);

        let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
        for instruction in [1, 2, 4, 5] {
            assert!(plans.is_member(BlockId(0), instruction));
        }
        assert!(plans.is_dead_zero_definition(BlockId(0), 0));
        assert_eq!(
            plans.root(BlockId(0), 4),
            Some(address(crate::SPARSE_WORKING_REGION, 0))
        );
        assert_eq!(
            plans.root(BlockId(0), 5),
            Some(address(crate::SPARSE_WORKING_REGION, 1))
        );
    }

    #[test]
    fn direct_stable_zero_fill_keeps_the_sparse_origin_bulk_plan() {
        let unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            zero_store_in_region(STABLE_REGION, 0, 0),
            zero_store_in_region(STABLE_REGION, 0, 64),
        ]);

        let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
        assert!(plans.is_member(BlockId(0), 1));
        assert!(plans.is_member(BlockId(0), 2));
        assert_eq!(plans.root(BlockId(0), 2), Some(address(STABLE_REGION, 0)));
        assert!(plans.is_dead_zero_definition(BlockId(0), 0));
    }

    #[test]
    fn direct_stable_zero_fill_does_not_require_a_sparse_home() {
        let unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            zero_store_in_region(STABLE_REGION, 0, 0),
            zero_store_in_region(STABLE_REGION, 0, 64),
        ]);
        let mut layout = zero_fill_layout();
        layout
            .sparse_layouts
            .remove(&address(STABLE_REGION, 0).absolute_addr());

        let plans = find_sparse_zero_fills(&unit, &layout);
        assert!(plans.is_member(BlockId(0), 1));
        assert!(plans.is_member(BlockId(0), 2));
        assert_eq!(plans.root(BlockId(0), 2), Some(address(STABLE_REGION, 0)));
        assert!(plans.is_dead_zero_definition(BlockId(0), 0));
    }

    #[test]
    fn same_object_read_prevents_zero_fill_reordering() {
        let unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            zero_store(0, 0),
            SIRInstruction::Load(
                RegisterId(1),
                address(crate::SPARSE_WORKING_REGION, 0),
                SIROffset::Static(0),
                64,
            ),
            zero_store(0, 64),
        ]);

        let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
        assert!(!plans.is_member(BlockId(0), 1));
        assert!(!plans.is_member(BlockId(0), 3));
    }

    #[test]
    fn partial_zero_overwrite_is_not_a_fill() {
        let unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            zero_store(0, 0),
        ]);

        let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
        assert!(!plans.is_member(BlockId(0), 1));
        assert!(!plans.is_dead_zero_definition(BlockId(0), 0));
    }

    #[test]
    fn zero_fill_elides_its_wide_concat_tree() {
        let mut unit = zero_fill_unit(vec![
            SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
            SIRInstruction::Concat(RegisterId(1), vec![RegisterId(0), RegisterId(0)]),
            SIRInstruction::Store(
                address(crate::SPARSE_WORKING_REGION, 0),
                SIROffset::Static(0),
                128,
                RegisterId(1),
                vec![],
                vec![],
            ),
        ]);
        unit.register_map
            .insert(RegisterId(1), RegisterType::Logic { width: 128 });

        let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
        assert!(plans.is_member(BlockId(0), 2));
        assert!(plans.is_dead_zero_definition(BlockId(0), 1));
        assert!(plans.is_dead_zero_definition(BlockId(0), 0));
    }

    #[test]
    fn straight_line_marks_only_each_objects_first_store() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store(0, RegisterId(0)),
                    store(0, RegisterId(0)),
                    store_at(0, 64, RegisterId(0)),
                    store_at(0, 4096, RegisterId(0)),
                    store(1, RegisterId(0)),
                    commit(0),
                    commit(1),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 5);

        assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(0), 1), SparseWriteState::Active);
        assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::Active);
        assert_eq!(facts.state(BlockId(0), 3), SparseWriteState::Active);
        assert_eq!(facts.state(BlockId(0), 4), SparseWriteState::First);
        assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Dirty);
        assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(0), 3), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(0), 4), SparseChunkState::Clean);
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 0),
            SparseChunkState::Clean
        );
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 1),
            SparseChunkState::Dirty
        );
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 2),
            SparseChunkState::Dirty
        );
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 3),
            SparseChunkState::Clean
        );
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 4),
            SparseChunkState::Clean
        );
    }

    #[test]
    fn straight_line_clean_chunks_share_one_deferred_metadata_update() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store_at(0, 0, RegisterId(0)),
                    store_at(0, 64, RegisterId(0)),
                    store_at(0, 128, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 3);

        assert_eq!(
            facts.metadata_action(BlockId(0), 0),
            SparseMetadataAction::Deferred
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 1),
            SparseMetadataAction::Deferred
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 2),
            SparseMetadataAction::Batch {
                dirty_word: 0,
                dirty_mask: 0b111,
                initial_write_state: SparseWriteState::First,
                initial_dirty_word_state: SparseChunkState::Clean,
            }
        );
    }

    #[test]
    fn commit_closes_a_metadata_batch() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store_at(0, 0, RegisterId(0)),
                    commit(0),
                    store_at(0, 64, RegisterId(0)),
                    store_at(0, 128, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 4);

        assert_eq!(
            facts.metadata_action(BlockId(0), 0),
            SparseMetadataAction::Immediate
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 2),
            SparseMetadataAction::Deferred
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 3),
            SparseMetadataAction::Batch {
                dirty_word: 0,
                dirty_mask: 0b110,
                initial_write_state: SparseWriteState::First,
                initial_dirty_word_state: SparseChunkState::Clean,
            }
        );
    }

    #[test]
    fn runtime_event_closes_a_metadata_batch() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store_at(0, 0, RegisterId(0)),
                    SIRInstruction::RuntimeEvent {
                        site_id: 0,
                        args: vec![],
                    },
                    store_at(0, 64, RegisterId(0)),
                    store_at(0, 128, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 4);

        assert_eq!(
            facts.metadata_action(BlockId(0), 0),
            SparseMetadataAction::Immediate
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 2),
            SparseMetadataAction::Deferred
        );
        assert!(matches!(
            facts.metadata_action(BlockId(0), 3),
            SparseMetadataAction::Batch {
                dirty_mask: 0b110,
                ..
            }
        ));
    }

    #[test]
    fn mutually_exclusive_arm_stores_are_both_first_writes() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0)), commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = analyze(&unit, BlockId(3), 1);

        assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(3), 0), SparseWriteState::Active);
        assert_eq!(facts.chunk_state(BlockId(1), 0), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(3), 0), SparseChunkState::Dirty);
        assert_eq!(
            facts.dirty_word_state(BlockId(3), 0),
            SparseChunkState::Dirty
        );
    }

    #[test]
    fn join_after_a_maybe_store_is_not_a_first_write() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(4), vec![]),
                },
            ),
            (
                BlockId(4),
                BasicBlock {
                    id: BlockId(4),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = analyze(&unit, BlockId(4), 0);

        assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(3), 0), SparseWriteState::Unknown);
        assert_eq!(facts.chunk_state(BlockId(3), 0), SparseChunkState::Unknown);
        assert_eq!(
            facts.dirty_word_state(BlockId(3), 0),
            SparseChunkState::Unknown
        );
    }

    #[test]
    fn loop_backedge_prevents_a_first_write_certificate() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), vec![]),
                        false_block: (BlockId(3), vec![]),
                    },
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = analyze(&unit, BlockId(3), 0);

        assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::Unknown);
        assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Unknown);
        assert_eq!(
            facts.dirty_word_state(BlockId(2), 0),
            SparseChunkState::Unknown
        );
    }

    #[test]
    fn commit_reset_prevents_active_state_from_crossing_it() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store(0, RegisterId(0)),
                    commit(0),
                    store(0, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 3);

        assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::First);
        assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 2),
            SparseChunkState::Clean
        );
    }

    #[test]
    fn dynamic_store_only_blocks_a_reaching_chunk_proof() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store(0, RegisterId(0)),
                    SIRInstruction::Store(
                        address(crate::SPARSE_WORKING_REGION, 0),
                        SIROffset::Dynamic(RegisterId(0)),
                        1,
                        RegisterId(0),
                        vec![],
                        vec![],
                    ),
                    store_at(0, 64, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 3);

        assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(0), 1), SparseWriteState::Active);
        assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::Active);
        assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Clean);
        assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Unknown);
        assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Unknown);
        assert_eq!(
            facts.metadata_action(BlockId(0), 0),
            SparseMetadataAction::Immediate
        );
        assert_eq!(
            facts.metadata_action(BlockId(0), 2),
            SparseMetadataAction::Immediate
        );
    }

    #[test]
    fn dynamic_store_on_a_sibling_path_does_not_block_a_clean_chunk() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![SIRInstruction::Store(
                        address(crate::SPARSE_WORKING_REGION, 0),
                        SIROffset::Dynamic(RegisterId(0)),
                        1,
                        RegisterId(0),
                        vec![],
                        vec![],
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store_at(0, 64, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = analyze(&unit, BlockId(3), 0);

        assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::First);
        assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Clean);
        assert_eq!(
            facts.dirty_word_state(BlockId(2), 0),
            SparseChunkState::Clean
        );
    }

    #[test]
    fn static_multi_chunk_store_clobbers_only_its_exact_range() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Store(
                        address(crate::SPARSE_WORKING_REGION, 0),
                        SIROffset::Static(0),
                        128,
                        RegisterId(0),
                        vec![],
                        vec![],
                    ),
                    store_at(0, 64, RegisterId(0)),
                    store_at(0, 128, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = analyze(&unit, BlockId(0), 3);

        assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Unknown);
        assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Dirty);
        assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 1),
            SparseChunkState::Dirty
        );
        assert_eq!(
            facts.dirty_word_state(BlockId(0), 2),
            SparseChunkState::Dirty
        );
    }
}
