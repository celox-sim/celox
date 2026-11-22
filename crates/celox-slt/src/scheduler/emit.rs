//! Lowering scheduled logic paths to stores.

use super::*;

pub(super) fn lower_logic_path_expr<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) -> RegisterId {
    if path.local_inputs.is_empty() {
        return lowerer.lower(builder, path.expr, arena, lower_cache);
    }

    let mut env_inputs = HashMap::default();
    for (addr, node) in &path.local_inputs {
        let reg = lowerer.lower(builder, *node, arena, lower_cache);
        let width = crate::get_width(*node, arena);
        if width > 0 {
            env_inputs.insert(VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, path.expr, arena, lower_cache, env_inputs)
}

pub(super) fn lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) -> RegisterId {
    if path.local_inputs.is_empty() {
        return lowerer.lower(builder, node, arena, lower_cache);
    }
    // Unbound captures may already have been materialized before a later
    // write. Do not rebuild them under the observer's unrelated local inputs.
    if matches!(arena.get(node), crate::SLTNode::Capture { .. })
        && let Some(reg) = lower_cache.get(&node)
    {
        return *reg;
    }
    let mut env_inputs = HashMap::default();
    for (addr, local_node) in &path.local_inputs {
        let reg = lowerer.lower(builder, *local_node, arena, lower_cache);
        let width = crate::get_width(*local_node, arena);
        if width > 0 {
            env_inputs.insert(VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, node, arena, lower_cache, env_inputs)
}

pub(super) fn pre_lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) {
    if path.local_inputs.is_empty() {
        lowerer.lower(builder, node, arena, lower_cache);
    }
}

pub(super) fn static_access_offset<Addr: Eq + Hash>(
    addr: &Addr,
    access: BitAccess,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> SIROffset {
    let width = access.msb - access.lsb + 1;
    match unpacked_element_widths.get(addr).copied() {
        Some(element_width)
            if element_width != 0
                && width > element_width
                && access.lsb.is_multiple_of(element_width)
                && width.is_multiple_of(element_width) =>
        {
            SIROffset::PackedElements {
                bit_offset: access.lsb,
                element_width,
            }
        }
        _ => SIROffset::Static(access.lsb),
    }
}

pub(super) fn emit_logic_path_store_with_result<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    prepared_result: Option<RegisterId>,
) {
    match &path.target {
        LogicPathTarget::Var(target) => {
            for node in &path.pre_lower_nodes {
                pre_lower_logic_path_node(lowerer, builder, path, *node, arena, lower_cache);
            }
            let result_reg = match prepared_result {
                Some(result) => result,
                None => lower_logic_path_expr(lowerer, builder, path, arena, lower_cache),
            };
            let width = 1 + target.access.msb - target.access.lsb;
            let offset = static_access_offset(&target.id, target.access, unpacked_element_widths);
            let old_reg =
                if path.comb_capture_enable_sites.is_empty() || path.comb_capture_enable_always {
                    None
                } else {
                    let old_reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Load(
                        old_reg,
                        target.id,
                        offset.clone(),
                        width,
                    ));
                    Some(old_reg)
                };
            builder.emit(SIRInstruction::Store(
                target.id,
                offset,
                width,
                result_reg,
                Vec::new(),
                Vec::new(),
            ));
            if !path.comb_capture_enable_sites.is_empty() {
                let (old, new) = if path.comb_capture_enable_always {
                    let old = builder.alloc_bit(1, false);
                    let new = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Imm(old, SIRValue::new(0u8)));
                    builder.emit(SIRInstruction::Imm(new, SIRValue::new(1u8)));
                    (old, new)
                } else {
                    (
                        old_reg.expect("changed capture enable loads the old value"),
                        result_reg,
                    )
                };
                builder.emit(SIRInstruction::CombCaptureEnableIfChanged {
                    old,
                    new,
                    sites: path.comb_capture_enable_sites.clone(),
                });
            }
        }
        LogicPathTarget::Process { unit, .. } => {
            // The unit can write every output of its process, not only this
            // path's scheduling target. Do not reuse values across the call.
            lower_cache.clear();
            builder.inline_execution_unit(unit);
            lower_cache.clear();
        }
        LogicPathTarget::CombCaptureEvent {
            site_id,
            guard,
            emit_on_true,
            args,
            loop_runner,
            fatal_error_code,
            consume_enabled,
        } => {
            debug_assert!(prepared_result.is_none());
            if let Some(loop_runner) = loop_runner {
                lower_logic_path_node(lowerer, builder, path, *loop_runner, arena, lower_cache);
                return;
            }
            let emit = |builder: &mut SIRBuilder<Addr>,
                        lowerer: &mut crate::SLTToSIRLowerer,
                        lower_cache: &mut HashMap<NodeId, RegisterId>| {
                let regs = args
                    .iter()
                    .map(|arg| {
                        lower_logic_path_node(lowerer, builder, path, *arg, arena, lower_cache)
                    })
                    .collect();
                builder.emit(SIRInstruction::CombCaptureEvent {
                    site_id: *site_id,
                    args: regs,
                    fatal_error_code: *fatal_error_code,
                    consume_enabled: *consume_enabled,
                });
            };
            if let Some(guard) = guard {
                let cond =
                    lower_logic_path_node(lowerer, builder, path, *guard, arena, lower_cache);
                let branch_cond = if *emit_on_true {
                    cond
                } else {
                    let inverted = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Unary(inverted, UnaryOp::LogicNot, cond));
                    inverted
                };
                let event_block = builder.new_block();
                let done_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: branch_cond,
                    true_block: (event_block, vec![]),
                    false_block: (done_block, vec![]),
                });
                builder.switch_to_block(event_block);
                // Values first lowered inside the guarded block do not
                // dominate the join; later paths must not reuse them.
                let dominating_cache = lower_cache.clone();
                emit(builder, lowerer, lower_cache);
                *lower_cache = dominating_cache;
                builder.seal_block(SIRTerminator::Jump(done_block, vec![]));
                builder.switch_to_block(done_block);
            } else {
                emit(builder, lowerer, lower_cache);
            }
        }
    }
}

