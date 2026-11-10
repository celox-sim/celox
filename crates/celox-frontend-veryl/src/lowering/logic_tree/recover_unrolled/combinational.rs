//! Proving that a feedback path is combinational.

use super::*;

/// Prove that constant-masked writes overwrite preceding activation values.
/// Dependencies are bit sets, computed once per reachable DAG node. Runtime
/// folds remain unsupported by the existing constant specializer.
pub(crate) fn proves_combinational(
    paths: &[LogicPath<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> bool {
    prove_combinational(paths, arena, false)
}

/// Only a diagnostic proof: the executable tree still retains its unmatched
/// branch, so this never changes four-state case semantics.
pub(crate) fn proves_binary_combinational(
    paths: &[LogicPath<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> bool {
    prove_combinational(paths, arena, true)
}

pub(super) fn prove_combinational(
    paths: &[LogicPath<VarId>],
    arena: &SLTNodeArena<VarId>,
    binary_cases: bool,
) -> bool {
    let mut targets: HashMap<VarId, Vec<BitAccess>> = HashMap::default();
    for target in paths.iter().filter_map(|path| path.target.var()) {
        targets.entry(target.id).or_default().push(target.access);
    }
    for ranges in targets.values_mut() {
        ranges.sort_by_key(|range| range.lsb);
        let mut merged: Vec<BitAccess> = Vec::new();
        for range in ranges.drain(..) {
            if let Some(previous) = merged.last_mut()
                && range.lsb <= previous.msb.saturating_add(1)
            {
                previous.msb = previous.msb.max(range.msb);
            } else {
                merged.push(range);
            }
        }
        *ranges = merged;
    }
    let roots = paths.iter().map(|path| path.expr).collect::<Vec<_>>();
    let mut budget = FeedbackProofBudget {
        nodes: 65_536,
        bits: 16_777_216,
        edges: 262_144,
    };
    // Preflight the reachable graph before copying or specializing it. Neither
    // unrelated module logic nor a large output alone determines proof cost.
    let Some(nodes) = feedback_proof_nodes(&roots, arena, &mut budget) else {
        return false;
    };
    // The preflight is iterative and also supplies the cheap no-feedback
    // check. Dynamic reads can reach beyond their base access window.
    if !nodes.iter().any(|&node| {
        let SLTNode::Input {
            variable,
            access,
            index,
            ..
        } = arena.get(node)
        else {
            return false;
        };
        targets.get(variable).is_some_and(|ranges| {
            !index.is_empty() || ranges.iter().any(|range| access.overlaps(range))
        })
    }) {
        return true;
    }
    let mut proof_arena = SLTNodeArena::new();
    let mut mapped = HashMap::default();
    let mut specialized = HashMap::default();
    let mut proof_roots = Vec::new();
    for root in roots {
        let Ok(root) =
            arena
                .get(root)
                .map_addr(root, arena, &mut proof_arena, &mut mapped, &|id| *id)
        else {
            return false;
        };
        let Some(root) = specialize_slt_node(root, None, None, &mut proof_arena, &mut specialized)
        else {
            return false;
        };
        proof_roots.push(root);
    }
    feedback_masks(
        &proof_roots,
        &proof_arena,
        &targets,
        &mut budget,
        binary_cases,
    )
    .is_some_and(|masks| masks.iter().all(Zero::is_zero))
}

pub(super) struct FeedbackProofBudget {
    pub(super) nodes: usize,
    pub(super) bits: usize,
    pub(super) edges: usize,
}

/// Bound every pending edge and bit-set allocation before creating it.
pub(super) fn feedback_proof_nodes(
    roots: &[NodeId],
    arena: &SLTNodeArena<VarId>,
    budget: &mut FeedbackProofBudget,
) -> Option<Vec<NodeId>> {
    budget.edges = budget.edges.checked_sub(roots.len())?;
    let mut pending = roots.to_vec();
    let mut seen = HashSet::default();
    let mut nodes = Vec::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        budget.nodes = budget.nodes.checked_sub(1)?;
        budget.bits = budget.bits.checked_sub(get_width(node, arena))?;
        let mut child = |node| {
            budget.edges = budget.edges.checked_sub(1)?;
            pending.push(node);
            Some(())
        };
        match arena.get(node) {
            SLTNode::Input { index, .. } => {
                for index in index {
                    child(index.node)?;
                }
            }
            SLTNode::Binary(lhs, _, rhs) => {
                child(*lhs)?;
                child(*rhs)?;
            }
            SLTNode::Unary(_, inner)
            | SLTNode::Capture { expr: inner, .. }
            | SLTNode::Slice { expr: inner, .. } => {
                child(*inner)?;
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                child(*cond)?;
                child(*then_expr)?;
                child(*else_expr)?;
            }
            SLTNode::Concat(parts) => {
                for (part, _) in parts {
                    child(*part)?;
                }
            }
            SLTNode::Constant(..) => {}
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        }
        nodes.push(node);
    }
    nodes.sort_unstable();
    Some(nodes)
}

pub(super) fn feedback_masks(
    roots: &[NodeId],
    arena: &SLTNodeArena<VarId>,
    targets: &HashMap<VarId, Vec<BitAccess>>,
    budget: &mut FeedbackProofBudget,
    binary_cases: bool,
) -> Option<Vec<BigUint>> {
    let nodes = feedback_proof_nodes(roots, arena, budget)?;
    let mut masks: HashMap<NodeId, BigUint> = HashMap::default();
    for node in nodes {
        let width = get_width(node, arena);
        let all_if = |feedback: bool| {
            if feedback {
                width_mask(width)
            } else {
                BigUint::zero()
            }
        };
        let mask = match arena.get(node) {
            SLTNode::Input {
                variable,
                access,
                index,
                ..
            } => {
                let ranges = targets.get(variable).map(Vec::as_slice).unwrap_or(&[]);
                let first = ranges.partition_point(|range| range.msb < access.lsb);
                let overlaps = ranges[first..]
                    .iter()
                    .take_while(|range| range.lsb <= access.msb);
                if !index.is_empty() {
                    // An address can itself depend on old process state. For
                    // indexed target reads, keep every possibly selected bit.
                    // `access` can be a base window rather than the complete
                    // dynamically reachable footprint. Any written range of
                    // this variable can therefore supply the selected value.
                    all_if(
                        !ranges.is_empty()
                            || index.iter().any(|index| !masks[&index.node].is_zero()),
                    )
                } else {
                    let mut mask = BigUint::zero();
                    for range in overlaps {
                        let low = range.lsb.max(access.lsb);
                        let high = range.msb.min(access.msb);
                        mask |= width_mask(high - low + 1) << (low - access.lsb);
                    }
                    mask
                }
            }
            SLTNode::Constant(..) => BigUint::zero(),
            SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
                if get_width(*lhs, arena) == width && get_width(*rhs, arena) == width =>
            {
                let survives = |other| {
                    let Some(constant) = specialized_constant(other, arena) else {
                        return width_mask(width);
                    };
                    match op {
                        // X/Z bits cannot erase a dependency. Only a known
                        // zero for AND or known one for OR is controlling.
                        BinaryOp::And => constant.value | constant.mask,
                        BinaryOp::Or => (width_mask(width) ^ constant.value) | constant.mask,
                        _ => width_mask(width),
                    }
                };
                (&masks[lhs] & survives(*rhs)) | (&masks[rhs] & survives(*lhs))
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::Shl | BinaryOp::Shr), rhs)
                if masks[rhs].is_zero() =>
            {
                match specialized_constant(*rhs, arena).filter(|constant| constant.mask.is_zero()) {
                    Some(constant) => {
                        match constant.value.to_usize().filter(|&shift| shift < width) {
                            Some(shift) if *op == BinaryOp::Shl => {
                                (&masks[lhs] << shift) & width_mask(width)
                            }
                            Some(shift) => &masks[lhs] >> shift,
                            None => BigUint::zero(),
                        }
                    }
                    None => all_if(!masks[lhs].is_zero()),
                }
            }
            SLTNode::Binary(lhs, _, rhs) => all_if(!masks[lhs].is_zero() || !masks[rhs].is_zero()),
            SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::BitNot, inner)
                if get_width(*inner, arena) == width =>
            {
                masks[inner].clone()
            }
            SLTNode::Unary(_, inner) => all_if(!masks[inner].is_zero()),
            SLTNode::Capture { expr, .. } => masks[expr].clone(),
            SLTNode::Slice { expr, access } => (&masks[expr] >> access.lsb) & width_mask(width),
            SLTNode::Concat(parts) => {
                let mut mask = BigUint::zero();
                for (part, part_width) in parts {
                    let part_mask =
                        if *part_width > get_width(*part, arena) && !masks[part].is_zero() {
                            width_mask(*part_width)
                        } else {
                            &masks[part] & width_mask(*part_width)
                        };
                    mask = (mask << part_width) | part_mask;
                }
                mask
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                if binary_cases
                    && let Some(mask) = exhaustive_binary_case_mask(node, arena, &masks, budget)
                {
                    mask
                } else if !masks[cond].is_zero() {
                    width_mask(width)
                } else if get_width(*then_expr, arena) == width
                    && get_width(*else_expr, arena) == width
                {
                    &masks[then_expr] | &masks[else_expr]
                } else {
                    all_if(!masks[then_expr].is_zero() || !masks[else_expr].is_zero())
                }
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        };
        masks.insert(node, mask);
    }
    Some(roots.iter().map(|root| masks[root].clone()).collect())
}

/// Recognize a bounded chain of equality tests on the same selector. Counting
/// distinct labels (not arms) prevents duplicates from hiding missing values.
/// Mixed widths, masks, nonconstant labels and signed comparisons are left to
/// the conservative proof. Budget every visited edge, including failed scans.
pub(super) fn exhaustive_binary_case_mask(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    masks: &HashMap<NodeId, BigUint>,
    budget: &mut FeedbackProofBudget,
) -> Option<BigUint> {
    let mut result = BigUint::zero();
    for arm in exhaustive_binary_case(root, arena, budget)? {
        let SLTNode::Mux {
            cond, then_expr, ..
        } = arena.get(arm)
        else {
            unreachable!("case arms are muxes");
        };
        if !masks[cond].is_zero() {
            return None;
        }
        result |= &masks[then_expr];
    }
    Some(result)
}

/// The muxes of a case chain rooted at `root` whose distinct constant labels
/// cover every binary selector value; the last one completes the labels.
pub(super) fn exhaustive_binary_case(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    budget: &mut FeedbackProofBudget,
) -> Option<Vec<NodeId>> {
    let width = get_width(root, arena);
    let mut current = root;
    let mut selector = None;
    let mut labels = HashSet::default();
    let mut arms = Vec::new();
    loop {
        budget.edges = budget.edges.checked_sub(1)?;
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(current)
        else {
            return None;
        };
        if get_width(*then_expr, arena) != width {
            return None;
        }
        let mut condition = *cond;
        // Procedural conditions normalize unknown truth to false.
        while let SLTNode::Unary(UnaryOp::ToTwoState | UnaryOp::Or, inner) = arena.get(condition) {
            budget.edges = budget.edges.checked_sub(1)?;
            condition = *inner;
        }
        let SLTNode::Binary(lhs, BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard, rhs) =
            arena.get(condition)
        else {
            return None;
        };
        let (input, constant) = if let Some(c) = specialized_constant(*rhs, arena) {
            (*lhs, c)
        } else {
            (*rhs, specialized_constant(*lhs, arena)?)
        };
        let bits = get_width(input, arena);
        if bits == 0
            || bits > 8
            || constant.width != bits
            || !constant.mask.is_zero()
            || constant.signed
        {
            return None;
        }
        if selector.is_some_and(|previous| previous != input) {
            return None;
        }
        selector = Some(input);
        let label = constant.value.to_usize()?;
        if label >= 1usize << bits {
            return None;
        }
        labels.insert(label);
        arms.push(current);
        if labels.len() == 1usize << bits {
            return Some(arms);
        }
        current = *else_expr;
    }
}

/// Two-state builds only: end every complete binary case chain at the arm
/// that completes its labels. A two-state selector always matches one of
/// them, so the dropped unmatched branch, the value retained from the
/// previous activation, could never be selected. The paths change only when
/// the result is proved combinational.
pub(crate) fn complete_binary_cases(
    paths: &mut [LogicPath<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> bool {
    let original = paths.iter().map(|path| path.expr).collect::<Vec<_>>();
    let mut budget = FeedbackProofBudget {
        nodes: 65_536,
        bits: 16_777_216,
        edges: 262_144,
    };
    let Some(nodes) = feedback_proof_nodes(&original, arena, &mut budget) else {
        return false;
    };
    // Children precede their users in NodeId order.
    let mut mapped: HashMap<NodeId, NodeId> = HashMap::default();
    for node in nodes {
        let rebuilt = match exhaustive_binary_case(node, arena, &mut budget) {
            Some(arms) => {
                let (&last, rest) = arms.split_last().expect("a complete case has arms");
                let SLTNode::Mux { then_expr, .. } = arena.get(last) else {
                    unreachable!("case arms are muxes");
                };
                let mut chain = mapped[then_expr];
                for &arm in rest.iter().rev() {
                    let SLTNode::Mux {
                        cond, then_expr, ..
                    } = arena.get(arm).clone()
                    else {
                        unreachable!("case arms are muxes");
                    };
                    let Ok(id) = arena.alloc(SLTNode::Mux {
                        cond: mapped[&cond],
                        then_expr: mapped[&then_expr],
                        else_expr: chain,
                    }) else {
                        return false;
                    };
                    chain = id;
                }
                chain
            }
            None => {
                let map = |id: &mut NodeId| *id = mapped[id];
                let mut copy = arena.get(node).clone();
                match &mut copy {
                    SLTNode::Input { index, .. } => {
                        index.iter_mut().for_each(|index| map(&mut index.node));
                    }
                    SLTNode::Binary(lhs, _, rhs) => {
                        map(lhs);
                        map(rhs);
                    }
                    SLTNode::Unary(_, inner)
                    | SLTNode::Capture { expr: inner, .. }
                    | SLTNode::Slice { expr: inner, .. } => map(inner),
                    SLTNode::Mux {
                        cond,
                        then_expr,
                        else_expr,
                    } => {
                        map(cond);
                        map(then_expr);
                        map(else_expr);
                    }
                    SLTNode::Concat(parts) => parts.iter_mut().for_each(|(part, _)| map(part)),
                    SLTNode::Constant(..) => {}
                    SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                        unreachable!("feedback proofs reject folds")
                    }
                }
                if &copy == arena.get(node) {
                    node
                } else {
                    let Ok(id) = arena.alloc(copy) else {
                        return false;
                    };
                    id
                }
            }
        };
        mapped.insert(node, rebuilt);
    }
    for path in paths.iter_mut() {
        path.expr = mapped[&path.expr];
    }
    if proves_combinational(paths, arena) {
        return true;
    }
    for (path, expr) in paths.iter_mut().zip(original) {
        path.expr = expr;
    }
    false
}
