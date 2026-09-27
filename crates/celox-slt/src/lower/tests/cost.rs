use super::*;

#[test]
fn sparse_cost_cache_reset_matches_fresh_analysis_across_roots_and_arenas() {
    let reused = SLTToSIRLowerer::new(false);
    for padding in [2048, 4096, 1024] {
        let mut arena = SLTNodeArena::<u32>::new();
        for variable in 0..padding {
            input(&mut arena, variable, 64);
        }
        let leaf = input(&mut arena, padding, 64);
        let shared = operation_chain(&mut arena, leaf, BinaryOp::Add, 4, 3, 64);
        let other = operation_chain(&mut arena, leaf, BinaryOp::DivU, 2, 7, 64);
        let root = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Xor, shared))
            .unwrap();
        for (roots, honor) in [
            (vec![root], true),
            (vec![other, shared], false),
            (vec![shared], true),
        ] {
            let materialized = [(shared, RegisterId(0))].into_iter().collect();
            let fresh = SLTToSIRLowerer::new(false);
            for lowerer in [&reused, &fresh] {
                lowerer.reset_cost_cache_roots(&roots, &arena, &materialized, honor);
            }
            let new_variable = arena.len() as u32 + padding;
            let grown = input(&mut arena, new_variable, 32);
            for lowerer in [&reused, &fresh] {
                // Analyze a node outside the reset traversal as well as a
                // materialized subtree. Both must be cleared next time.
                for node in [root, shared, other, leaf, grown] {
                    lowerer.estimated_tree_cost(node, &arena);
                    lowerer.owned_tree_cost(node, &arena);
                    lowerer.owned_slice_lower_cost(node, &arena);
                    lowerer.contains_shared_nontrivial(node, &arena);
                    lowerer.is_speculatable_pure(node, &arena);
                    lowerer.contains_div_rem(node, &arena);
                }
            }
            let actual = reused.cost_cache.borrow();
            let expected = fresh.cost_cache.borrow();
            assert_eq!(actual.tree_costs, expected.tree_costs, "tree_costs");
            assert_eq!(
                actual.contains_div_rem, expected.contains_div_rem,
                "contains_div_rem"
            );
            assert_eq!(actual.fanout, expected.fanout, "fanout");
            assert_eq!(
                actual.initially_materialized, expected.initially_materialized,
                "initially_materialized"
            );
            assert_eq!(actual.owned_costs, expected.owned_costs, "owned_costs");
            assert_eq!(
                actual.owned_slice_lower_costs, expected.owned_slice_lower_costs,
                "owned_slice_lower_costs"
            );
            assert_eq!(
                actual.contains_shared_nontrivial, expected.contains_shared_nontrivial,
                "contains_shared_nontrivial"
            );
            assert_eq!(
                actual.is_speculatable_pure, expected.is_speculatable_pure,
                "is_speculatable_pure"
            );
            assert_eq!(
                actual.traversal_seen, expected.traversal_seen,
                "traversal_seen"
            );
            assert!(actual.touched.len() < 64);
        }
    }
}

#[test]
fn fold_body_cost_analysis_uses_the_environment_scoped_cache() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 8, 8);
    let previous = input(&mut arena, 10, 8);
    let cond = input_bit(&mut arena, 10, 0);
    let divisor = constant(&mut arena, 2, 8);
    let quotient = arena
        .alloc(SLTNode::Binary(previous, BinaryOp::DivU, divisor))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr: quotient,
            else_expr: previous,
        })
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let unavailable_outer_value = builder.alloc_logic(8);
    builder.emit(SIRInstruction::Imm(
        unavailable_outer_value,
        SIRValue::new(0u8),
    ));
    let mut cache = crate::HashMap::default();
    cache.insert(quotient, unavailable_outer_value);
    SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut cache);
    let eu = finish_lowering(builder);

    assert_eq!(
        branch_count(&eu),
        3,
        "the body-local division arm must retain its mandatory lazy branch"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
        )),
        1,
    );
    eu.verify_result().unwrap();
}

#[test]
fn nested_mux_analysis_is_linear_in_dag_size() {
    let mut arena = SLTNodeArena::new();
    let mut value = input(&mut arena, 0, 64);
    for depth in 0..256u32 {
        let cond = input(&mut arena, 1 + depth * 2, 1);
        let arm_input = input(&mut arena, 2 + depth * 2, 64);
        let arm = operation_chain(
            &mut arena,
            arm_input,
            BinaryOp::Add,
            4,
            1_000 + u64::from(depth) * 8,
            64,
        );
        value = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: arm,
                else_expr: value,
            })
            .unwrap();
    }

    let lowerer = SLTToSIRLowerer::new(false);
    let mut builder = SIRBuilder::new();
    lowerer.lower(&mut builder, value, &arena, &mut crate::HashMap::default());
    let visits = lowerer.analysis_node_visits();
    let node_count = arena.len();
    finish_lowering(builder);

    assert!(
        visits <= node_count * 20,
        "analysis revisited {visits} nodes for a {node_count}-node nested mux DAG",
    );
}

#[test]
fn unrelated_global_cache_does_not_enter_mux_analysis() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let then_input = input(&mut arena, 1, 64);
    let else_input = input(&mut arena, 2, 64);
    let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
    let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 8, 100, 64);
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();

    let empty_lowerer = SLTToSIRLowerer::new(false);
    let mut empty_builder = SIRBuilder::new();
    empty_lowerer.lower(
        &mut empty_builder,
        mux,
        &arena,
        &mut crate::HashMap::default(),
    );
    let empty_visits = empty_lowerer.analysis_node_visits();
    finish_lowering(empty_builder);

    let mut large_cache = crate::HashMap::default();
    for index in 0..20_000usize {
        large_cache.insert(NodeId(arena.len() + index), RegisterId(index));
    }
    let cached_lowerer = SLTToSIRLowerer::new(false);
    let mut cached_builder = SIRBuilder::new();
    cached_lowerer.lower(&mut cached_builder, mux, &arena, &mut large_cache);
    let cached_visits = cached_lowerer.analysis_node_visits();
    finish_lowering(cached_builder);

    assert_eq!(cached_visits, empty_visits);
}
