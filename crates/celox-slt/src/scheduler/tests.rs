use num_bigint::{BigInt, BigUint};

use super::{
    ExactFoldGroup, ExactIndexedLoadKey, FfAccessSummary, FoldGroupReadFacts, NormalizedIndexExpr,
    add_acyclic_ff_write_order_edges, best_weighted_fold_family, build_fold_group_schedule_index,
    build_logic_path_memory_ssa, collect_node_input_deps, direct_ff_write_ranges, index_accesses,
    indexed_overlaps, plan_ff_comb_schedule, prepare_atomic_fold_group_results, sort,
    stable_topological_sccs,
};
use crate::{HashMap, HashSet};
use crate::{
    LogicPath, LogicPathTarget, SLTForFoldGroupState, SLTNode, SLTNodeArena, SLTToSIRLowerer,
};
use celox_design::{BinaryOp, BitAccess, VarAtomBase};
use celox_sir::{SIRBuilder, SIRInstruction, SIROffset, SIRTerminator};

#[test]
fn deep_input_dependencies_use_heap_and_preserve_inverse_index() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut arena = SLTNodeArena::<usize>::new();
            let input = |variable| SLTNode::Input {
                variable,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
                signed: false,
            };
            let data = arena.alloc(input(0)).unwrap();
            let cond = arena.alloc(input(1)).unwrap();
            let mut root = data;
            for _ in 0..8192 {
                root = arena
                    .alloc(SLTNode::Mux {
                        cond,
                        then_expr: root,
                        else_expr: data,
                    })
                    .unwrap();
            }
            let mut memo = HashMap::default();
            let mut inverse = HashMap::default();
            let deps = collect_node_input_deps(root, &arena, &mut memo, &mut inverse);
            assert_eq!(deps, HashSet::from_iter([0, 1]));
            assert!(inverse[&0].contains(&root));
            assert!(inverse[&1].contains(&root));
            assert_eq!(memo.len(), 8194);
            assert_eq!(
                collect_node_input_deps(root, &arena, &mut memo, &mut inverse),
                deps
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn stable_scc_order_preserves_source_order_when_dependencies_allow_it() {
    // 0 and 1 are initially ready, both feed 2, and 2 feeds 3. Tarjan is
    // free to return components in any order; lowering remains stable.
    let adj = vec![vec![2], vec![2], vec![3], vec![]];
    let unordered = vec![vec![3], vec![1], vec![2], vec![0]];

    let (ordered, component_by_path) =
        stable_topological_sccs(unordered, &adj, &[None; 4]).unwrap();

    assert_eq!(ordered, vec![vec![0], vec![1], vec![2], vec![3]]);
    assert_eq!(component_by_path.len(), adj.len());
}

#[test]
fn stable_scc_order_drains_only_ready_members_of_an_effect_domain() {
    let adj = vec![vec![], vec![], vec![], vec![]];
    let unordered = vec![vec![3], vec![1], vec![2], vec![0]];

    let (ordered, _) =
        stable_topological_sccs(unordered, &adj, &[Some(0), Some(1), Some(0), Some(1)]).unwrap();

    assert_eq!(ordered, vec![vec![0], vec![2], vec![1], vec![3]]);
}

#[test]
fn effect_domain_preference_never_pulls_a_path_across_its_dependency() {
    // Path 1 shares path 0's destination domain, but it is not ready until
    // the independent path 2 has executed.
    let adj = vec![vec![], vec![], vec![1]];
    let unordered = vec![vec![1], vec![2], vec![0]];

    let (ordered, _) =
        stable_topological_sccs(unordered, &adj, &[Some(0), Some(0), Some(1)]).unwrap();

    assert_eq!(ordered, vec![vec![0], vec![2], vec![1]]);
}

#[test]
fn id_indexed_overlap_probes_match_a_naive_scan() {
    // The scheduling passes index one side of each overlap test by object
    // id. A probe must agree with scanning the whole access list, including
    // when objects are interleaved or a probe shares no object with it.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..256 {
        let accesses = (0..(next() % 12) as usize)
            .map(|_| {
                let lsb = (next() % 32) as usize;
                VarAtomBase::new((next() % 5) as u32, lsb, lsb + (next() % 8) as usize)
            })
            .collect::<Vec<_>>();
        let probe_lsb = (next() % 32) as usize;
        let probe = VarAtomBase::new(
            (next() % 5) as u32,
            probe_lsb,
            probe_lsb + (next() % 8) as usize,
        );

        let index = index_accesses(&accesses);
        let naive = accesses
            .iter()
            .any(|access| access.id == probe.id && access.access.overlaps(&probe.access));
        assert_eq!(indexed_overlaps(&index, &probe), naive);
    }
}

