//! Sparse late forwarding for direct SimState round trips.
//!
//! Physical byte ranges and reaching memory definitions are provided by the
//! IR-independent celox-analysis MemorySSA. This adapter only maps MIR effects
//! and selects exact same-shaped Store-to-Load clusters.

use std::collections::BTreeMap;
use std::fmt;

use celox_analysis::memory::{MemoryEffect, MemoryLocation, effects_may_alias};
use celox_analysis::memory_ssa::{
    self, ClobberWalker, MemoryAccess, MemoryAccessEvent, MemoryClobber,
};

use crate::backend::native::memory_effect::{self, MemoryObject, analysis_effects};
use crate::backend::native::mir::{BaseReg, BlockId, MFunction, MInst, OpSize, SpillDesc, VReg};

use super::cfg::NormalizedCfg;

type ProgramPoint = (usize, usize);

#[derive(Debug)]
struct ForwardMemoryAnalysis {
    memory: ForwardMemoryState,
}

#[derive(Debug)]
struct ForwardMemoryState {
    reads: Vec<ForwardMemoryRead>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForwardMemoryRead {
    instruction: ProgramPoint,
    read_index: usize,
    effect: MemoryEffect<MemoryObject>,
    clobber: ForwardClobber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardClobber {
    LiveOnEntry,
    Definition(ProgramPoint),
    Phi(usize),
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatePromotionError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl StatePromotionError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            message: message.into(),
        }
    }
}

impl fmt::Display for StatePromotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for StatePromotionError {}

fn exact_location(base: BaseReg, offset: i32, size: OpSize) -> MemoryLocation<MemoryObject> {
    MemoryLocation {
        object: MemoryObject::direct(base),
        offset: i64::from(offset),
        byte_len: size.bytes() as usize,
    }
}

fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<ForwardMemoryAnalysis, StatePromotionError> {
    if func.blocks.len() != cfg.successors.len() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.CFG_SHAPE",
            None,
            None,
            "normalized CFG does not cover every MIR block",
        ));
    }
    let mut events =
        vec![Vec::<MemoryAccessEvent<ProgramPoint, ProgramPoint>>::new(); func.blocks.len()];
    let mut writes_by_definition = BTreeMap::<ProgramPoint, Vec<MemoryEffect<MemoryObject>>>::new();
    let mut pending_reads = Vec::<(ProgramPoint, usize, MemoryEffect<MemoryObject>)>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let point = (block, instruction);
            let write_effects = memory_effect::writes(inst);
            let mut reads = Vec::new();
            let writes = analysis_effects(&write_effects).collect::<Vec<_>>();

            if let MInst::Load {
                base: BaseReg::SimState,
                offset,
                size,
                ..
            } = inst
            {
                let location = exact_location(BaseReg::SimState, *offset, *size);
                reads.push(MemoryEffect::Exact(location));
            }
            if !reads.is_empty() || !writes.is_empty() {
                for &effect in reads.iter().chain(&writes) {
                    validate_memory_effect(func, block, instruction, effect)?;
                }
                for (read_index, &effect) in reads.iter().enumerate() {
                    pending_reads.push((point, read_index, effect));
                }
                let definition = if writes.is_empty() {
                    None
                } else {
                    if writes_by_definition.insert(point, writes).is_some() {
                        return Err(StatePromotionError::new(
                            "STATE_PROMOTE.DEFINITION_IDENTITY",
                            Some(mir_block.id),
                            Some(instruction),
                            "one MIR instruction produced multiple MemoryDef records",
                        ));
                    }
                    Some(point)
                };
                events[block].push(MemoryAccessEvent { point, definition });
            }
        }
    }

    let (graph, points) = memory_ssa::build(cfg, &events).map_err(|error| {
        StatePromotionError::new(
            error.rule,
            error
                .block
                .and_then(|block| func.blocks.get(block).map(|mir_block| mir_block.id)),
            None,
            error.message,
        )
    })?;
    let oracle = |definition: &ProgramPoint, query: &MemoryEffect<MemoryObject>| {
        writes_by_definition.get(definition).is_some_and(|writes| {
            writes
                .iter()
                .copied()
                .any(|write| effects_may_alias(write, *query))
        })
    };
    let mut walker = ClobberWalker::new();
    let mut reads = Vec::with_capacity(pending_reads.len());
    for (instruction, read_index, effect) in pending_reads {
        let start = points
            .event(instruction)
            .map(|point| point.before)
            .ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.READ_POINT",
                    func.blocks.get(instruction.0).map(|block| block.id),
                    Some(instruction.1),
                    "MemorySSA has no coordinate for an analyzed load",
                )
            })?;
        let clobber = walker
            .clobber(&graph, start, &effect, &oracle)
            .ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.READ_ACCESS",
                    func.blocks.get(instruction.0).map(|block| block.id),
                    Some(instruction.1),
                    "MemorySSA read starts at an invalid access",
                )
            })?;
        let clobber = match clobber {
            MemoryClobber::Indeterminate => ForwardClobber::Indeterminate,
            MemoryClobber::Access(access) => match graph.access(access).ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.CLOBBER_ACCESS",
                    func.blocks.get(instruction.0).map(|block| block.id),
                    Some(instruction.1),
                    "clobber walker returned an invalid graph access",
                )
            })? {
                MemoryAccess::LiveOnEntry => ForwardClobber::LiveOnEntry,
                MemoryAccess::Definition { definition, .. } => {
                    ForwardClobber::Definition(*definition)
                }
                MemoryAccess::Phi { block, .. } => ForwardClobber::Phi(block),
            },
        };
        reads.push(ForwardMemoryRead {
            instruction,
            read_index,
            effect,
            clobber,
        });
    }
    let memory = ForwardMemoryState { reads };
    Ok(ForwardMemoryAnalysis { memory })
}

