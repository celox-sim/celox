use std::fmt::Debug;
use std::{collections::BTreeSet, hash::Hash};

use veryl_analyzer::ir::VarId;

use crate::{
    AbsoluteAddr, GlueAddr, GlueBlock, HashMap, InstancePath, RelocationModule, SimModule,
};
use celox_design::{BinaryOp, BitAccess, InstanceId, UnaryOp, VarAtomBase};
use celox_slt::{
    CombObserver, LogicPath, LogicPathTarget, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError,
    get_width,
};

pub struct FlattenedModule {
    pub relocation: RelocationModule,
    pub pre_atomized_comb_blocks: Vec<LogicPath<AbsoluteAddr>>,
}

pub fn flatten_module(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<FlattenedModule, SLTNodeFactsError> {
    let instance_id = instance_ids[path];
    let cv = &|id: &VarId| AbsoluteAddr {
        instance_id,
        var_id: *id,
    };

    let mut comb_cache = HashMap::default();
    let mut comb_blocks: Vec<_> = module
        .comb_blocks
        .iter()
        .map(|e| convert_logic_path(e, &module.arena, arena, &mut comb_cache, &cv))
        .collect::<Result<_, _>>()?;
    let mut observer_cache = HashMap::default();
    let comb_observers: Vec<_> = module
        .comb_observers
        .iter()
        .map(|observer| {
            convert_comb_observer(observer, &module.arena, arena, &mut observer_cache, &cv)
        })
        .collect::<Result<_, _>>()?;
    for (child_instance_name, gbs) in &module.glue_blocks {
        for (idx, gb) in gbs.iter().enumerate() {
            let mut glue_cache = HashMap::default();
            let mut child_path = path.0.clone();
            child_path.push((*child_instance_name, idx));
            let child_id = instance_ids[&InstancePath(child_path)];
            comb_blocks.extend(convert_glue_block(
                gb,
                instance_id,
                child_id,
                &gb.arena,
                arena,
                &mut glue_cache,
            )?);
        }
    }
    // Atomize logic paths
    let atomized_comb_blocks = atomize_logic_paths(
        &comb_blocks,
        global_boundaries,
        unpacked_element_widths,
        arena,
    )?;

    Ok(FlattenedModule {
        pre_atomized_comb_blocks: comb_blocks,
        relocation: RelocationModule {
            eval_apply_ff_blocks: HashMap::default(),
            eval_only_ff_blocks: HashMap::default(),
            apply_ff_blocks: HashMap::default(),
            comb_blocks: atomized_comb_blocks,
            comb_observers,
        },
    })
}

/// Atomizes the given logic paths based on the provided boundary map.
fn atomize_logic_paths(
    paths: &Vec<LogicPath<AbsoluteAddr>>,
    boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<Vec<LogicPath<AbsoluteAddr>>, SLTNodeFactsError> {
    let mut atomized_paths = Vec::new();

    for path in paths {
        let Some(target_var) = path.target.var() else {
            atomized_paths.push(path.clone());
            continue;
        };
        let element_width = unpacked_element_widths.get(&target_var.id).copied();
        let mut effective_boundaries = boundaries.get(&target_var.id).cloned().unwrap_or_default();
        if let Some(element_width) = element_width {
            let mut boundary =
                (target_var.access.lsb / element_width + 1).saturating_mul(element_width);
            while boundary <= target_var.access.msb {
                effective_boundaries.insert(boundary);
                let Some(next) = boundary.checked_add(element_width) else {
                    break;
                };
                boundary = next;
            }
        }
        if !effective_boundaries.is_empty() {
            // This variable has defined boundaries, so we need to split it.
            let atoms = target_var.access.calculate_atoms(&effective_boundaries);

            // Extract the set of variable IDs that were originally declared as sources.
            // This acts as a "source mask" to filter out unintended dependencies.
            let original_source_ids: crate::HashSet<_> =
                path.sources.iter().map(|s| s.id).collect();
            let original_previous_sources = path.previous_sources.clone();

            // Compute per-atom source sets (with bit ranges), then coalesce
            // consecutive atoms whose source sets are identical into wider paths.
            let mut atom_infos: Vec<(
                BitAccess,
                crate::HashSet<VarAtomBase<AbsoluteAddr>>,
                crate::HashSet<AbsoluteAddr>,
            )> = Vec::new();
            for atom_access in &atoms {
                let relative_atom_access = BitAccess::new(
                    atom_access.lsb - target_var.access.lsb,
                    atom_access.msb - target_var.access.lsb,
                );
                let new_expr = project_logic_path_expr(path.expr, relative_atom_access, arena)?;
                let mut expr_inputs = crate::HashSet::default();
                collect_inputs(new_expr, arena, &mut expr_inputs);
                let filtered_sources: crate::HashSet<_> = expr_inputs
                    .into_iter()
                    .filter(|input_atom| original_source_ids.contains(&input_atom.id))
                    .collect();
                let filtered_source_ids = filtered_sources.iter().map(|source| source.id).collect();
                atom_infos.push((*atom_access, filtered_sources, filtered_source_ids));
            }

            // Coalesce adjacent atoms which depend on the same state objects.
            // The merged expression is projected again below, so its precise
            // source ranges are recovered rather than inherited from either
            // atom. Only padded unpacked-element boundaries are physical and
            // therefore prohibit this coalescing.
            let mut i = 0;
            while i < atom_infos.len() {
                let group_start = i;
                while i + 1 < atom_infos.len() {
                    let current = &atom_infos[i];
                    let next = &atom_infos[i + 1];
                    let exact_sources_match = next.1 == current.1;
                    let source_objects_match = next.2 == current.2;
                    let pointwise_single_bits =
                        current.0.lsb == current.0.msb && next.0.lsb == next.0.msb;
                    let contiguous_unpacked_elements =
                        element_width.is_some_and(|width| width.is_multiple_of(8));
                    // Byte-aligned unpacked elements are physically contiguous
                    // in every finalized layout, so retaining their semantic
                    // boundary here only forces later byte-at-a-time RMW. A
                    // non-byte-aligned element has padding and must remain a
                    // separate path unless the transfer is explicitly lowered
                    // as a whole-object copy.
                    let crosses_strided_element = element_width.is_some_and(|width| {
                        !width.is_multiple_of(8) && next.0.lsb.is_multiple_of(width)
                    });
                    let may_recover_coarse_range = source_objects_match
                        && (pointwise_single_bits || contiguous_unpacked_elements);
                    if !(exact_sources_match || may_recover_coarse_range) || crosses_strided_element
                    {
                        break;
                    }
                    i += 1;
                }
                let group_end = i;
                i += 1;

                let merged_lsb = atom_infos[group_start].0.lsb;
                let merged_msb = atom_infos[group_end].0.msb;

                // Build the merged path expression.
                let relative_access = BitAccess::new(
                    merged_lsb - target_var.access.lsb,
                    merged_msb - target_var.access.lsb,
                );
                let merged_width = merged_msb - merged_lsb + 1;
                let original_width = target_var.access.msb - target_var.access.lsb + 1;

                let merged_expr = if merged_width == original_width {
                    // Covers the full original range — use the expression directly.
                    path.expr
                } else {
                    project_logic_path_expr(path.expr, relative_access, arena)?
                };

                // Collect the actual bit-level sources for the merged range.
                let mut merged_sources = crate::HashSet::default();
                collect_inputs(merged_expr, arena, &mut merged_sources);
                let filtered_sources: crate::HashSet<_> = merged_sources
                    .iter()
                    .copied()
                    .filter(|input_atom| original_source_ids.contains(&input_atom.id))
                    .collect();
                let filtered_previous_sources: crate::HashSet<_> = merged_sources
                    .iter()
                    .copied()
                    .filter(|input_atom| {
                        original_previous_sources.iter().any(|previous| {
                            previous.id == input_atom.id
                                && previous.access.overlaps(&input_atom.access)
                        })
                    })
                    .collect();
                let filtered_address_sources: crate::HashSet<_> = merged_sources
                    .into_iter()
                    .filter(|input_atom| {
                        path.address_sources.iter().any(|address| {
                            address.id == input_atom.id
                                && address.access.overlaps(&input_atom.access)
                        })
                    })
                    .collect();

                let target = VarAtomBase::new(target_var.id, merged_lsb, merged_msb);
                atomized_paths.push(LogicPath {
                    target: LogicPathTarget::Var(target),
                    sources: filtered_sources,
                    previous_sources: filtered_previous_sources,
                    address_sources: filtered_address_sources,
                    local_inputs: path.local_inputs.clone(),
                    order_before: path.order_before.clone(),
                    comb_capture_enable_sites: path.comb_capture_enable_sites.clone(),
                    pre_lower_nodes: path.pre_lower_nodes.clone(),
                    expr: merged_expr,
                });
            }
        } else {
            // No boundaries defined for this target, so just add it as is.
            atomized_paths.push(path.clone());
        }
    }
    Ok(atomized_paths)
}

fn project_logic_path_expr(
    expression: NodeId,
    access: BitAccess,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<NodeId, SLTNodeFactsError> {
    match arena.get(expression).clone() {
        SLTNode::Input {
            variable,
            signed,
            index,
            access: input_access,
        } if access.msb <= input_access.msb - input_access.lsb => arena.alloc(SLTNode::Input {
            variable,
            signed,
            index,
            access: BitAccess::new(input_access.lsb + access.lsb, input_access.lsb + access.msb),
        }),
        SLTNode::Slice {
            expr: inner,
            access: inner_access,
        } if access.msb <= inner_access.msb - inner_access.lsb => project_logic_path_expr(
            inner,
            BitAccess::new(inner_access.lsb + access.lsb, inner_access.lsb + access.msb),
            arena,
        ),
        _ => arena.alloc(SLTNode::Slice {
            expr: expression,
            access,
        }),
    }
}

pub fn collect_inputs<A: Hash + Eq + Clone + Debug>(
    expr: NodeId,
    arena: &SLTNodeArena<A>,
    set: &mut crate::HashSet<VarAtomBase<A>>,
) {
    let mut visited = HashMap::default();
    collect_inputs_with_window(expr, None, arena, set, &mut visited);
}

fn collect_inputs_with_window<A: Hash + Eq + Clone + Debug>(
    expr: NodeId,
    window: Option<BitAccess>,
    arena: &SLTNodeArena<A>,
    set: &mut crate::HashSet<VarAtomBase<A>>,
    visited: &mut HashMap<NodeId, Vec<BitAccess>>,
) {
    let requested = window.unwrap_or_else(|| BitAccess::new(0, get_width(expr, arena) - 1));
    let uncovered = claim_uncovered_window(visited.entry(expr).or_default(), requested);

    // Shared DAG nodes are often reached through overlapping slices.  Process
    // only the newly requested portions so both traversal work and memoized
    // state are bounded by the union of bit ranges, not the number of paths.
    for window in uncovered.into_iter().map(Some) {
        match arena.get(expr) {
            SLTNode::Input {
                variable,
                access,
                index,
                ..
            } => {
                // Register the variable and its bit range as an input.
                if !index.is_empty() {
                    // --- Dynamic Indexing Case ---
                    // For scheduling safety, we MUST ignore the `window` here.
                    // Dynamic access can point to different bits within the range,
                    // so we need to cover the entire reachable bounding box.

                    let element_width = get_width(expr, arena);
                    let full_width = access.msb - access.lsb + 1;

                    let mut max_reachable_elements = 1usize;
                    for idx in index {
                        let idx_width = get_width(idx.node, arena);
                        let reachable = 1usize.checked_shl(idx_width as u32).unwrap_or(usize::MAX);
                        max_reachable_elements = max_reachable_elements.saturating_mul(reachable);
                    }

                    // Clamp by the actual number of elements in the variable.
                    let actual_elements = full_width / element_width;
                    let effective_elements = std::cmp::min(max_reachable_elements, actual_elements);

                    // Calculate the bounding box:
                    // LSB: Always the start of the first element (access.lsb).
                    // MSB: The end of the last reachable element.
                    let reachable_lsb = access.lsb;
                    let reachable_msb = access.lsb + (effective_elements * element_width) - 1;

                    set.insert(VarAtomBase::new(
                        variable.clone(),
                        reachable_lsb,
                        std::cmp::min(reachable_msb, access.msb),
                    ));
                } else {
                    // If the index is empty, it means the variable is statically indexed.
                    // In this case, we can apply the window to minimize the dependencies.
                    let full_width = access.msb - access.lsb + 1;
                    let win = window.unwrap_or(BitAccess::new(0, full_width - 1));

                    set.insert(VarAtomBase::new(
                        variable.clone(),
                        access.lsb + win.lsb,
                        access.lsb + win.msb,
                    ));
                }

                // Also collect inputs from the index expressions (dynamic indexing).
                for idx in index {
                    collect_inputs_with_window(idx.node, None, arena, set, visited);
                }
            }
            SLTNode::Slice { expr, access } => {
                let composed = if let Some(win) = window {
                    BitAccess::new(access.lsb + win.lsb, access.lsb + win.msb)
                } else {
                    *access
                };
                collect_inputs_with_window(*expr, Some(composed), arena, set, visited)
            }
            SLTNode::Concat(parts) => {
                if let Some(win) = window {
                    // Concat bit layout: LSB is at the end of `parts`.
                    // Walk from LSB side to map the requested window to each part.
                    let mut part_lsb = 0usize;
                    for (part, width) in parts.iter().rev() {
                        let part_msb = part_lsb + width - 1;
                        if win.overlaps(&BitAccess::new(part_lsb, part_msb)) {
                            let ov_lsb = std::cmp::max(win.lsb, part_lsb);
                            let ov_msb = std::cmp::min(win.msb, part_msb);
                            let local = BitAccess::new(ov_lsb - part_lsb, ov_msb - part_lsb);
                            collect_inputs_with_window(*part, Some(local), arena, set, visited);
                        }
                        part_lsb += width;
                    }
                } else {
                    for (part, _) in parts {
                        collect_inputs_with_window(*part, None, arena, set, visited);
                    }
                }
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let pointwise = matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor);
                let lhs_window = pointwise
                    .then(|| dependency_window(window, *lhs, arena))
                    .flatten();
                let rhs_window = pointwise
                    .then(|| dependency_window(window, *rhs, arena))
                    .flatten();
                collect_inputs_with_window(*lhs, lhs_window, arena, set, visited);
                collect_inputs_with_window(*rhs, rhs_window, arena, set, visited);
            }
            SLTNode::Unary(op, inner) => {
                let pointwise =
                    matches!(op, UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::BitNot);
                let inner_window = pointwise
                    .then(|| dependency_window(window, *inner, arena))
                    .flatten();
                collect_inputs_with_window(*inner, inner_window, arena, set, visited);
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                collect_inputs_with_window(*cond, None, arena, set, visited);
                let then_window = dependency_window(window, *then_expr, arena);
                let else_window = dependency_window(window, *else_expr, arena);
                collect_inputs_with_window(*then_expr, then_window, arena, set, visited);
                collect_inputs_with_window(*else_expr, else_window, arena, set, visited);
            }
            SLTNode::ForFold {
                loop_var,
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                if let celox_slt::SLTLoopBound::Expr(node) = start {
                    collect_inputs_with_window(*node, None, arena, set, visited);
                }
                if let celox_slt::SLTLoopBound::Expr(node) = end {
                    collect_inputs_with_window(*node, None, arena, set, visited);
                }
                if let celox_slt::SLTForFoldResult::Transient { initial, update } = result {
                    collect_inputs_with_window(*initial, None, arena, set, visited);
                    collect_inputs_with_window(*update, None, arena, set, visited);
                }
                for init in initials {
                    collect_inputs_with_window(init.expr, None, arena, set, visited);
                }
                for update in updates {
                    collect_inputs_with_window(update.expr, None, arena, set, visited);
                }
                for effect in effects {
                    match effect {
                        celox_slt::SLTForEffect::Event { guard, args, .. } => {
                            if let Some(guard) = guard {
                                collect_inputs_with_window(*guard, None, arena, set, visited);
                            }
                            for arg in args {
                                collect_inputs_with_window(*arg, None, arena, set, visited);
                            }
                        }
                        celox_slt::SLTForEffect::Runner(runner) => {
                            collect_inputs_with_window(*runner, None, arena, set, visited);
                        }
                    }
                }
                collect_inputs_with_window(*continue_cond, None, arena, set, visited);
                set.retain(|atom| atom.id != *loop_var);
            }
            SLTNode::ForFoldGroup {
                loop_var,
                entry_guard,
                states,
                ..
            } => {
                let mut group_inputs = crate::HashSet::default();
                let mut group_visited = HashMap::default();
                collect_inputs_with_window(
                    *entry_guard,
                    None,
                    arena,
                    &mut group_inputs,
                    &mut group_visited,
                );
                for state in states {
                    collect_inputs_with_window(
                        state.initial,
                        None,
                        arena,
                        &mut group_inputs,
                        &mut group_visited,
                    );
                }
                let mut update_inputs = crate::HashSet::default();
                let mut update_visited = HashMap::default();
                for state in states {
                    collect_inputs_with_window(
                        state.update,
                        None,
                        arena,
                        &mut update_inputs,
                        &mut update_visited,
                    );
                }
                update_inputs.retain(|atom| {
                    atom.id != *loop_var && !carried_states_cover_atom(atom, states)
                });
                group_inputs.extend(update_inputs);
                set.extend(group_inputs);
            }
            SLTNode::Constant(_, _, _, _) => {}
        }
    }
}

/// Add `requested` to a sorted union of covered intervals and return only the
/// portions which were not already covered.
fn claim_uncovered_window(covered: &mut Vec<BitAccess>, requested: BitAccess) -> Vec<BitAccess> {
    let mut uncovered = Vec::new();
    let mut cursor = requested.lsb;
    for range in covered.iter().copied() {
        if range.msb < cursor {
            continue;
        }
        if range.lsb > requested.msb {
            break;
        }
        if range.lsb > cursor {
            uncovered.push(BitAccess::new(cursor, requested.msb.min(range.lsb - 1)));
        }
        cursor = cursor.max(range.msb.saturating_add(1));
        if cursor > requested.msb {
            break;
        }
    }
    if cursor <= requested.msb {
        uncovered.push(BitAccess::new(cursor, requested.msb));
    }
    if uncovered.is_empty() {
        return uncovered;
    }

    let mut merged = requested;
    let start = covered.partition_point(|range| range.msb.saturating_add(1) < merged.lsb);
    let mut end = start;
    while end < covered.len() && covered[end].lsb <= merged.msb.saturating_add(1) {
        merged.lsb = merged.lsb.min(covered[end].lsb);
        merged.msb = merged.msb.max(covered[end].msb);
        end += 1;
    }
    covered.splice(start..end, std::iter::once(merged));
    uncovered
}

/// Preserve a result bit window only when it is also a valid operand window.
/// Width-changing operands may use extension bits, so falling back to the full
/// operand is the conservative answer in that case.
fn dependency_window<A: Hash + Eq + Clone + Debug>(
    window: Option<BitAccess>,
    operand: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<BitAccess> {
    window.filter(|window| window.msb < get_width(operand, arena))
}

/// Return whether the union of loop-carried ranges covers every bit of an
/// input atom.  Matching only on the variable ID is unsound for partial state
/// targets because uncovered bits still come from the enclosing storage.
fn carried_states_cover_atom<A: Hash + Eq + Clone>(
    atom: &VarAtomBase<A>,
    states: &[celox_slt::SLTForFoldGroupState<A>],
) -> bool {
    let mut ranges = states
        .iter()
        .filter(|state| state.target.id == atom.id)
        .map(|state| state.target.access)
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|access| (access.lsb, access.msb));

    let mut next = atom.access.lsb;
    for range in ranges {
        if range.msb < next {
            continue;
        }
        if range.lsb > next {
            return false;
        }
        if range.msb >= atom.access.msb {
            return true;
        }
        let Some(after) = range.msb.checked_add(1) else {
            return false;
        };
        next = after;
    }
    false
}
fn convert_logic_path<
    A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    B: Hash + Eq + Clone,
>(
    lp: &LogicPath<A>,
    arena: &SLTNodeArena<A>,
    target_arena: &mut SLTNodeArena<B>,
    cache: &mut HashMap<NodeId, NodeId>,
    f: &impl Fn(&A) -> B,
) -> Result<LogicPath<B>, SLTNodeFactsError> {
    lp.map_addr(arena, target_arena, cache, f)
}

fn convert_comb_observer<
    A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    B: Hash + Eq + Clone,
>(
    observer: &CombObserver<A>,
    arena: &SLTNodeArena<A>,
    target_arena: &mut SLTNodeArena<B>,
    cache: &mut HashMap<NodeId, NodeId>,
    f: &impl Fn(&A) -> B,
) -> Result<CombObserver<B>, SLTNodeFactsError> {
    let mut map_node = |node| {
        arena
            .get(node)
            .map_addr(node, arena, target_arena, cache, f)
    };
    Ok(CombObserver {
        site_id: observer.site_id,
        activation_group: observer.activation_group,
        guard: observer.guard.map(&mut map_node).transpose()?,
        args: observer
            .args
            .iter()
            .copied()
            .map(&mut map_node)
            .collect::<Result<_, _>>()?,
        loop_runner: observer.loop_runner.map(&mut map_node).transpose()?,
        sensitivity: observer
            .sensitivity
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        local_inputs: observer
            .local_inputs
            .iter()
            .map(|(id, node)| {
                Ok((
                    f(id),
                    arena
                        .get(*node)
                        .map_addr(*node, arena, target_arena, cache, f)?,
                ))
            })
            .collect::<Result<_, SLTNodeFactsError>>()?,
        observed_inputs: observer
            .observed_inputs
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        position_inputs: observer
            .position_inputs
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        preceding_writes: observer
            .preceding_writes
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        written_before: observer
            .written_before
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        written_input_atoms: observer
            .written_input_atoms
            .iter()
            .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
            .collect(),
        written_inputs: observer.written_inputs.iter().map(f).collect(),
        captured_in_loop: observer.captured_in_loop,
    })
}

fn convert_glue_block(
    gb: &GlueBlock,
    parent_id: InstanceId,
    child_id: InstanceId,
    arena: &SLTNodeArena<GlueAddr>,
    target_arena: &mut SLTNodeArena<AbsoluteAddr>,
    cache: &mut HashMap<NodeId, NodeId>,
) -> Result<Vec<LogicPath<AbsoluteAddr>>, SLTNodeFactsError> {
    let GlueBlock {
        module_id: _,
        input_ports,
        output_ports,
        arena: _,
    } = gb;
    let cv = &|addr: &GlueAddr| match addr {
        GlueAddr::Parent(v) => AbsoluteAddr {
            instance_id: parent_id,
            var_id: *v,
        },
        GlueAddr::Child(v) => AbsoluteAddr {
            instance_id: child_id,
            var_id: *v,
        },
    };
    let mut res = Vec::new();

    for (_ports, abb) in input_ports {
        res.push(convert_logic_path(abb, arena, target_arena, cache, cv)?);
    }
    for (_ports, abb) in output_ports {
        res.push(convert_logic_path(abb, arena, target_arena, cache, cv)?);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleParser;
    use celox_design::ModuleId;
    use celox_slt::SLTForFoldGroupState;
    use num_bigint::{BigInt, BigUint};
    use veryl_analyzer::{
        self, Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir, VarPath},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::{Parser, resource_table::StrId};

    fn setup(code: &str) -> (HashMap<ModuleId, SimModule>, HashMap<StrId, ModuleId>, Ir) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();

        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = veryl_analyzer::ir::Ir::default();

        let errors = analyzer.analyze_pass1("prj", &parser.veryl);
        assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass1();
        assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");
        let errors = analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir));
        assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass2(&ir);
        assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

        // First pass: assign ModuleIds
        let mut name_to_id: HashMap<StrId, ModuleId> = HashMap::default();
        let mut ir_modules: Vec<(ModuleId, &veryl_analyzer::ir::Module)> = Vec::new();
        let mut next_id = 0usize;
        for component in &ir.components {
            if let Component::Module(module) = component {
                let id = ModuleId(next_id);
                next_id += 1;
                name_to_id.insert(module.name, id);
                ir_modules.push((id, module));
            }
        }

        // Second pass: parse with inst_ids
        let mut sim_modules = HashMap::default();
        for &(mid, module) in &ir_modules {
            let inst_ids: Vec<ModuleId> = module
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Inst(inst) => {
                        let child_name = match &*inst.component {
                            Component::Module(m) => m.name,
                            Component::SystemVerilog(sv) => sv.name,
                            Component::Interface(_) => unreachable!(),
                        };
                        Some(name_to_id[&child_name])
                    }
                    _ => None,
                })
                .collect();
            let sim_module = ModuleParser::parse(module, &crate::BuildConfig::default(), &inst_ids)
                .expect("module parse failed");
            sim_modules.insert(mid, sim_module);
        }

        (sim_modules, name_to_id, ir)
    }

    #[test]
    fn atomization_does_not_remerge_unpacked_elements() {
        let address = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        };
        let source = AbsoluteAddr {
            instance_id: InstanceId(1),
            var_id: VarId::from_raw(1),
        };
        let mut arena = SLTNodeArena::new();
        let expression = arena
            .alloc(SLTNode::Input {
                variable: source,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 23),
            })
            .unwrap();
        let mut sources = crate::HashSet::default();
        sources.insert(VarAtomBase::new(source, 0, 23));
        let path = LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(address, 0, 23)),
            sources,
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr: expression,
        };
        let mut element_widths = HashMap::default();
        element_widths.insert(address, 6);

        let atomized = atomize_logic_paths(
            &vec![path],
            &HashMap::default(),
            &element_widths,
            &mut arena,
        )
        .unwrap();
        let accesses = atomized
            .iter()
            .map(|path| path.target.var().unwrap().access)
            .collect::<Vec<_>>();

        assert_eq!(
            accesses,
            vec![
                BitAccess::new(0, 5),
                BitAccess::new(6, 11),
                BitAccess::new(12, 17),
                BitAccess::new(18, 23),
            ]
        );
        for (path, expected) in atomized.iter().zip(accesses) {
            assert!(matches!(
                arena.get(path.expr),
                SLTNode::Input {
                    variable,
                    access,
                    ..
                } if *variable == source && *access == expected
            ));
        }
    }

    #[test]
    fn atomization_remerges_physically_contiguous_unpacked_elements() {
        let address = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        };
        let source = AbsoluteAddr {
            instance_id: InstanceId(1),
            var_id: VarId::from_raw(1),
        };
        let mut arena = SLTNodeArena::new();
        let expression = arena
            .alloc(SLTNode::Input {
                variable: source,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 31),
            })
            .unwrap();
        let path = LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(address, 0, 31)),
            sources: [VarAtomBase::new(source, 0, 31)].into_iter().collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr: expression,
        };
        let element_widths = [(address, 8)].into_iter().collect();

        let atomized = atomize_logic_paths(
            &vec![path],
            &HashMap::default(),
            &element_widths,
            &mut arena,
        )
        .unwrap();

        assert_eq!(atomized.len(), 1);
        assert_eq!(
            atomized[0].target.var().unwrap().access,
            BitAccess::new(0, 31)
        );
    }

    #[test]
    fn collect_inputs_preserves_slice_window_through_bitwise_mux() {
        let mut arena = SLTNodeArena::<u32>::new();
        let input = |arena: &mut SLTNodeArena<u32>, variable, msb| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, msb),
                })
                .unwrap()
        };
        let old = input(&mut arena, 1, 15);
        let replacement = input(&mut arena, 2, 15);
        let condition = input(&mut arena, 3, 0);
        let merged = arena
            .alloc(SLTNode::Binary(old, BinaryOp::Or, replacement))
            .unwrap();
        let selected = arena
            .alloc(SLTNode::Mux {
                cond: condition,
                then_expr: merged,
                else_expr: old,
            })
            .unwrap();
        let upper = arena
            .alloc(SLTNode::Slice {
                expr: selected,
                access: BitAccess::new(8, 15),
            })
            .unwrap();

        let mut inputs = crate::HashSet::default();
        collect_inputs(upper, &arena, &mut inputs);

        assert!(inputs.contains(&VarAtomBase::new(1, 8, 15)));
        assert!(inputs.contains(&VarAtomBase::new(2, 8, 15)));
        assert!(inputs.contains(&VarAtomBase::new(3, 0, 0)));
        assert!(!inputs.contains(&VarAtomBase::new(1, 0, 15)));
        assert!(!inputs.contains(&VarAtomBase::new(2, 0, 15)));
    }

    #[test]
    fn atomization_coalesces_adjacent_pointwise_ranges_with_the_same_sources() {
        let target = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        };
        let lhs_addr = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(1),
        };
        let rhs_addr = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(2),
        };
        let mut arena = SLTNodeArena::new();
        let mut input = |variable| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 15),
                })
                .unwrap()
        };
        let lhs = input(lhs_addr);
        let rhs = input(rhs_addr);
        let expression = arena
            .alloc(SLTNode::Binary(lhs, BinaryOp::Or, rhs))
            .unwrap();
        let path = LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 15)),
            sources: [
                VarAtomBase::new(lhs_addr, 0, 15),
                VarAtomBase::new(rhs_addr, 0, 15),
            ]
            .into_iter()
            .collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr: expression,
        };
        let boundaries = [(target, (1..16).collect())].into_iter().collect();

        let atomized =
            atomize_logic_paths(&vec![path], &boundaries, &HashMap::default(), &mut arena).unwrap();

        assert_eq!(atomized.len(), 1);
        assert_eq!(
            atomized[0].target.var().unwrap().access,
            BitAccess::new(0, 15)
        );
        assert_eq!(
            atomized[0].sources,
            [
                VarAtomBase::new(lhs_addr, 0, 15),
                VarAtomBase::new(rhs_addr, 0, 15),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn test_flatting_simple_hierarchy() {
        let code = r#"
            module child(
                 i: input logic<1>,
                 o: output logic<1>,
            ) {
                assign o = i;
            }

            module top(
                i: input logic<1>,
                o: output logic<1>,
            ) {
                var i_c: logic<1>;
                var o_c: logic<1>;

                assign i_c = i;

                inst c: child(
                    i: i_c,
                    o: o_c,
                );

                assign o = o_c;
            }
        "#;

        let (sim_modules, name_to_id, ir) = setup(code);

        let top_module_ir = ir
            .components
            .iter()
            .find_map(|c| {
                if let Component::Module(m) = c {
                    if m.name.to_string() == "top" {
                        Some(m)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();
        let child_module_ir = ir
            .components
            .iter()
            .find_map(|c| {
                if let Component::Module(m) = c {
                    if m.name.to_string() == "child" {
                        Some(m)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();

        let top_module_sim = &sim_modules[&name_to_id[&top_module_ir.name]];
        let child_module_sim = &sim_modules[&name_to_id[&child_module_ir.name]];

        let mut instance_ids = HashMap::default();
        let top_path = InstancePath(vec![]);
        let child_instance_name = *top_module_sim.glue_blocks.keys().next().unwrap();
        let child_path = InstancePath(vec![(child_instance_name, 0)]);
        assert!(
            instance_ids
                .insert(top_path.clone(), InstanceId(0))
                .is_none()
        );
        assert!(instance_ids.insert(child_path, InstanceId(1)).is_none());

        let find_var_id = |sim_module: &SimModule, name: &str| {
            let str_id = StrId::from(name);
            let var_path = VarPath(vec![str_id]);
            sim_module.find_var_id(&var_path)
        };

        let top_i_id = find_var_id(top_module_sim, "i");
        let top_o_id = find_var_id(top_module_sim, "o");
        let top_ic_id = find_var_id(top_module_sim, "i_c");
        let top_oc_id = find_var_id(top_module_sim, "o_c");

        let child_i_id = find_var_id(child_module_sim, "i");
        let child_o_id = find_var_id(child_module_sim, "o");

        let mut arena = SLTNodeArena::new();
        let relocation_module = flatten_module(
            top_module_sim,
            &top_path,
            &instance_ids,
            &HashMap::default(),
            &HashMap::default(),
            &mut arena,
        )
        .unwrap()
        .relocation;

        // Expected logic paths:
        // 1. i_c = i; (in top)
        // 2. o = o_c; (in top)
        // 3. c.i = i_c; (glue)
        // 4. o_c = c.o; (glue)
        assert_eq!(relocation_module.comb_blocks.len(), 4);
        let paths = &relocation_module.comb_blocks;

        // Check path 1: i_c = i
        let path1 = paths
            .iter()
            .find(|p| {
                p.target.var().unwrap().id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_ic_id,
                    })
            })
            .unwrap();
        assert_eq!(path1.sources.len(), 1);
        assert!(path1.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_i_id
            },
            0,
            0
        )));

        // Check path 2: o = o_c
        let path2 = paths
            .iter()
            .find(|p| {
                p.target.var().unwrap().id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_o_id,
                    })
            })
            .unwrap();
        assert_eq!(path2.sources.len(), 1);
        assert!(path2.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_oc_id
            },
            0,
            0
        )));

        // Check path 3: c.i = i_c (child input)
        let path3 = paths
            .iter()
            .find(|p| {
                p.target.var().unwrap().id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(1),
                        var_id: child_i_id,
                    })
            })
            .unwrap();
        assert_eq!(path3.sources.len(), 1);
        assert!(path3.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_ic_id
            },
            0,
            0
        )));

        // Check path 4: o_c = c.o (child output)
        let path4 = paths
            .iter()
            .find(|p| {
                p.target.var().unwrap().id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_oc_id,
                    })
            })
            .unwrap();
        assert_eq!(path4.sources.len(), 1);
        assert!(path4.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(1),
                var_id: child_o_id
            },
            0,
            0
        )));
    }

    #[test]
    fn for_fold_group_inputs_keep_initial_but_hide_loop_scoped_update_bindings() {
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
        let uncovered_state_input = arena
            .alloc(SLTNode::Input {
                variable: 2,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(8, 15),
            })
            .unwrap();
        let scoped_sum = arena
            .alloc(SLTNode::Binary(state_input, BinaryOp::Add, loop_input))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(scoped_sum, BinaryOp::Add, external_input))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(
                update,
                BinaryOp::Add,
                uncovered_state_input,
            ))
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

        let mut inputs = crate::HashSet::default();
        collect_inputs(group, &arena, &mut inputs);

        assert!(inputs.contains(&VarAtomBase::new(2, 0, 7)));
        assert!(inputs.contains(&VarAtomBase::new(2, 8, 15)));
        assert!(inputs.contains(&VarAtomBase::new(3, 0, 7)));
        assert!(!inputs.iter().any(|atom| atom.id == 1));
    }
}