fn simple_path(arena: &mut SLTNodeArena<u32>, target: u32, source: Option<u32>) -> LogicPath<u32> {
    let expr = if let Some(source) = source {
        arena
            .alloc(SLTNode::Input {
                variable: source,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap()
    } else {
        arena
            .alloc(SLTNode::Constant(
                BigUint::from(target),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap()
    };
    LogicPath {
        transparent_copy: false,
        target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
        sources: source
            .map(|source| [VarAtomBase::new(source, 0, 7)].into_iter().collect())
            .unwrap_or_default(),
        previous_sources: crate::HashSet::default(),
        address_sources: crate::HashSet::default(),
        local_inputs: Vec::new(),
        order_before: crate::HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr,
    }
}

#[test]
fn scheduled_paths_share_one_high_level_guard_without_changing_store_order() {
    let mut arena = SLTNodeArena::new();
    let condition = arena
        .alloc(SLTNode::Input {
            variable: 100,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let mut paths = Vec::new();
    for target in 0u32..12 {
        let then_expr = arena
            .alloc(SLTNode::Constant(
                BigUint::from(target),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let else_expr = arena
            .alloc(SLTNode::Constant(
                BigUint::from(target + 32),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let expr = arena
            .alloc(SLTNode::Mux {
                cond: condition,
                then_expr,
                else_expr,
            })
            .unwrap();
        paths.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: [VarAtomBase::new(100, 0, 0)].into_iter().collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
    }

    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &crate::HashMap::default(),
        0,
    )
    .unwrap();
    let eu = &result.execution_units[0];
    let branches = eu
        .blocks
        .values()
        .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
        .collect::<Vec<_>>();
    assert_eq!(branches.len(), 1);
    assert_eq!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
            .count(),
        0
    );

    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &branches[0].terminator
    else {
        unreachable!()
    };
    for block in [true_block.0, false_block.0] {
        let stores = eu.blocks[&block]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(target, ..) => Some(*target),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores, (0..12).collect::<Vec<_>>());
    }
}

#[test]
fn scheduled_outer_guard_keeps_nested_mux_lowering_decisions() {
    let mut arena = SLTNodeArena::new();
    let outer_condition = arena
        .alloc(SLTNode::Input {
            variable: 100,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let inner_condition = arena
        .alloc(SLTNode::Input {
            variable: 101,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let input_value = arena
        .alloc(SLTNode::Input {
            variable: 102,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let one = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            8,
            false,
        ))
        .unwrap();
    let mut inner_true = input_value;
    let mut inner_false = input_value;
    for _ in 0..16 {
        inner_true = arena
            .alloc(SLTNode::Binary(inner_true, BinaryOp::Add, one))
            .unwrap();
        inner_false = arena
            .alloc(SLTNode::Binary(inner_false, BinaryOp::Xor, one))
            .unwrap();
    }
    let nested = arena
        .alloc(SLTNode::Mux {
            cond: inner_condition,
            then_expr: inner_true,
            else_expr: inner_false,
        })
        .unwrap();

    let mut paths = Vec::new();
    for target in 0u32..12 {
        let then_expr = if target == 0 {
            nested
        } else {
            arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap()
        };
        let else_expr = arena
            .alloc(SLTNode::Constant(
                BigUint::from(target + 32),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let expr = arena
            .alloc(SLTNode::Mux {
                cond: outer_condition,
                then_expr,
                else_expr,
            })
            .unwrap();
        paths.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: [
                VarAtomBase::new(100, 0, 0),
                VarAtomBase::new(101, 0, 0),
                VarAtomBase::new(102, 0, 7),
            ]
            .into_iter()
            .collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
    }

    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &crate::HashMap::default(),
        0,
    )
    .unwrap();
    let branch_count = result.execution_units[0]
        .blocks
        .values()
        .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
        .count();
    assert_eq!(
        branch_count, 2,
        "outer region and nested Mux must both branch"
    );
}

#[test]
fn ff_plan_models_comb_and_ff_old_state_readers_before_writer() {
    let mut arena = SLTNodeArena::new();
    let comb_value = simple_path(&mut arena, 10, None);
    let comb_old_state_reader = simple_path(&mut arena, 11, Some(20));
    let ff = vec![
        FfAccessSummary {
            reads: vec![VarAtomBase::new(10, 0, 7), VarAtomBase::new(11, 0, 7)],
            writes: vec![VarAtomBase::new(20, 0, 7)],
            dynamic_writes: HashSet::default(),
        },
        FfAccessSummary {
            reads: vec![VarAtomBase::new(20, 0, 7)],
            writes: vec![VarAtomBase::new(30, 0, 7)],
            dynamic_writes: HashSet::default(),
        },
    ];

    let plan = plan_ff_comb_schedule(&[comb_value, comb_old_state_reader], &ff).unwrap();

    assert_eq!(plan.required_comb, vec![true, true]);
    assert_eq!(plan.comb_value_predecessors, vec![vec![0, 1], vec![]]);
    assert_eq!(plan.comb_before_direct_write, vec![vec![1], vec![]]);
    assert_eq!(plan.ff_before_direct_write, vec![vec![1], vec![]]);
}

#[test]
fn ff_plan_keeps_same_recipe_old_state_reads_local_to_lowering() {
    let summary = FfAccessSummary {
        reads: vec![VarAtomBase::new(20, 0, 7)],
        writes: vec![VarAtomBase::new(20, 0, 7)],
        dynamic_writes: HashSet::default(),
    };

    let plan = plan_ff_comb_schedule(&[], &[summary]).unwrap();

    assert_eq!(plan.ff_before_direct_write, vec![Vec::<usize>::new()]);
}

#[test]
fn ff_direct_write_proof_is_kept_per_bit_range() {
    let summaries = vec![
        FfAccessSummary {
            reads: vec![VarAtomBase::new(30, 0, 7)],
            writes: vec![VarAtomBase::new(20, 0, 7), VarAtomBase::new(20, 8, 15)],
            dynamic_writes: HashSet::default(),
        },
        FfAccessSummary {
            reads: vec![VarAtomBase::new(20, 0, 7)],
            writes: vec![VarAtomBase::new(30, 0, 7)],
            dynamic_writes: HashSet::default(),
        },
    ];
    let plan = plan_ff_comb_schedule(&[], &summaries).unwrap();
    // The FF0 -> FF1 edge is retained, while the inverse edge which only
    // protects FF0's low byte is dropped to break the cycle.
    let retained = [(0, 1)].into_iter().collect();
    let var_widths = [(20, 16), (30, 8)].into_iter().collect();

    let direct = direct_ff_write_ranges(
        &[],
        &summaries,
        &plan,
        &retained,
        0,
        &var_widths,
        &HashMap::default(),
    );

    assert_eq!(direct[0], vec![VarAtomBase::new(20, 8, 15)]);
    assert_eq!(direct[1], vec![VarAtomBase::new(30, 0, 7)]);

    let local_old_read = vec![FfAccessSummary {
        reads: vec![VarAtomBase::new(20, 0, 7)],
        writes: vec![VarAtomBase::new(20, 0, 7), VarAtomBase::new(20, 8, 15)],
        dynamic_writes: HashSet::default(),
    }];
    let local_plan = plan_ff_comb_schedule(&[], &local_old_read).unwrap();
    let local_direct = direct_ff_write_ranges(
        &[],
        &local_old_read,
        &local_plan,
        &HashSet::default(),
        0,
        &var_widths,
        &HashMap::default(),
    );

    assert_eq!(local_direct[0], vec![VarAtomBase::new(20, 8, 15)]);
}

#[test]
fn ff_direct_write_rejects_ranges_requiring_rmw() {
    let summary = FfAccessSummary {
        reads: Vec::new(),
        writes: vec![
            VarAtomBase::new(20, 0, 3),
            VarAtomBase::new(21, 0, 0),
            VarAtomBase::new(22, 0, 7),
            VarAtomBase::new(23, 6, 11),
            VarAtomBase::new(24, 8, 15),
        ],
        dynamic_writes: [22].into_iter().collect(),
    };
    let plan = plan_ff_comb_schedule(&[], std::slice::from_ref(&summary)).unwrap();
    let var_widths = [(20, 8), (21, 1), (22, 8), (23, 24), (24, 24)]
        .into_iter()
        .collect();
    let unpacked_element_widths = [(23, 6), (24, 8)].into_iter().collect();

    let direct = direct_ff_write_ranges(
        &[],
        &[summary],
        &plan,
        &HashSet::default(),
        0,
        &var_widths,
        &unpacked_element_widths,
    );

    assert_eq!(
        direct[0],
        vec![VarAtomBase::new(21, 0, 0), VarAtomBase::new(24, 8, 15)]
    );
}

#[test]
fn ff_direct_write_stages_complete_overlapping_components() {
    let summary = FfAccessSummary {
        reads: Vec::new(),
        writes: vec![
            VarAtomBase::new(20, 4, 19),
            VarAtomBase::new(20, 16, 31),
            VarAtomBase::new(20, 24, 31),
            VarAtomBase::new(20, 40, 47),
        ],
        dynamic_writes: HashSet::default(),
    };
    let plan = plan_ff_comb_schedule(&[], std::slice::from_ref(&summary)).unwrap();
    let var_widths = [(20, 64)].into_iter().collect();

    let direct = direct_ff_write_ranges(
        &[],
        &[summary],
        &plan,
        &HashSet::default(),
        0,
        &var_widths,
        &HashMap::default(),
    );

    // The unaligned first write is staged. Staging propagates through the
    // two transitively overlapping native ranges, but not the disjoint byte.
    assert_eq!(direct[0], vec![VarAtomBase::new(20, 40, 47)]);
}

#[test]
fn cyclic_direct_write_preference_falls_back_to_staging() {
    let mut adj = vec![vec![1], Vec::new(), Vec::new()];

    add_acyclic_ff_write_order_edges(&mut adj, [(1, 0), (1, 2)]);

    assert_eq!(adj, vec![vec![1], vec![2], Vec::new()]);
}

fn partitioned_sort(
    paths: Vec<LogicPath<u32>>,
    arena: &SLTNodeArena<u32>,
) -> super::ScheduleResult<u32> {
    let widths = (10..14).map(|id| (id, 8)).collect();
    super::sort_with_unpacked_element_widths(
        paths,
        arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        &crate::HashMap::default(),
        1,
        Some(super::SettlePartitioning {
            flags: 99,
            target_cost: 1,
            external_readers: crate::HashMap::default(),
            clock_nets: crate::HashSet::default(),
            profile: None,
        }),
    )
    .unwrap()
}

#[test]
fn settle_partitions_follow_schedule_and_guard_on_their_flags() {
    let mut arena = SLTNodeArena::new();
    let paths = vec![
        simple_path(&mut arena, 13, Some(12)),
        simple_path(&mut arena, 12, Some(11)),
        simple_path(&mut arena, 11, Some(10)),
    ];
    let result = partitioned_sort(paths, &arena);
    let plan = result
        .partitions
        .iter()
        .map(|partition| (partition.reads.clone(), partition.writes.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        plan,
        vec![
            (vec![10], vec![11]),
            (vec![11], vec![12]),
            (vec![12], vec![13]),
        ]
    );
    assert!(result.partitions.iter().all(|partition| !partition.pinned));
    // The settle starts by reading the untracked byte (24). Each
    // partition loads its own flag, and sets its reader's flag when it
    // changed what it wrote.
    let mut loaded = Vec::new();
    let mut stored = Vec::new();
    let mut compares = 0;
    for unit in &result.execution_units {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Load(_, 99, SIROffset::Static(byte), 8) => loaded.push(*byte),
                    SIRInstruction::Store(99, SIROffset::Static(byte), 8, ..) => stored.push(*byte),
                    SIRInstruction::Binary(_, _, BinaryOp::NeCase, _) => compares += 1,
                    _ => {}
                }
            }
        }
    }
    loaded.sort_unstable();
    stored.sort_unstable();
    // The three guards read their flags. Publication stores 1 directly
    // without reading an individual reader's flag.
    assert_eq!(loaded, vec![0, 8, 16, 24]);
    // The clears of all three flags and the marks of partitions 1 and
    // 2; the untracked path fills the three flags with one wider store.
    assert_eq!(stored, vec![0, 8, 8, 16, 16]);
    // 11 and 12 are compared before their readers are marked.
    assert_eq!(compares, 2);
}

/// A partition that writes a slice of a wide variable compares that
/// slice before marking the readers, not the whole variable; an unpacked
/// array is compared in whole elements. A reader of other bits of the
/// variable is no reader of the slice.
#[test]
fn settle_partitions_compare_only_the_written_bits() {
    let loads_of_13 = |element_width: Option<usize>, read: (usize, usize)| {
        let mut arena = SLTNodeArena::new();
        let mut slice = simple_path(&mut arena, 13, Some(12));
        slice.target = LogicPathTarget::Var(VarAtomBase::new(13, 8, 15));
        let mut reader = simple_path(&mut arena, 14, Some(13));
        reader.expr = arena
            .alloc(SLTNode::Input {
                variable: 13,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(read.0, read.1),
            })
            .unwrap();
        reader.sources = [VarAtomBase::new(13, read.0, read.1)].into_iter().collect();
        let paths = vec![reader, slice];
        let widths = [(12, 8), (13, 64), (14, 8)].into_iter().collect();
        let elements = element_width.map(|width| (13, width)).into_iter().collect();
        let result = super::sort_with_unpacked_element_widths(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            &elements,
            1,
            Some(super::SettlePartitioning {
                flags: 99,
                target_cost: 1,
                external_readers: crate::HashMap::default(),
                clock_nets: crate::HashSet::default(),
                profile: None,
            }),
        )
        .unwrap();
        assert_eq!(result.partitions.len(), 2);
        let mut loads = Vec::new();
        for unit in &result.execution_units {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    if let SIRInstruction::Load(_, 13, offset, width) = instruction {
                        loads.push((offset.clone(), *width));
                    }
                }
            }
        }
        loads.sort_unstable();
        loads
    };
    // The reader's input, and the written slice before and after.
    assert_eq!(
        loads_of_13(None, (8, 15)),
        vec![(SIROffset::Static(8), 8); 3]
    );
    // The slice lies in the first 16-bit element.
    assert_eq!(
        loads_of_13(Some(16), (8, 15)),
        vec![
            (SIROffset::Static(0), 16),
            (SIROffset::Static(0), 16),
            (SIROffset::Static(8), 8),
        ]
    );
    // A reader of the other byte: only its input; the element holding
    // both bytes is still compared.
    assert_eq!(loads_of_13(None, (0, 7)), vec![(SIROffset::Static(0), 8)]);
    assert_eq!(
        loads_of_13(Some(16), (0, 7)),
        vec![
            (SIROffset::Static(0), 8),
            (SIROffset::Static(0), 16),
            (SIROffset::Static(0), 16),
        ]
    );
}

/// A partition that changes state an external reader watches sets the
/// reader's flag, which follows the partitions' bytes and the untracked
/// byte.
#[test]
fn settle_partitions_mark_external_readers() {
    let mut arena = SLTNodeArena::new();
    let paths = vec![
        simple_path(&mut arena, 13, Some(12)),
        simple_path(&mut arena, 12, Some(11)),
        simple_path(&mut arena, 11, Some(10)),
    ];
    let widths = (10..14).map(|id| (id, 8)).collect();
    let result = super::sort_with_unpacked_element_widths(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        &crate::HashMap::default(),
        1,
        Some(super::SettlePartitioning {
            flags: 99,
            target_cost: 1,
            external_readers: [(11, vec![1])].into_iter().collect(),
            clock_nets: crate::HashSet::default(),
            profile: None,
        }),
    )
    .unwrap();
    assert_eq!(result.partitions.len(), 3);
    let mut stored = Vec::new();
    for unit in &result.execution_units {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                if let SIRInstruction::Store(99, SIROffset::Static(bit), width, ..) = instruction {
                    stored.push((*bit, *width));
                }
            }
        }
    }
    stored.sort_unstable();
    // The three clears, the untracked fill, the mark of partition 2,
    // and the byte stores of partition 1's flag and of external flag 1
    // at byte 3 + 1 + 1, both set when 11 changed.
    assert_eq!(
        stored,
        vec![(0, 8), (0, 24), (8, 8), (8, 8), (16, 8), (16, 8), (40, 8)]
    );
}

#[test]
fn settle_copy_readers_read_the_copied_source() {
    let mut arena = SLTNodeArena::new();
    let mut copy = simple_path(&mut arena, 12, Some(11));
    copy.transparent_copy = true;
    let paths = vec![
        simple_path(&mut arena, 13, Some(12)),
        copy,
        simple_path(&mut arena, 11, Some(10)),
    ];
    let result = partitioned_sort(paths, &arena);
    let plan = result
        .partitions
        .iter()
        .map(|partition| (partition.reads.clone(), partition.writes.clone()))
        .collect::<Vec<_>>();
    // The copy is guarded like other work, and its reader reads the
    // copied source too.
    assert_eq!(
        plan,
        vec![
            (vec![10], vec![11]),
            (vec![11], vec![12]),
            (vec![11, 12], vec![13]),
        ]
    );
}

/// An external reader watching the target of a whole-variable copy must
/// still receive its wake flag when the copy partition runs, even though
/// internal partition-to-partition change checks on the copy target are
/// skipped.
#[test]
fn settle_copy_partitions_mark_external_readers() {
    let mut arena = SLTNodeArena::new();
    let mut copy = simple_path(&mut arena, 12, Some(11));
    copy.transparent_copy = true;
    let paths = vec![
        simple_path(&mut arena, 13, Some(12)),
        copy,
        simple_path(&mut arena, 11, Some(10)),
    ];
    let widths = (10..14).map(|id| (id, 8)).collect();
    let result = super::sort_with_unpacked_element_widths(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        &crate::HashMap::default(),
        1,
        Some(super::SettlePartitioning {
            flags: 99,
            target_cost: 1,
            external_readers: [(12, vec![1])].into_iter().collect(),
            clock_nets: crate::HashSet::default(),
            profile: None,
        }),
    )
    .unwrap();
    assert_eq!(result.partitions.len(), 3);
    let mut stored = Vec::new();
    for unit in &result.execution_units {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                if let SIRInstruction::Store(99, SIROffset::Static(bit), width, ..) = instruction {
                    stored.push((*bit, *width));
                }
            }
        }
    }
    stored.sort_unstable();
    // External flag 1 at byte 3 + 1 + 1 (bit 40) is marked when 12 is written.
    assert!(
        stored.contains(&(40, 8)),
        "external reader flag must be marked by the copy partition: {stored:?}"
    );
}

#[test]
fn memory_ssa_scheduler_lowers_independent_single_use_chains_contiguously() {
    let mut arena = SLTNodeArena::new();
    let paths = vec![
        simple_path(&mut arena, 10, None),
        simple_path(&mut arena, 20, None),
        simple_path(&mut arena, 11, Some(10)),
        simple_path(&mut arena, 21, Some(20)),
        simple_path(&mut arena, 12, Some(11)),
        simple_path(&mut arena, 22, Some(21)),
    ];
    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &crate::HashMap::default(),
        1,
    )
    .unwrap();
    let unit = &result.execution_units[0];
    let stores = unit.blocks[&unit.entry_block_id]
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, ..) => Some(*address),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(stores, vec![10, 11, 12, 20, 21, 22]);
}

#[test]
fn previous_value_use_is_an_order_edge_not_a_forwarded_value() {
    let mut arena = SLTNodeArena::new();
    let mut previous_user = simple_path(&mut arena, 20, Some(10));
    previous_user.sources.clear();
    previous_user.previous_sources = [VarAtomBase::new(10, 0, 7)].into_iter().collect();
    let writer = simple_path(&mut arena, 10, None);
    let paths = vec![previous_user, writer];

    let memory_ssa = build_logic_path_memory_ssa(&paths).unwrap();
    assert_eq!(memory_ssa.dependencies.users, vec![vec![1], vec![]]);
    assert_eq!(memory_ssa.dependencies.predecessors, vec![vec![], vec![0]]);
    assert_eq!(
        memory_ssa.values.users,
        vec![Vec::<usize>::new(), Vec::new()]
    );
    assert_eq!(
        memory_ssa.values.predecessors,
        vec![Vec::<usize>::new(), Vec::new()]
    );

    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &crate::HashMap::default(),
        1,
    )
    .unwrap();
    let unit = &result.execution_units[0];
    let stores = unit.blocks[&unit.entry_block_id]
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, ..) => Some(*address),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores, vec![20, 10]);
}