fn validate_memory_effect(
    func: &MFunction,
    block: usize,
    instruction: usize,
    effect: MemoryEffect<MemoryObject>,
) -> Result<(), StatePromotionError> {
    let MemoryEffect::Exact(location) = effect else {
        return Ok(());
    };
    if location.byte_len == 0 || location.end().is_none() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.MEMORY_RANGE",
            func.blocks.get(block).map(|block| block.id),
            Some(instruction),
            "MIR memory effect has an empty or overflowing physical range",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForwardCandidate {
    store_block: usize,
    store_instruction: usize,
    block: usize,
    instruction: usize,
    destination: VReg,
    source: VReg,
    size: OpSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForwardCluster {
    store_block: usize,
    store_instruction: usize,
    source: VReg,
    size: OpSize,
    loads: Vec<ForwardCandidate>,
}

fn exact_forward_candidates(
    func: &MFunction,
    analysis: &ForwardMemoryAnalysis,
) -> Result<Vec<ForwardCandidate>, StatePromotionError> {
    let mut candidates = Vec::new();
    for read in &analysis.memory.reads {
        let load_location = read.instruction;
        let (load_block, load_instruction) = load_location;
        let Some(MInst::Load {
            dst: destination,
            base: BaseReg::SimState,
            offset: load_offset,
            size: load_size,
        }) = func
            .blocks
            .get(load_block)
            .and_then(|block| block.insts.get(load_instruction))
        else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.LOAD_ACCESS",
                func.blocks.get(load_block).map(|block| block.id),
                Some(load_instruction),
                "analyzed load no longer matches MIR",
            ));
        };
        if read.read_index != 0 {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.LOAD_QUERY",
                func.blocks.get(load_block).map(|block| block.id),
                Some(load_instruction),
                "direct MIR load has an unexpected MemorySSA query index",
            ));
        }
        let expected_location = exact_location(BaseReg::SimState, *load_offset, *load_size);
        if read.effect != MemoryEffect::Exact(expected_location) {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.LOAD_LOCATION",
                func.blocks.get(load_block).map(|block| block.id),
                Some(load_instruction),
                "MemorySSA query location differs from the direct MIR load",
            ));
        }

        let ForwardClobber::Definition(store_location) = read.clobber else {
            continue;
        };
        let Some(MInst::Store {
            base: BaseReg::SimState,
            offset: store_offset,
            size: store_size,
            src: store_source,
        }) = func
            .blocks
            .get(store_location.0)
            .and_then(|block| block.insts.get(store_location.1))
        else {
            // A bounded indexed write or another exact MIR write can be the
            // reaching byte definition, but it is not a forwardable store.
            continue;
        };
        if load_offset != store_offset || load_size != store_size || store_source == destination {
            continue;
        }
        candidates.push(ForwardCandidate {
            store_block: store_location.0,
            store_instruction: store_location.1,
            block: load_block,
            instruction: load_instruction,
            destination: *destination,
            source: *store_source,
            size: *load_size,
        });
    }
    candidates.sort_unstable_by_key(|candidate| (candidate.block, candidate.instruction));
    Ok(candidates)
}

