use std::fmt::Debug;
use std::{collections::BTreeSet, hash::Hash};

use crate::{
    HashMap, InstancePath, SourceAddr as AbsoluteAddr, SourceVarId,
    symbolic::artifact::{
        RelocationModule, SimModule, SymbolicGlueAddr as GlueAddr, SymbolicGlueBlock as GlueBlock,
    },
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

/// A recovered loop may bundle independent reductions into one atomic node.
/// Its union of inputs can invent feedback through another combinational
/// process. Split only groups on such cycles: keeping acyclic groups intact
/// preserves their shared loop control and the existing scheduler's choices.
pub(super) fn refine_cyclic_fold_groups(
    paths: &mut [LogicPath<AbsoluteAddr>],
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<(), SLTNodeFactsError> {
    // Other graph errors (for example multiple drivers) are diagnosed by the
    // ordinary scheduler with source information, not by this refinement.
    let Ok(cyclic) = celox_slt::scheduler::cyclic_logic_paths(paths) else {
        return Ok(());
    };
    let mut groups = BTreeSet::new();
    for index in cyclic {
        if let Some((group, _)) = fold_projection(paths[index].expr, arena) {
            groups.insert(group);
        }
    }
    let mut replacements = HashMap::default();
    for group in groups {
        let SLTNode::ForFoldGroup {
            loop_var,
            loop_width,
            loop_signed,
            start,
            step,
            trip_count,
            entry_guard,
            states,
        } = arena.get(group).clone()
        else {
            unreachable!();
        };
        let sources = states
            .iter()
            .map(|state| {
                let mut inputs = crate::HashSet::default();
                collect_inputs(state.update, arena, &mut inputs);
                inputs
            })
            .collect::<Vec<_>>();
        let depends_on = |from: usize, to: usize| {
            sources[from].iter().any(|input| {
                input.id == states[to].target.id && input.access.overlaps(&states[to].target.access)
            })
        };
        // Weak components retain one-way and transitive carried-state
        // recurrences. Initial expressions remain external inputs, since they
        // execute before the loop's local state bindings exist.
        let mut seen = vec![false; states.len()];
        let mut components = Vec::new();
        for seed in 0..states.len() {
            if seen[seed] {
                continue;
            }
            seen[seed] = true;
            let mut pending = vec![seed];
            let mut component = Vec::new();
            while let Some(index) = pending.pop() {
                component.push(index);
                for (other, visited) in seen.iter_mut().enumerate() {
                    if !*visited && (depends_on(index, other) || depends_on(other, index)) {
                        *visited = true;
                        pending.push(other);
                    }
                }
            }
            component.sort_unstable();
            components.push(component);
        }
        if components.len() < 2 {
            continue;
        }
        let mut offsets = Vec::new();
        let mut offset = get_width(group, arena);
        for state in &states {
            let width = state.target.access.msb - state.target.access.lsb + 1;
            offset -= width;
            offsets.push(BitAccess::new(offset, offset + width - 1));
        }
        let mut fragments = Vec::new();
        for component in components {
            let split = arena.alloc(SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start: start.clone(),
                step: step.clone(),
                trip_count,
                entry_guard,
                states: component.iter().map(|&i| states[i].clone()).collect(),
            })?;
            let mut offset = get_width(split, arena);
            for index in component {
                let original = offsets[index];
                let width = original.msb - original.lsb + 1;
                offset -= width;
                let projection = arena.alloc(SLTNode::Slice {
                    expr: split,
                    access: BitAccess::new(offset, offset + width - 1),
                })?;
                fragments.push((original, projection));
            }
        }
        fragments.sort_by_key(|(access, _)| std::cmp::Reverse(access.lsb));
        replacements.insert(group, fragments);
    }
    for path in paths {
        // These recipes carry additional source/statement-position semantics.
        // Leave them intact rather than infer new masks from only `expr`.
        if !path.local_inputs.is_empty()
            || !path.pre_lower_nodes.is_empty()
            || !path.previous_sources.is_empty()
            || !path.address_sources.is_empty()
            || path.target.var().is_none()
        {
            continue;
        }
        let Some((group, access)) = fold_projection(path.expr, arena) else {
            continue;
        };
        let Some(fragments) = replacements.get(&group) else {
            continue;
        };
        let mut parts = Vec::new();
        for &(original, projection) in fragments {
            if !access.overlaps(&original) {
                continue;
            }
            let lsb = access.lsb.max(original.lsb) - original.lsb;
            let msb = access.msb.min(original.msb) - original.lsb;
            let part = arena.alloc(SLTNode::Slice {
                expr: projection,
                access: BitAccess::new(lsb, msb),
            })?;
            parts.push((part, msb - lsb + 1));
        }
        let expr = if parts.len() == 1 {
            parts[0].0
        } else {
            arena.alloc(SLTNode::Concat(parts))?
        };
        let mut inputs = crate::HashSet::default();
        collect_inputs(expr, arena, &mut inputs);
        let original_ids: crate::HashSet<_> = path.sources.iter().map(|source| source.id).collect();
        inputs.retain(|input| original_ids.contains(&input.id));
        path.expr = expr;
        path.sources = inputs;
    }
    Ok(())
}

fn fold_projection(
    node: NodeId,
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> Option<(NodeId, BitAccess)> {
    match arena.get(node) {
        SLTNode::ForFoldGroup { .. } => Some((node, BitAccess::new(0, get_width(node, arena) - 1))),
        SLTNode::Slice { expr, access } => {
            let (group, parent) = fold_projection(*expr, arena)?;
            Some((
                group,
                BitAccess::new(parent.lsb + access.lsb, parent.lsb + access.msb),
            ))
        }
        _ => None,
    }
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
    let cv = &|id: &SourceVarId| AbsoluteAddr {
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
            child_path.push((child_instance_name.clone(), idx));
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
                    comb_capture_enable_always: path.comb_capture_enable_always,
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
            SLTNode::Capture { expr, .. } => {
                let inner_window = dependency_window(window, *expr, arena);
                collect_inputs_with_window(*expr, inner_window, arena, set, visited);
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

enum UncoveredWindows {
    Empty,
    One(BitAccess),
    Multiple(Vec<BitAccess>),
}

impl UncoveredWindows {
    fn push(&mut self, window: BitAccess) {
        match std::mem::replace(self, Self::Empty) {
            Self::Empty => *self = Self::One(window),
            Self::One(first) => *self = Self::Multiple(vec![first, window]),
            Self::Multiple(mut windows) => {
                windows.push(window);
                *self = Self::Multiple(windows);
            }
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

enum UncoveredWindowsIter {
    Empty,
    One(std::option::IntoIter<BitAccess>),
    Multiple(std::vec::IntoIter<BitAccess>),
}

impl Iterator for UncoveredWindowsIter {
    type Item = BitAccess;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::One(window) => window.next(),
            Self::Multiple(windows) => windows.next(),
        }
    }
}

impl IntoIterator for UncoveredWindows {
    type Item = BitAccess;
    type IntoIter = UncoveredWindowsIter;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Empty => UncoveredWindowsIter::Empty,
            Self::One(window) => UncoveredWindowsIter::One(Some(window).into_iter()),
            Self::Multiple(windows) => UncoveredWindowsIter::Multiple(windows.into_iter()),
        }
    }
}

/// Add `requested` to a sorted union of covered intervals and return only the
/// portions which were not already covered. The common zero- and one-window
/// cases stay inline; only a fragmented result allocates a buffer.
fn claim_uncovered_window(covered: &mut Vec<BitAccess>, requested: BitAccess) -> UncoveredWindows {
    let mut uncovered = UncoveredWindows::Empty;
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
mod fold_refinement_tests {
    use super::*;
    use celox_slt::SLTForFoldGroupState;

    fn addr(id: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(id),
        }
    }

    fn atom(id: u32) -> VarAtomBase<AbsoluteAddr> {
        VarAtomBase {
            id: addr(id),
            access: BitAccess::new(0, 0),
        }
    }

    fn input(id: u32, arena: &mut SLTNodeArena<AbsoluteAddr>) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable: addr(id),
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
            })
            .unwrap()
    }

    fn path(id: u32, expr: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> LogicPath<AbsoluteAddr> {
        let mut sources = crate::HashSet::default();
        collect_inputs(expr, arena, &mut sources);
        LogicPath {
            target: LogicPathTarget::Var(atom(id)),
            sources,
            previous_sources: Default::default(),
            address_sources: Default::default(),
            local_inputs: Vec::new(),
            order_before: Default::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    // a reduces an external input; b reduces c. Connecting c to a creates
    // artificial feedback only because the atomic group gives a a read of c.
    fn fixture(coupled: bool) -> (SLTNodeArena<AbsoluteAddr>, Vec<LogicPath<AbsoluteAddr>>) {
        let mut arena = SLTNodeArena::new();
        let zero = arena
            .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 1, false))
            .unwrap();
        let one = arena
            .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
            .unwrap();
        let a = input(0, &mut arena);
        let b = input(1, &mut arena);
        let c = input(2, &mut arena);
        let external = input(3, &mut arena);
        let update_a = arena
            .alloc(SLTNode::Binary(a, BinaryOp::Or, external))
            .unwrap();
        let update_b = arena
            .alloc(SLTNode::Binary(
                b,
                BinaryOp::Or,
                if coupled { a } else { c },
            ))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: addr(4),
                loop_width: 2,
                loop_signed: false,
                start: 0.into(),
                step: 1.into(),
                trip_count: 2,
                entry_guard: one,
                states: vec![
                    SLTForFoldGroupState {
                        target: atom(0),
                        initial: zero,
                        update: update_a,
                    },
                    SLTForFoldGroupState {
                        target: atom(1),
                        initial: zero,
                        update: update_b,
                    },
                ],
            })
            .unwrap();
        let a_result = arena
            .alloc(SLTNode::Slice {
                expr: group,
                access: BitAccess::new(1, 1),
            })
            .unwrap();
        let b_result = arena
            .alloc(SLTNode::Slice {
                expr: group,
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        let paths = vec![path(0, a_result, &arena), path(1, b_result, &arena)];
        (arena, paths)
    }

    #[test]
    fn acyclic_groups_keep_exact_expression_and_arena_identity() {
        let (mut arena, mut paths) = fixture(false);
        let original = paths.clone();
        let nodes = arena.len();
        refine_cyclic_fold_groups(&mut paths, &mut arena).unwrap();
        assert_eq!(paths, original);
        assert_eq!(arena.len(), nodes);
    }

    #[test]
    fn independent_groups_remove_only_artificial_feedback() {
        let (mut arena, mut paths) = fixture(false);
        let a = input(0, &mut arena);
        paths.push(path(2, a, &arena));
        assert!(
            !celox_slt::scheduler::cyclic_logic_paths(&paths)
                .unwrap()
                .is_empty()
        );
        refine_cyclic_fold_groups(&mut paths, &mut arena).unwrap();
        assert!(
            celox_slt::scheduler::cyclic_logic_paths(&paths)
                .unwrap()
                .is_empty()
        );
        assert_eq!(paths[0].sources, [atom(3)].into_iter().collect());
        assert_eq!(paths[1].sources, [atom(2)].into_iter().collect());
    }

    #[test]
    fn actual_feedback_remains_a_cycle_after_refinement() {
        let (mut arena, mut paths) = fixture(false);
        let b = input(1, &mut arena);
        paths.push(path(2, b, &arena));
        refine_cyclic_fold_groups(&mut paths, &mut arena).unwrap();
        assert_eq!(
            celox_slt::scheduler::cyclic_logic_paths(&paths).unwrap(),
            vec![1, 2]
        );
    }

    #[test]
    fn coupled_recurrences_are_not_split_to_hide_feedback() {
        let (mut arena, mut paths) = fixture(true);
        let b = input(1, &mut arena);
        paths.push(path(3, b, &arena));
        let original = paths.clone();
        let nodes = arena.len();
        refine_cyclic_fold_groups(&mut paths, &mut arena).unwrap();
        assert_eq!(paths, original);
        assert_eq!(arena.len(), nodes);
        assert!(
            !celox_slt::scheduler::cyclic_logic_paths(&paths)
                .unwrap()
                .is_empty()
        );
    }
}