#[test]
fn memory_use_depends_on_each_overlapping_bit_range_definition() {
    let mut arena = SLTNodeArena::new();
    let mut low = simple_path(&mut arena, 10, None);
    low.target = LogicPathTarget::Var(VarAtomBase::new(10, 0, 3));
    let mut high = simple_path(&mut arena, 10, None);
    high.target = LogicPathTarget::Var(VarAtomBase::new(10, 4, 7));
    let mut user = simple_path(&mut arena, 20, Some(10));
    user.sources = [VarAtomBase::new(10, 2, 5)].into_iter().collect();

    let memory_ssa = build_logic_path_memory_ssa(&[low, high, user]).unwrap();
    assert_eq!(
        memory_ssa.dependencies.users,
        vec![vec![2], vec![2], vec![]]
    );
    assert_eq!(
        memory_ssa.dependencies.predecessors,
        vec![vec![], vec![], vec![0, 1]]
    );
    assert_eq!(memory_ssa.values.users, vec![vec![2], vec![2], vec![]]);
    assert_eq!(
        memory_ssa.values.predecessors,
        vec![vec![], vec![], vec![0, 1]]
    );
}

fn fixed_group_path(
    arena: &mut SLTNodeArena<u32>,
    guard: crate::NodeId,
    loop_var: u32,
    target: u32,
    external: u32,
    trip_count: usize,
) -> LogicPath<u32> {
    fixed_group_path_with_index(
        arena,
        guard,
        loop_var,
        target,
        external,
        trip_count,
        1,
        BitAccess::new(0, 7),
    )
}