fn cluster_candidates(
    candidates: Vec<ForwardCandidate>,
) -> Result<Vec<ForwardCluster>, StatePromotionError> {
    let mut by_store = BTreeMap::<(usize, usize), ForwardCluster>::new();
    for candidate in candidates {
        let key = (candidate.store_block, candidate.store_instruction);
        let cluster = by_store.entry(key).or_insert_with(|| ForwardCluster {
            store_block: candidate.store_block,
            store_instruction: candidate.store_instruction,
            source: candidate.source,
            size: candidate.size,
            loads: Vec::new(),
        });
        if cluster.source != candidate.source || cluster.size != candidate.size {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.CLUSTER_STORE",
                None,
                None,
                "one physical store location names incompatible forwarding clusters",
            ));
        }
        cluster.loads.push(candidate);
    }
    for cluster in by_store.values_mut() {
        cluster
            .loads
            .sort_unstable_by_key(|candidate| (candidate.block, candidate.instruction));
    }
    Ok(by_store.into_values().collect())
}

fn allocate_cluster_value(func: &mut MFunction) -> Result<VReg, StatePromotionError> {
    let value = func.vregs.alloc();
    if value.0 as usize != func.spill_descs.len() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.VALUE_DOMAIN",
            None,
            None,
            "MIR VReg allocator and spill-descriptor table are not dense",
        ));
    }
    func.spill_descs.push(SpillDesc::transient());
    Ok(value)
}

fn canonicalize_store_value(
    func: &mut MFunction,
    source: VReg,
    size: OpSize,
    canonical_bits: &[u8],
) -> Result<(VReg, Option<MInst>), StatePromotionError> {
    let stored_bits = u8::try_from(size.bytes() * 8).expect("native store width fits u8");
    let source_bits = canonical_bits.get(source.0 as usize).ok_or_else(|| {
        StatePromotionError::new(
            "STATE_PROMOTE.CANONICAL_VALUE",
            None,
            None,
            "store source is outside the canonical-value side table",
        )
    })?;
    if *source_bits <= stored_bits {
        return Ok((source, None));
    }
    let canonical = allocate_cluster_value(func)?;
    let normalization = match size {
        OpSize::S8 => MInst::AndImm32 {
            dst: canonical,
            src: source,
            imm: u8::MAX.into(),
        },
        OpSize::S16 => MInst::AndImm32 {
            dst: canonical,
            src: source,
            imm: u16::MAX.into(),
        },
        OpSize::S32 => MInst::Mov32 {
            dst: canonical,
            src: source,
        },
        OpSize::S64 => unreachable!("64-bit stores need no canonicalization"),
    };
    Ok((canonical, Some(normalization)))
}

