use super::*;

fn profitability_with_probability(
    true_arm_cost: u128,
    false_arm_cost: u128,
    phi_copy_cost: u128,
    live_through_cost: u128,
    probability: StaticBranchProbability,
) -> BranchProfitability {
    BranchProfitability {
        true_arm_cost,
        false_arm_cost,
        removed_mux_cost: 1,
        probability,
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost,
        live_through_cost,
    }
}

fn profitability(
    true_arm_cost: u128,
    false_arm_cost: u128,
    phi_copy_cost: u128,
    live_through_cost: u128,
) -> BranchProfitability {
    profitability_with_probability(
        true_arm_cost,
        false_arm_cost,
        phi_copy_cost,
        live_through_cost,
        StaticBranchProbability::EVEN,
    )
}

#[test]
fn one_expensive_arm_must_pay_for_its_unselected_half() {
    // Expected savings: 24 / 2 + 1 = 13. Introduced cost: 11 + 2 = 13.
    // Equality is deliberately rejected because it does not prove a win.
    assert!(!profitability(24, 0, 2, 0).proves_expected_benefit());
}

#[test]
fn work_on_both_arms_can_prove_expected_benefit() {
    // Expected savings: (20 + 20) / 2 + 1 = 21. Introduced cost: 13.
    assert!(profitability(20, 20, 2, 0).proves_expected_benefit());
}

#[test]
fn live_through_cost_can_turn_a_candidate_into_a_rejection() {
    assert!(profitability(20, 10, 2, 0).proves_expected_benefit());
    // Expected savings and introduced cost are now both 16.
    assert!(!profitability(20, 10, 2, 3).proves_expected_benefit());
}

#[test]
fn decoder_probability_can_prove_a_local_expected_win() {
    assert!(!profitability(10, 0, 0, 0).proves_expected_benefit());
    assert!(
        profitability_with_probability(10, 0, 0, 0, StaticBranchProbability::EQUALITY_TO_CONSTANT,)
            .proves_expected_benefit()
    );
}

#[test]
fn static_probability_tracks_constant_equality_and_inversion() {
    let eu = unit(vec![
        imm(1, 7),
        SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::Ident, RegisterId(1)),
        SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(0),
            crate::ir::BinaryOp::EqWildcard,
            RegisterId(5),
        ),
        SIRInstruction::Unary(RegisterId(3), crate::ir::UnaryOp::LogicNot, RegisterId(2)),
        SIRInstruction::Binary(
            RegisterId(4),
            RegisterId(0),
            crate::ir::BinaryOp::Ne,
            RegisterId(1),
        ),
    ]);
    let block = &eu.blocks[&BlockId(0)];
    let def_pos = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| def_reg(inst).map(|register| (register, idx)))
        .collect::<HashMap<_, _>>();

    assert_eq!(
        static_true_probability(block, &def_pos, RegisterId(2)),
        StaticBranchProbability::EQUALITY_TO_CONSTANT,
    );
    assert_eq!(
        static_true_probability(block, &def_pos, RegisterId(3)),
        StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
    );
    assert_eq!(
        static_true_probability(block, &def_pos, RegisterId(4)),
        StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
    );
    assert_eq!(
        static_true_probability(block, &def_pos, RegisterId(0)),
        StaticBranchProbability::EVEN,
    );
}

#[test]
fn runtime_work_cost_scales_with_width_and_operation() {
    let mut register_map = HashMap::default();
    for register in [RegisterId(1), RegisterId(2)] {
        register_map.insert(
            register,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
    }
    let mul = SIRInstruction::Binary(
        RegisterId(2),
        RegisterId(1),
        crate::ir::BinaryOp::Mul,
        RegisterId(1),
    );
    let div = SIRInstruction::Binary(
        RegisterId(2),
        RegisterId(1),
        crate::ir::BinaryOp::DivU,
        RegisterId(1),
    );
    assert_eq!(branchified_instruction_cost(&mul, &register_map), 5);
    assert_eq!(branchified_instruction_cost(&div, &register_map), 12);

    for register in [RegisterId(1), RegisterId(2)] {
        register_map.insert(
            register,
            RegisterType::Bit {
                width: 128,
                signed: false,
            },
        );
    }
    assert_eq!(branchified_instruction_cost(&mul, &register_map), 20);
    assert_eq!(branchified_instruction_cost(&div, &register_map), 48);
}