#[allow(clippy::too_many_arguments)]
fn fixed_group_path_with_index(
    arena: &mut SLTNodeArena<u32>,
    guard: crate::NodeId,
    loop_var: u32,
    target: u32,
    external: u32,
    trip_count: usize,
    index_scale: u8,
    load_access: BitAccess,
) -> LogicPath<u32> {
    let target = VarAtomBase::new(target, 0, 7);
    let initial = arena
        .alloc(SLTNode::Input {
            variable: target.id,
            signed: false,
            index: Vec::new(),
            access: target.access,
        })
        .unwrap();
    let loop_index = arena
        .alloc(SLTNode::Input {
            variable: loop_var,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let loop_index = if index_scale == 1 {
        loop_index
    } else {
        let scale = arena
            .alloc(SLTNode::Constant(
                BigUint::from(index_scale),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        arena
            .alloc(SLTNode::Binary(loop_index, BinaryOp::Mul, scale))
            .unwrap()
    };
    let update = arena
        .alloc(SLTNode::Input {
            variable: external,
            signed: false,
            index: vec![
                serde_json::from_value(serde_json::json!({
                    "node": loop_index,
                    "stride": 8,
                    "kind": "Packed",
                }))
                .unwrap(),
            ],
            access: load_access,
        })
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target,
                initial,
                update,
            }],
        })
        .unwrap();
    LogicPath {
        transparent_copy: false,
        target: LogicPathTarget::Var(target),
        sources: [VarAtomBase::new(external, 0, 63)].into_iter().collect(),
        previous_sources: crate::HashSet::default(),
        address_sources: crate::HashSet::default(),
        local_inputs: Vec::new(),
        order_before: crate::HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr: group,
    }
}