/// Forward exact same-shaped state round trips after pressure scheduling.
///
/// Each original store remains the packed-state home. Loads reached by that
/// exact store share one canonical store value and become ordinary copy
/// affinities. If the cluster is evicted, point-specific MemorySSA
/// rematerialization recreates a load at the corresponding use. No state cell,
/// terminal writeback, or synthetic phi is added here.
pub(super) fn forward_exact_round_trips(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
) -> Result<usize, StatePromotionError> {
    let analysis = analyze(func, cfg)?;
    let candidates = exact_forward_candidates(func, &analysis)?;
    if candidates.is_empty() {
        return Ok(0);
    }

    let mut rewritten = func.clone();
    let clusters = cluster_candidates(candidates)?;
    let canonical_bits = super::reload::canonical_value_bits(&rewritten).map_err(|error| {
        StatePromotionError::new(
            error.rule,
            error.block,
            error.instruction,
            format!("canonical store-value analysis failed: {}", error.message),
        )
    })?;
    let mut stores = BTreeMap::<(usize, usize), (VReg, Option<MInst>)>::new();
    let mut loads = BTreeMap::<(usize, usize), (VReg, VReg)>::new();
    let mut forwarded = 0usize;
    for cluster in clusters {
        let (canonical, normalization) = canonicalize_store_value(
            &mut rewritten,
            cluster.source,
            cluster.size,
            &canonical_bits,
        )?;
        stores.insert(
            (cluster.store_block, cluster.store_instruction),
            (canonical, normalization),
        );
        for load in cluster.loads {
            if loads
                .insert(
                    (load.block, load.instruction),
                    (load.destination, canonical),
                )
                .is_some()
            {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.CLUSTER_LOAD",
                    rewritten.blocks.get(load.block).map(|block| block.id),
                    Some(load.instruction),
                    "one load location belongs to multiple forwarding clusters",
                ));
            }
            rewritten.spill_descs[load.destination.0 as usize] = SpillDesc::transient();
            forwarded = forwarded.saturating_add(1);
        }
    }

    for block in 0..rewritten.blocks.len() {
        let original = std::mem::take(&mut rewritten.blocks[block].insts);
        let mut instructions = Vec::with_capacity(original.len());
        for (instruction, mut inst) in original.into_iter().enumerate() {
            if let Some(&(canonical, ref normalization)) = stores.get(&(block, instruction)) {
                if let Some(normalization) = normalization {
                    instructions.push(normalization.clone());
                }
                let MInst::Store {
                    base: BaseReg::SimState,
                    src,
                    ..
                } = &mut inst
                else {
                    return Err(StatePromotionError::new(
                        "STATE_PROMOTE.CLUSTER_STORE",
                        Some(rewritten.blocks[block].id),
                        Some(instruction),
                        "forwarding cluster store no longer matches MIR",
                    ));
                };
                *src = canonical;
            }
            if let Some(&(destination, canonical)) = loads.get(&(block, instruction)) {
                instructions.push(MInst::Mov {
                    dst: destination,
                    src: canonical,
                });
            } else {
                instructions.push(inst);
            }
        }
        rewritten.blocks[block].insts = instructions;
    }
    rewritten.verify_result().map_err(|error| {
        StatePromotionError::new(
            "STATE_PROMOTE.MIR_VERIFY",
            None,
            None,
            format!("state-forwarded MIR failed canonical verification: {error}"),
        )
    })?;
    *func = rewritten;
    Ok(forwarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, MemoryAliasRange, VRegAllocator};

    fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function = MFunction::new(
            values,
            (0..value_count).map(|_| SpillDesc::transient()).collect(),
        );
        function.blocks = blocks;
        function
    }

    fn normalize(function: &mut MFunction) -> NormalizedCfg {
        super::super::cfg::normalize(function).unwrap()
    }

    #[test]
    fn same_store_load_cluster_shares_one_width_normalization() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x1ff,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 2);
        assert!(matches!(
            function.blocks[0].insts[1],
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 0xff,
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[2],
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(3),
                size: OpSize::S8,
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(3),
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[5],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(3),
            }
        ));
        assert_eq!(function.vregs.count(), 4);
    }

    #[test]
    fn already_canonical_narrow_store_reuses_its_source_value() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 3,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Return,
        ];
        let mut function = function(2, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        assert_eq!(function.vregs.count(), 2);
        assert!(matches!(
            function.blocks[0].insts[1],
            MInst::Store {
                src: VReg(0),
                size: OpSize::S8,
                ..
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        ));
    }

    #[test]
    fn differently_reaching_overlapping_store_keeps_the_original_load() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 0);
        assert!(matches!(
            function.blocks[0].insts[4],
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn one_arm_store_keeps_the_join_load_as_memory() {
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut dirty = MBlock::new(BlockId(1));
        dirty.insts = vec![
            MInst::LoadImm {
                dst: VReg(1),
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut clean = MBlock::new(BlockId(2));
        clean.insts = vec![MInst::Jump { target: BlockId(3) }];
        let mut join = MBlock::new(BlockId(3));
        join.insts = vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![entry, dirty, clean, join]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 0);
        let join = cfg.block_index[&BlockId(3)];
        assert!(function.blocks[join].phis.is_empty());
        assert!(matches!(
            function.blocks[join].insts[0],
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn indexed_reads_do_not_kill_forwardable_state_versions() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadIndexed {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(64, 8),
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 5,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);
        let analysis = analyze(&function, &cfg).unwrap();
        assert!(analysis.memory.reads.is_empty());
    }

    #[test]
    fn sparse_commit_reads_are_not_memory_ssa_queries() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::SparseCommit {
                src_offset: 1_000_000,
                dst_offset: 2_000_000,
                byte_size: 16 * 1024 * 1024,
                dirty_words_offset: 3_000_000,
                dirty_word_count: 1,
                summary_words_offset: 4_000_000,
                summary_word_count: 1,
                four_state: false,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(2, vec![block]);
        let cfg = normalize(&mut function);
        let analysis = analyze(&function, &cfg).unwrap();

        assert_eq!(analysis.memory.reads.len(), 1);
        assert_eq!(
            analysis.memory.reads[0].clobber,
            ForwardClobber::Definition((0, 1))
        );
    }

    #[test]
    fn bounded_indexed_store_kills_only_the_reaching_component() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 5,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 64,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(64, 8),
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(4, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        assert!(matches!(
            function.blocks[0].insts[5],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[6],
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn unknown_write_on_one_arm_does_not_blacklist_the_clean_arm() {
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut dirty = MBlock::new(BlockId(1));
        dirty.insts = vec![
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S64,
                alias_range: None,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut clean = MBlock::new(BlockId(2));
        clean.insts = vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut join = MBlock::new(BlockId(3));
        join.insts = vec![
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(4, vec![entry, dirty, clean, join]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        let clean = cfg.block_index[&BlockId(2)];
        let join = cfg.block_index[&BlockId(3)];
        assert!(matches!(
            function.blocks[clean].insts[0],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            }
        ));
        assert!(matches!(
            function.blocks[join].insts[0],
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn unknown_kills_are_one_sparse_generation_not_cells_times_kills() {
        const CELLS: usize = 512;
        const KILLS: usize = 32;
        let mut instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
        ];
        for cell in 0..CELLS {
            instructions.push(MInst::Store {
                base: BaseReg::SimState,
                offset: i32::try_from(cell * 16).unwrap(),
                src: VReg(1),
                size: OpSize::S64,
            });
        }
        for _ in 0..KILLS {
            instructions.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S64,
                alias_range: None,
            });
        }
        for cell in 0..CELLS {
            instructions.push(MInst::Load {
                dst: VReg(u32::try_from(cell + 2).unwrap()),
                base: BaseReg::SimState,
                offset: i32::try_from(cell * 16).unwrap(),
                size: OpSize::S64,
            });
        }
        instructions.push(MInst::Return);
        let mut function = function(
            (CELLS + 2) as u32,
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
        );
        let cfg = normalize(&mut function);
        let analysis = analyze(&function, &cfg).unwrap();

        assert_eq!(analysis.memory.reads.len(), CELLS);
        let last_kill = (0, 2 + CELLS + KILLS - 1);
        for read in &analysis.memory.reads {
            assert_eq!(read.clobber, ForwardClobber::Definition(last_kill));
        }
    }
}
