//! Partitioning of the combinational settle and its flags.

use super::*;

/// Smallest combinational program, in expression nodes, whose settle
/// [`SettlePartitionMode::Estimated`] partitions.
pub(super) const MIN_PARTITIONED_SETTLE_COST: usize = 20_000;

/// Expression nodes per partition of an estimated partitioned settle.
pub(super) const SETTLE_PARTITION_COST: usize = 32;

/// Bound flag tests, publication checks, and their compiler analyses on large
/// designs instead of growing the number of partitions with every source node.
///
/// This cap and [`SETTLE_PARTITION_COST`] are a heuristic, not a derived
/// optimum. Finer partitions pay off only when a design is large and its
/// per-cycle activity is sparse and localized (most logic idle in most
/// settles, changes confined to narrow slices), with shallow dependency
/// propagation; a design whose logic toggles densely gains nothing and
/// pays the extra flag tests and partition runs. The 8192 cap (target
/// cost 32 instead of 51 on a ~208k-node program) was tuned on the
/// RTLMeter designs: XuanTie-E906 gained ~10% steady throughput, E902 was
/// unchanged, and both finer (16/24) and coarser (128) targets were
/// slower. Revisit these numbers with `--settle-profile` evidence (runs
/// per settle, runs changing nothing compared) before changing them for a
/// new design class; an activity-adaptive choice would be preferable.
pub(super) const TARGET_ESTIMATED_SETTLE_PARTITIONS: usize = 8192;

pub(super) fn estimated_settle_partition_cost(cost: usize) -> usize {
    SETTLE_PARTITION_COST.max(cost.div_ceil(TARGET_ESTIMATED_SETTLE_PARTITIONS))
}

/// The cost per partition of a partitioned settle of `comb_blocks`, or
/// `None` to keep one pass.
///
/// A partition costs a flag test per settle and flag stores in every writer
/// of what it reads, and pays off when most of the program is idle in most
/// settles. A design with few event domains drives most of its logic from
/// one clock, so it keeps the single pass, as does a small program.
pub(super) fn settle_partitioning(
    parallel: &ParallelScheduleOptions,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    events: usize,
) -> Option<usize> {
    // Lane-partitioned kernels run the comb program as lane tasks.
    if parallel.enabled() {
        return None;
    }
    match parallel.settle {
        SettlePartitionMode::Off => None,
        SettlePartitionMode::Always => Some(1),
        SettlePartitionMode::Estimated => {
            if events <= MERGED_COMB_APPLY_EVENTS {
                return None;
            }
            let cost: usize = comb_blocks
                .iter()
                .map(|path| scheduler::path_cost(arena, path))
                .sum();
            (cost >= MIN_PARTITIONED_SETTLE_COST).then(|| estimated_settle_partition_cost(cost))
        }
    }
}

/// Declare the `bytes` flags of a partitioned settle, one per partition,
/// the untracked byte and one per process for its edge wait, as a hidden
/// variable of the top module.
pub(super) fn declare_settle_flags(
    modules: &mut HashMap<ModuleId, SimModule>,
    root: ModuleId,
    var_id: SourceVarId,
    bytes: usize,
) {
    let width = bytes * 8;
    let variables = &mut modules.get_mut(&root).expect("top module").variables;
    variables.insert(
        var_id,
        crate::symbolic::artifact::SymbolicVariable {
            path: vec!["$settle".to_string(), "flags".to_string()],
            kind: crate::VariableKind::Variable,
            signed: false,
            metadata: VariableMetadata {
                width,
                is_4state: false,
                kind: DomainKind::Other,
                type_kind: celox_design::PortTypeKind::Bit,
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
            packed_dims: vec![width],
            unpacked_ranges: Vec::new(),
            source: None,
            module_affiliated: false,
        },
    );
}