fn fixed_group_fixture(
    left_trip_count: usize,
    right_trip_count: usize,
) -> (
    SLTNodeArena<u32>,
    Vec<LogicPath<u32>>,
    crate::HashMap<u32, usize>,
) {
    let mut arena = SLTNodeArena::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let paths = vec![
        fixed_group_path(&mut arena, guard, 100, 10, 50, left_trip_count),
        fixed_group_path(&mut arena, guard, 101, 11, 50, right_trip_count),
    ];
    let widths = [(10, 8), (11, 8)].into_iter().collect();
    (arena, paths, widths)
}

fn schedule_branch_count(result: &super::ScheduleResult<u32>) -> usize {
    result
        .execution_units
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
        .count()
}

fn synthetic_exact_load(base: u32) -> ExactIndexedLoadKey<u32> {
    ExactIndexedLoadKey {
        base,
        access: BitAccess::new(0, 7),
        index: vec![(
            NormalizedIndexExpr::LoopValue {
                signed: false,
                access: BitAccess::new(0, 7),
            },
            8,
        )],
    }
}

fn synthetic_fold_group(
    root: usize,
    target: u32,
    loop_var: u32,
    indexed_loads: impl IntoIterator<Item = ExactIndexedLoadKey<u32>>,
    carried_chunks: u128,
) -> ExactFoldGroup<u32> {
    ExactFoldGroup {
        root: crate::NodeId(root),
        facts: FoldGroupReadFacts {
            loop_var,
            state_targets: vec![VarAtomBase::new(target, 0, 7)],
            guard_reads: Vec::new(),
            initial_reads: Vec::new(),
            update_reads: Vec::new(),
            indexed_loads: indexed_loads.into_iter().collect(),
            carried_chunks,
        },
    }
}

