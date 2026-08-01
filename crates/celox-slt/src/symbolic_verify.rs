use std::hash::Hash;

use celox_design::BitAccess;

use crate::{
    CombObserver, HashMap, HashSet, LogicPath, LogicPathTarget, NodeId, SLTNode, SLTNodeArena,
    SLTNodeFacts, SLTNodeFactsError,
};

pub fn verify_symbolic_roots<A>(
    arena: &SLTNodeArena<A>,
    paths: &[LogicPath<A>],
    observers: &[CombObserver<A>],
    variable_widths: &HashMap<A, usize>,
    variable_signedness: &HashMap<A, bool>,
) -> Result<(), SLTNodeFactsError>
where
    A: Hash + Eq + Clone,
{
    let facts = SLTNodeFacts::verify(arena)?;
    let require = |node, role| facts.require_lowerable(node, role);
    let fail = |invariant, node, message| SLTNodeFactsError::new(invariant, node, message);
    let access_width =
        |access: BitAccess, role: &'static str, node: NodeId| -> Result<usize, SLTNodeFactsError> {
            let span = access.msb.checked_sub(access.lsb).ok_or_else(|| {
                fail(
                    "ROOT.ACCESS_ORDERED",
                    node,
                    format!(
                        "{role} access has lsb {} greater than msb {}",
                        access.lsb, access.msb
                    ),
                )
            })?;
            span.checked_add(1).ok_or_else(|| {
                fail(
                    "ROOT.ACCESS_REPRESENTABLE",
                    node,
                    format!(
                        "{role} access [{}:{}] has an unrepresentable width",
                        access.msb, access.lsb
                    ),
                )
            })
        };
    let verify_atom = |id: &A,
                       access: BitAccess,
                       role: &'static str,
                       node: NodeId|
     -> Result<usize, SLTNodeFactsError> {
        let width = access_width(access, role, node)?;
        let Some(&variable_width) = variable_widths.get(id) else {
            return Err(fail(
                "ROOT.VARIABLE_EXISTS",
                node,
                format!("{role} names a variable absent from the semantic type table"),
            ));
        };
        if variable_width == 0 || access.msb >= variable_width {
            return Err(fail(
                "ROOT.ACCESS_IN_VARIABLE_BOUNDS",
                node,
                format!(
                    "{role} access [{}:{}] is outside variable width {variable_width}",
                    access.msb, access.lsb
                ),
            ));
        }
        Ok(width)
    };

    for (node_index, node) in arena.iter().enumerate() {
        let node_id = NodeId(node_index);
        match node {
            SLTNode::Input {
                variable, access, ..
            } => {
                verify_atom(variable, *access, "SLT input", node_id)?;
            }
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                result,
                initials,
                updates,
                ..
            } => {
                let Some(&declared_loop_width) = variable_widths.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD.LOOP_VARIABLE_EXISTS",
                        node_id,
                        "ForFold loop variable is absent from the semantic type table".to_string(),
                    ));
                };
                if *loop_width != declared_loop_width {
                    return Err(fail(
                        "FOR_FOLD.LOOP_WIDTH_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFold loop width {loop_width} does not equal declared width {declared_loop_width}"
                        ),
                    ));
                }
                let Some(&declared_loop_signed) = variable_signedness.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD.LOOP_SIGNEDNESS_EXISTS",
                        node_id,
                        "ForFold loop variable signedness is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_signed != declared_loop_signed {
                    return Err(fail(
                        "FOR_FOLD.LOOP_SIGNEDNESS_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFold loop signedness {loop_signed} does not equal declared signedness {declared_loop_signed}"
                        ),
                    ));
                }
                verify_atom(&result.id, result.access, "ForFold result", node_id)?;
                for update in initials.iter().chain(updates) {
                    verify_atom(
                        &update.target.id,
                        update.target.access,
                        "ForFold state target",
                        node_id,
                    )?;
                }
            }
            SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                states,
                ..
            } => {
                let Some(&declared_loop_width) = variable_widths.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_VARIABLE_EXISTS",
                        node_id,
                        "ForFoldGroup loop variable is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_width != declared_loop_width {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_WIDTH_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFoldGroup loop width {loop_width} does not equal declared width {declared_loop_width}"
                        ),
                    ));
                }
                let Some(&declared_loop_signed) = variable_signedness.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_EXISTS",
                        node_id,
                        "ForFoldGroup loop variable signedness is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_signed != declared_loop_signed {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFoldGroup loop signedness {loop_signed} does not equal declared signedness {declared_loop_signed}"
                        ),
                    ));
                }
                for state in states {
                    verify_atom(
                        &state.target.id,
                        state.target.access,
                        "ForFoldGroup state target",
                        node_id,
                    )?;
                }
            }
            _ => {}
        }
    }

    for (path_index, path) in paths.iter().enumerate() {
        let expression_width = require(path.expr, "logic-path result")?;
        if let LogicPathTarget::Var(target) = &path.target {
            let target_width =
                verify_atom(&target.id, target.access, "logic-path target", path.expr)?;
            if expression_width != target_width {
                return Err(fail(
                    "ROOT.RESULT_WIDTH_MATCHES_TARGET",
                    path.expr,
                    format!(
                        "logic-path result width {expression_width} does not equal target width {target_width}"
                    ),
                ));
            }
        }
        for &node in &path.pre_lower_nodes {
            require(node, "logic-path pre-lower value")?;
        }
        let mut local_ids = HashSet::default();
        for (_, node) in &path.local_inputs {
            require(*node, "logic-path local input")?;
        }
        for (id, _) in &path.local_inputs {
            if !local_ids.insert(id.clone()) {
                return Err(fail(
                    "ROOT.LOCAL_INPUT_ID_UNIQUE",
                    path.expr,
                    "logic-path contains duplicate local-input IDs".to_string(),
                ));
            }
        }
        for source in path
            .sources
            .iter()
            .chain(&path.previous_sources)
            .chain(&path.address_sources)
        {
            verify_atom(&source.id, source.access, "logic-path source", path.expr)?;
        }
        for address in &path.address_sources {
            if !path
                .sources
                .iter()
                .any(|source| source.id == address.id && source.access.overlaps(&address.access))
            {
                return Err(fail(
                    "ROOT.ADDRESS_SOURCE_IS_CURRENT_SOURCE",
                    path.expr,
                    "logic-path address source is absent from current-value sources".to_string(),
                ));
            }
        }
        for &successor in &path.order_before {
            if successor.0 >= paths.len() {
                return Err(fail(
                    "ROOT.ORDER_EDGE_EXISTS",
                    path.expr,
                    format!(
                        "logic path {path_index} orders before missing path {}",
                        successor.0
                    ),
                ));
            }
            if successor.0 == path_index {
                return Err(fail(
                    "ROOT.ORDER_EDGE_NOT_SELF",
                    path.expr,
                    format!("logic path {path_index} contains a self ordering edge"),
                ));
            }
        }
        if let LogicPathTarget::CombCaptureEvent {
            guard,
            args,
            loop_runner,
            ..
        } = &path.target
        {
            if let Some(guard) = guard {
                require(*guard, "capture-event guard")?;
            }
            for &arg in args {
                require(arg, "capture-event argument")?;
            }
            if let Some(loop_runner) = loop_runner {
                require(*loop_runner, "capture-event loop runner")?;
            }
        }
    }

    for observer in observers {
        if let Some(guard) = observer.guard {
            require(guard, "observer guard")?;
        }
        for &arg in &observer.args {
            require(arg, "observer argument")?;
        }
        if let Some(loop_runner) = observer.loop_runner {
            require(loop_runner, "observer loop runner")?;
        }
        let mut local_ids = HashSet::default();
        for (id, node) in &observer.local_inputs {
            require(*node, "observer local input")?;
            if !local_ids.insert(id.clone()) {
                return Err(fail(
                    "ROOT.LOCAL_INPUT_ID_UNIQUE",
                    *node,
                    "observer contains duplicate local-input IDs".to_string(),
                ));
            }
        }
        let diagnostic_node = observer
            .guard
            .or(observer.loop_runner)
            .or_else(|| observer.args.first().copied())
            .unwrap_or(NodeId(0));
        for atom in observer
            .sensitivity
            .iter()
            .chain(&observer.observed_inputs)
            .chain(&observer.position_inputs)
            .chain(&observer.preceding_writes)
            .chain(&observer.written_before)
            .chain(&observer.written_input_atoms)
        {
            verify_atom(&atom.id, atom.access, "observer atom", diagnostic_node)?;
        }
        for id in &observer.written_inputs {
            if !variable_widths.contains_key(id) {
                return Err(fail(
                    "ROOT.VARIABLE_EXISTS",
                    diagnostic_node,
                    "observer written input is absent from the semantic type table".to_string(),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use num_bigint::{BigInt, BigUint};

    use super::verify_symbolic_roots;
    use crate::{
        HashMap, HashSet, LogicPath, LogicPathTarget, SLTForFoldGroupState, SLTForUpdate,
        SLTLoopBound, SLTNode, SLTNodeArena, SLTStepOp,
    };
    use celox_design::VarAtomBase;

    fn path(expr: crate::NodeId) -> LogicPath<u32> {
        LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(2, 0, 7)),
            sources: HashSet::default(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn semantic_tables() -> (HashMap<u32, usize>, HashMap<u32, bool>) {
        (
            [(1, 8), (2, 8)].into_iter().collect(),
            [(1, false), (2, false)].into_iter().collect(),
        )
    }

    #[test]
    fn rejects_legacy_for_fold_loop_signedness_mismatch() {
        let mut arena = SLTNodeArena::new();
        let value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let continue_cond = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let target = VarAtomBase::new(2, 0, 7);
        let fold = arena
            .alloc(SLTNode::ForFold {
                loop_var: 1,
                loop_width: 8,
                loop_signed: true,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(1),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: target,
                initials: vec![SLTForUpdate {
                    target,
                    expr: value,
                }],
                updates: vec![SLTForUpdate {
                    target,
                    expr: value,
                }],
                effects: Vec::new(),
                continue_cond,
            })
            .unwrap();
        let (widths, signedness) = semantic_tables();

        let error =
            verify_symbolic_roots(&arena, &[path(fold)], &[], &widths, &signedness).unwrap_err();
        assert_eq!(error.invariant, "FOR_FOLD.LOOP_SIGNEDNESS_MATCHES_VARIABLE");
    }

    #[test]
    fn rejects_for_fold_group_loop_signedness_mismatch() {
        let mut arena = SLTNodeArena::new();
        let value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 1,
                loop_width: 8,
                loop_signed: true,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(2, 0, 7),
                    initial: value,
                    update: value,
                }],
            })
            .unwrap();
        let (widths, signedness) = semantic_tables();

        let error =
            verify_symbolic_roots(&arena, &[path(group)], &[], &widths, &signedness).unwrap_err();
        assert_eq!(
            error.invariant,
            "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_MATCHES_VARIABLE"
        );
    }
}
