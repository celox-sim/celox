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

    let condition_value = lowerer.lower(builder, condition, arena, lower_cache);
    let then_block = builder.new_block();
    let else_block = builder.new_block();
    let merge_block = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: condition_value,
        true_block: (then_block, Vec::new()),
        false_block: (else_block, Vec::new()),
    });

    let mut affected = paths
        .iter()
        .filter_map(|path| input[*path].target.var())
        .filter_map(|target| inverse_dep_memo.get(&target.id))
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    affected.sort_unstable();
    affected.dedup();
    let saved_cache = affected
        .iter()
        .filter_map(|node| lower_cache.get(node).copied().map(|value| (*node, value)))
        .collect::<Vec<_>>();

    let mut emit_arm = |builder: &mut SIRBuilder<Addr>,
                        lower_cache: &mut HashMap<NodeId, RegisterId>,
                        take_true: bool| {
        let mut inserted = Vec::new();
        for &path in paths {
            let (_, then_expr, else_expr) = scheduled_root_mux(&input[path], arena)
                .expect("scheduled guard region must retain root Muxes");
            let root = if take_true { then_expr } else { else_expr };
            // Keep the existing per-LogicPath lowering model for nested
            // decisions. The scheduled region owns only the common outer
            // guard; changing fanout/cost facts for all nested Muxes at once
            // would silently replace the established branch decisions.
            let value = lowerer.lower(builder, root, arena, lower_cache);
            inserted.extend(lowerer.take_scheduled_region_insertions());
            emit_logic_path_store_with_result(
                lowerer,
                builder,
                &input[path],
                arena,
                lower_cache,
                unpacked_element_widths,
                Some(value),
            );
            invalidate_logic_path_target(&input[path], inverse_dep_memo, lower_cache);
        }
        builder.seal_block(SIRTerminator::Jump(merge_block, Vec::new()));
        for node in inserted {
            lower_cache.remove(&node);
        }
        for node in &affected {
            lower_cache.remove(node);
        }
        lower_cache.extend(saved_cache.iter().copied());
    };

    builder.switch_to_block(then_block);
    emit_arm(builder, lower_cache, true);
    builder.switch_to_block(else_block);
    emit_arm(builder, lower_cache, false);
    builder.switch_to_block(merge_block);

    // Both arms publish every path target, so cached expressions reading any
    // such target are invalid at the merge even when both arm caches happened
    // to contain the same SLT node identity.
    for node in affected {
        lower_cache.remove(&node);
    }
}