#[test]
fn independent_exact_fold_groups_lower_jointly_and_keep_store_order() {
    let (arena, paths, widths) = fixed_group_fixture(4, 4);
    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        1,
    )
    .unwrap();

    assert_eq!(schedule_branch_count(&result), 2);
    let store_block = result
        .execution_units
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .find(|block| {
            block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                .count()
                == 2
        })
        .expect("joint results must be materialized before the ordered stores");
    let stores = store_block
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, _, _, _, _, _) => Some(*address),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores, vec![10, 11]);
}

#[test]
fn different_index_expression_or_load_slice_prevents_joint_lowering() {
    let mut arena = SLTNodeArena::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let paths = vec![
        fixed_group_path_with_index(&mut arena, guard, 100, 10, 50, 4, 1, BitAccess::new(0, 7)),
        fixed_group_path_with_index(&mut arena, guard, 101, 11, 50, 4, 2, BitAccess::new(0, 7)),
        fixed_group_path_with_index(&mut arena, guard, 102, 12, 50, 4, 1, BitAccess::new(8, 15)),
    ];
    let widths = [(10, 8), (11, 8), (12, 8)].into_iter().collect();
    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        1,
    )
    .unwrap();

    assert_eq!(schedule_branch_count(&result), 6);
}

#[test]
fn weighted_family_selection_ignores_conflicting_first_root() {
    let shared = synthetic_exact_load(50);
    let candidates = vec![
        synthetic_fold_group(0, 10, 100, [shared.clone()], 1),
        synthetic_fold_group(1, 11, 101, [shared.clone()], 1),
        synthetic_fold_group(2, 12, 102, [shared], 1),
    ];
    let available = vec![true; 3];
    let compatible = vec![
        vec![false, false, false],
        vec![false, false, true],
        vec![false, true, false],
    ];
    let shared_load = vec![
        vec![false, true, true],
        vec![true, false, true],
        vec![true, true, false],
    ];

    let family = best_weighted_fold_family(
        &candidates,
        &available,
        &compatible,
        &shared_load,
        &crate::HashSet::default(),
        false,
    )
    .expect("the compatible B+C family has positive net benefit");
    let mut roots = family
        .members
        .iter()
        .map(|member| candidates[*member].root.0)
        .collect::<Vec<_>>();
    roots.sort_unstable();
    assert_eq!(roots, vec![1, 2]);
}

#[test]
fn weighted_family_selection_rejects_benefit_not_exceeding_pressure() {
    let shared = synthetic_exact_load(50);
    let candidates = vec![
        synthetic_fold_group(0, 10, 100, [shared.clone()], 8),
        synthetic_fold_group(1, 11, 101, [shared], 8),
    ];
    let available = vec![true; 2];
    let compatible = vec![vec![false, true], vec![true, false]];
    let shared_load = compatible.clone();

    assert!(
        best_weighted_fold_family(
            &candidates,
            &available,
            &compatible,
            &shared_load,
            &crate::HashSet::default(),
            false,
        )
        .is_none()
    );
}