pub(super) fn emit_logic_path_store<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) {
    emit_logic_path_store_with_result(
        lowerer,
        builder,
        path,
        arena,
        lower_cache,
        unpacked_element_widths,
        None,
    );
}

pub(super) fn invalidate_logic_path_target<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    path: &LogicPath<Addr>,
    inverse_dep_memo: &HashMap<Addr, HashSet<NodeId>>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) {
    let Some(target) = path.target.var() else {
        return;
    };
    if let Some(to_remove) = inverse_dep_memo.get(&target.id) {
        for node in to_remove {
            if !path.pre_lower_nodes.contains(node) {
                lower_cache.remove(node);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_scheduled_guard_region<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    condition: NodeId,
    paths: &[usize],
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) {
    for &path in paths {
        collect_logic_path_input_deps(&input[path], arena, dep_memo, inverse_dep_memo);
    }

    use crate::condition_groups::{self, Merge, Write};
    let writes = paths
        .iter()
        .map(|&index| Write {
            index,
            value: input[index].expr,
        })
        .collect();
    // An earlier path can publish a new selector value in this region.
    // Merge only predicates which all of its writes leave invariant.
    let targets = paths
        .iter()
        .filter_map(|&i| input[i].target.var().map(|t| t.id))
        .collect();
    let mut stable = HashMap::default();
    let result = condition_groups::emit(
        writes,
        condition,
        arena,
        lowerer,
        builder,
        lower_cache,
        |_, _| {}, // Keep visibility of earlier combinational assignments.
        |cond, group| {
            if !*stable
                .entry(cond)
                .or_insert_with(|| !condition_reads_target(cond, &targets, arena))
                || !condition_groups::profitable(arena, cond, group.iter().map(|w| w.value))
            {
                return Merge::Separate;
            }
            let mut written = HashSet::default();
            for w in group {
                if !dep_memo[&w.value].is_disjoint(&written) {
                    return Merge::Branch;
                }
                written.insert(input[w.index].target.var().expect("guarded variable").id);
            }
            Merge::BranchWithIndependentWrites
        },
        |write, prepared, lowerer, builder, cache, local| {
            let path = &input[write.index];
            let value = prepared.unwrap_or_else(|| {
                let value = lowerer.lower(builder, write.value, arena, cache);
                local.extend(lowerer.take_scheduled_region_insertions());
                value
            });
            let value = lowerer.cast_reg_width(builder, value, crate::get_width(path.expr, arena));
            emit_logic_path_store_with_result(
                lowerer,
                builder,
                path,
                arena,
                cache,
                unpacked_element_widths,
                Some(value),
            );
            invalidate_logic_path_target(path, inverse_dep_memo, cache);
            Ok::<_, std::convert::Infallible>(())
        },
    );
    let Ok(()) = result;
}