#[test]
fn dependency_edge_separates_otherwise_joint_fold_groups() {
    let (arena, mut paths, widths) = fixed_group_fixture(4, 4);
    paths[1].sources.insert(VarAtomBase::new(10, 0, 7));
    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        1,
    )
    .unwrap();

    assert_eq!(schedule_branch_count(&result), 4);
}

#[test]
fn different_fold_group_domains_do_not_lower_jointly() {
    let (arena, paths, widths) = fixed_group_fixture(4, 5);
    let result = sort(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        1,
    )
    .unwrap();

    assert_eq!(schedule_branch_count(&result), 4);
}

#[test]
fn joint_fold_preparation_does_not_mutate_logic_path_metadata() {
    let (arena, mut paths, _) = fixed_group_fixture(4, 4);
    paths[0].previous_sources = [VarAtomBase::new(60, 0, 7)].into_iter().collect();
    paths[0].sources.insert(VarAtomBase::new(61, 0, 7));
    paths[0].address_sources = [VarAtomBase::new(61, 0, 7)].into_iter().collect();
    paths[0].comb_capture_enable_sites = vec![3, 7];
    let snapshot = paths.clone();
    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let mut dependencies = crate::HashMap::default();
    let mut inverse_dependencies = crate::HashMap::default();
    let schedule_index = build_fold_group_schedule_index(&paths, &arena);

    let prepared = prepare_atomic_fold_group_results(
        &[0, 1],
        &paths,
        &schedule_index,
        &mut SLTToSIRLowerer::new(false),
        &mut builder,
        &arena,
        &mut cache,
        &mut dependencies,
        &mut inverse_dependencies,
        false,
    );

    assert_eq!(prepared.len(), 2);
    assert_eq!(paths, snapshot);
}

#[test]
fn for_fold_group_dependencies_keep_initial_but_hide_loop_scoped_updates() {
    let mut arena = SLTNodeArena::<u32>::new();
    let input = |arena: &mut SLTNodeArena<u32>, variable| {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap()
    };
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let initial = input(&mut arena, 2);
    let state_input = input(&mut arena, 2);
    let loop_input = input(&mut arena, 1);
    let external_input = input(&mut arena, 3);
    let scoped_sum = arena
        .alloc(SLTNode::Binary(state_input, BinaryOp::Add, loop_input))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(scoped_sum, BinaryOp::Add, external_input))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 1,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(2, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();
    let mut memo = crate::HashMap::default();
    let mut inverse_memo = crate::HashMap::default();

    let dependencies = collect_node_input_deps(group, &arena, &mut memo, &mut inverse_memo);

    assert!(
        dependencies.contains(&2),
        "initial state is an external dependency"
    );
    assert!(
        dependencies.contains(&3),
        "ordinary update input remains external"
    );
    assert!(
        !dependencies.contains(&1),
        "loop variable is supplied by the fold"
    );
}

#[test]
fn partial_for_fold_group_state_keeps_uncovered_variable_dependency() {
    let mut arena = SLTNodeArena::<u32>::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let initial = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            8,
            false,
        ))
        .unwrap();
    let carried = arena
        .alloc(SLTNode::Input {
            variable: 2,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let uncovered = arena
        .alloc(SLTNode::Input {
            variable: 2,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(8, 15),
        })
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(carried, BinaryOp::Add, uncovered))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 1,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(2, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();
    let mut memo = crate::HashMap::default();
    let mut inverse_memo = crate::HashMap::default();

    let dependencies = collect_node_input_deps(group, &arena, &mut memo, &mut inverse_memo);
    assert!(
        dependencies.contains(&2),
        "the uncovered high byte remains an external dependency"
    );
}

#[test]
fn shared_for_fold_group_projections_materialize_once_and_store_sequentially() {
    let mut arena = SLTNodeArena::<u32>::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let input = |arena: &mut SLTNodeArena<u32>, variable| {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap()
    };
    let initial_a = input(&mut arena, 1);
    let initial_b = input(&mut arena, 2);
    let previous_a = input(&mut arena, 1);
    let previous_b = input(&mut arena, 2);
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 3,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![
                SLTForFoldGroupState {
                    target: VarAtomBase::new(1, 0, 7),
                    initial: initial_a,
                    update: previous_b,
                },
                SLTForFoldGroupState {
                    target: VarAtomBase::new(2, 0, 7),
                    initial: initial_b,
                    update: previous_a,
                },
            ],
        })
        .unwrap();
    let high = arena
        .alloc(SLTNode::Slice {
            expr: group,
            access: BitAccess::new(8, 15),
        })
        .unwrap();
    let low = arena
        .alloc(SLTNode::Slice {
            expr: group,
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let path = |target, expr| LogicPath {
        transparent_copy: false,
        target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
        // Keep the scheduling graph acyclic in this focused cache test;
        // dependency memoization still sees both initial reads.
        sources: crate::HashSet::default(),
        previous_sources: crate::HashSet::default(),
        address_sources: crate::HashSet::default(),
        local_inputs: Vec::new(),
        order_before: crate::HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr,
    };
    let mut widths = crate::HashMap::default();
    widths.insert(1, 8);
    widths.insert(2, 8);

    let result = sort(
        vec![path(1, high), path(2, low)],
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        1,
    )
    .unwrap();
    assert_eq!(result.execution_units.len(), 1);
    let eu = &result.execution_units[0];
    assert_eq!(
        eu.blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count(),
        2,
        "one grouped fold has only its entry and counted-loop branches"
    );
    assert_eq!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
            .count(),
        2,
        "both initial values are loaded once, not once per projection"
    );

    let store_block = eu
        .blocks
        .values()
        .find(|block| {
            block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                .count()
                == 2
        })
        .expect("both atomic projection stores share the materialization exit");
    let store_positions = store_block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(position, instruction)| {
            matches!(instruction, SIRInstruction::Store(..)).then_some(position)
        })
        .collect::<Vec<_>>();
    let stored_values = store_block
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(_, _, _, value, _, _) => Some(*value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(store_positions.len(), stored_values.len());
    for (projection, value) in stored_values.into_iter().enumerate() {
        let definition = store_block
            .instructions
            .iter()
            .position(|instruction| match instruction {
                SIRInstruction::Binary(dst, ..)
                | SIRInstruction::Unary(dst, ..)
                | SIRInstruction::Slice(dst, ..)
                | SIRInstruction::Concat(dst, ..)
                | SIRInstruction::Mux(dst, ..) => *dst == value,
                _ => false,
            })
            .expect("stored projection has a local definition");
        assert!(definition < store_positions[projection]);
        if projection != 0 {
            assert!(
                definition > store_positions[projection - 1],
                "a later projection must not remain live across an earlier Store"
            );
        }
    }
}

/// A process writes all its outputs in one run of its unit, so the unit
/// is emitted at its first projection only, while a downstream path of
/// each output still follows the process.
#[test]
fn acyclic_process_unit_runs_at_its_first_projection_only() {
    use celox_sir::{SIROffset, SIRValue};
    const INPUT: usize = 0;
    const PROCESS: usize = 10;
    let outputs = [1, 2, 3];
    let mut builder = SIRBuilder::<usize>::new();
    let value = builder.alloc_bit(8, false);
    builder.emit(SIRInstruction::Load(value, INPUT, SIROffset::Static(0), 8));
    for output in outputs {
        builder.emit(SIRInstruction::Store(
            output,
            SIROffset::Static(0),
            8,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    let marker = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Imm(marker, SIRValue::new(1u8)));
    builder.emit(SIRInstruction::Store(
        PROCESS,
        SIROffset::Static(0),
        1,
        marker,
        Vec::new(),
        Vec::new(),
    ));
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let unit = celox_sir::ExecutionUnit {
        entry_block_id: celox_sir::BlockId(0),
        blocks,
        register_map,
    };

    let mut arena = SLTNodeArena::<usize>::new();
    let input = |arena: &mut SLTNodeArena<usize>, variable| {
        arena
            .alloc(SLTNode::Input {
                variable,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
                signed: false,
            })
            .unwrap()
    };
    let path = |target, expr, sources: &[usize]| LogicPath {
        transparent_copy: false,
        target,
        sources: sources
            .iter()
            .map(|&source| VarAtomBase::new(source, 0, 7))
            .collect(),
        previous_sources: crate::HashSet::default(),
        address_sources: crate::HashSet::default(),
        local_inputs: Vec::new(),
        order_before: crate::HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr,
    };
    let mut paths = Vec::new();
    for output in outputs {
        let expr = input(&mut arena, output);
        paths.push(path(
            LogicPathTarget::Process {
                process: PROCESS,
                after_initials: false,
                sensitivity_counters: Vec::new(),
                startup: None,
                target: VarAtomBase::new(output, 0, 7),
                unit: Box::new(unit.clone()),
            },
            expr,
            &[INPUT],
        ));
        // A consumer of each output, scheduled after its projection.
        let read = input(&mut arena, output);
        paths.push(path(
            LogicPathTarget::Var(VarAtomBase::new(output + 100, 0, 7)),
            read,
            &[output],
        ));
    }
    let widths = [INPUT, PROCESS]
        .into_iter()
        .chain(outputs)
        .chain(outputs.map(|output| output + 100))
        .map(|variable| (variable, if variable == PROCESS { 1 } else { 8 }))
        .collect();
    for partitioning in [
        None,
        Some(super::SettlePartitioning {
            flags: 99,
            target_cost: 1,
            external_readers: crate::HashMap::default(),
            clock_nets: crate::HashSet::default(),
            profile: None,
        }),
    ] {
        let result = super::sort_with_unpacked_element_widths(
            paths.clone(),
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            &crate::HashMap::default(),
            1,
            partitioning,
        )
        .unwrap();
        let stores = |variable| {
            result.execution_units.iter()
                    .flat_map(|unit| unit.blocks.values())
                    .flat_map(|block| &block.instructions)
                    .filter(|instruction| matches!(instruction, SIRInstruction::Store(target, ..) if *target == variable))
                    .count()
        };
        assert_eq!(stores(PROCESS), 1, "the unit runs once");
        for output in outputs {
            assert_eq!(stores(output), 1);
            assert_eq!(stores(output + 100), 1, "each consumer is emitted");
        }
        if !result.partitions.is_empty() {
            assert_eq!(result.partitions.len(), 4, "one body and three consumers");
            assert_eq!(
                result
                    .partitions
                    .iter()
                    .filter(|part| part.reads.contains(&INPUT))
                    .count(),
                1
            );
            for output in outputs.into_iter().chain([PROCESS]) {
                assert_eq!(
                    result
                        .partitions
                        .iter()
                        .filter(|part| part.writes.contains(&output))
                        .count(),
                    1,
                    "only the executing partition watches output {output}"
                );
            }
        }
    }
}
