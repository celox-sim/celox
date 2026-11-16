use super::*;
use crate::native::features::X86Features;
use crate::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};

fn analyze_and_color(func: &mut MFunction) -> Result<ColoringResult, ColorError> {
    let cfg = super::super::cfg::normalize(func).unwrap();
    let analysis = super::super::analysis::analyze(func);
    color_ssa(
        func,
        &cfg,
        &analysis,
        &PermModel::default(),
        super::super::NUM_REGS,
    )
}

#[test]
fn streaming_coloring_reuses_colors_only_after_last_use() {
    let mut vregs = VRegAllocator::new();
    let left = vregs.alloc();
    let right = vregs.alloc();
    let sum = vregs.alloc();
    let later = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: left,
        value: 1,
    });
    block.push(MInst::LoadImm {
        dst: right,
        value: 2,
    });
    block.push(MInst::Add {
        dst: sum,
        lhs: left,
        rhs: right,
    });
    block.push(MInst::LoadImm {
        dst: later,
        value: 3,
    });
    block.push(MInst::Add {
        dst: result,
        lhs: sum,
        rhs: later,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let colored = analyze_and_color(&mut func).unwrap();
    assert_ne!(colored.assignment.get(left), colored.assignment.get(right));
    assert_eq!(colored.assignment.get(sum), colored.assignment.get(left));
    assert_ne!(colored.assignment.get(sum), colored.assignment.get(later));
    assert_eq!(colored.assignment.get(result), colored.assignment.get(sum));
}

#[test]
fn word32_instructions_expose_their_destructive_operand_affinities() {
    let unary = [
        MInst::Mov32 {
            dst: VReg(2),
            src: VReg(0),
        },
        MInst::AndImm32 {
            dst: VReg(2),
            src: VReg(0),
            imm: 0xff,
        },
    ];
    for instruction in &unary {
        assert_eq!(
            definition_preferences(instruction),
            [Some(VReg(0)), None],
            "{instruction}"
        );
    }

    let commutative = [
        MInst::Add32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Mul32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::And32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Or32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Xor32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
    ];
    for instruction in &commutative {
        assert_eq!(
            definition_preferences(instruction),
            [Some(VReg(0)), Some(VReg(1))],
            "{instruction}"
        );
    }

    assert_eq!(
        definition_preferences(&MInst::Sub32 {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        }),
        [Some(VReg(0)), None]
    );
}

#[test]
fn instruction_affinity_beats_the_first_free_color() {
    let source = VReg(0);
    let destination = VReg(1);
    let required = vec![None; 2];
    let forbidden = vec![ColorMask::empty(); 2];
    let preferences = HashMap::default();
    let colors = vec![Some(PhysReg::R14), None];

    let color = choose_color(
        BlockId(0),
        Some(0),
        destination,
        ALLOCATABLE_REGS,
        &required,
        &forbidden,
        [Some(source), None],
        &preferences,
        &colors,
        &ActiveColors::default(),
    )
    .unwrap();

    assert_eq!(color, PhysReg::R14);
}

#[test]
fn phi_bundle_matching_avoids_a_greedy_copy() {
    let first_rax = VReg(0);
    let first_rdx = VReg(1);
    let second_rax = VReg(2);
    let first_destination = VReg(3);
    let second_destination = VReg(4);
    let first = PhiNode {
        dst: first_destination,
        sources: vec![(BlockId(0), first_rax), (BlockId(1), first_rdx)],
    };
    let second = PhiNode {
        dst: second_destination,
        sources: vec![(BlockId(0), second_rax)],
    };
    let required = vec![None; 5];
    let forbidden = vec![ColorMask::empty(); 5];
    let preferences = [
        (first_destination, vec![first_rax, first_rdx]),
        (second_destination, vec![second_rax]),
    ]
    .into_iter()
    .collect();
    let colors = vec![
        Some(PhysReg::RAX),
        Some(PhysReg::RDX),
        Some(PhysReg::RAX),
        None,
        None,
    ];

    let result = choose_phi_bundle_colors(
        BlockId(2),
        &[&first, &second],
        &[PhysReg::RAX, PhysReg::RDX],
        &required,
        &forbidden,
        &preferences,
        &colors,
        &ActiveColors::default(),
    )
    .unwrap();

    assert_eq!(result, vec![PhysReg::RDX, PhysReg::RAX]);
}

#[test]
fn loop_phi_bundle_sees_through_an_exact_backedge_snapshot() {
    let mut vregs = VRegAllocator::new();
    let initial = vregs.alloc();
    let loop_condition = vregs.alloc();
    let intermediate = vregs.alloc();
    let snapshot = vregs.alloc();
    let outer_destination = vregs.alloc();
    let left_source = vregs.alloc();
    let right_source = vregs.alloc();
    let inner = PhiNode {
        dst: intermediate,
        sources: vec![(BlockId(10), left_source), (BlockId(11), right_source)],
    };
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: initial,
        value: 0,
    });
    entry.push(MInst::LoadImm {
        dst: loop_condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: left_source,
        value: 2,
    });
    entry.push(MInst::LoadImm {
        dst: right_source,
        value: 3,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    func.push_block(entry);

    let mut outer_header = MBlock::new(BlockId(1));
    outer_header.phis.push(PhiNode {
        dst: outer_destination,
        sources: vec![(BlockId(0), initial), (BlockId(2), snapshot)],
    });
    outer_header.push(MInst::Branch {
        cond: loop_condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    func.push_block(outer_header);

    let mut backedge = MBlock::new(BlockId(2));
    backedge.push(MInst::LoadImm {
        dst: intermediate,
        value: 4,
    });
    backedge.push(MInst::Mov {
        dst: snapshot,
        src: intermediate,
    });
    backedge.push(MInst::Jump { target: BlockId(1) });
    func.push_block(backedge);

    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    func.push_block(exit);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let preferences = phi_preferences(&func, &cfg);
    assert!(
        preferences
            .get(&intermediate)
            .is_some_and(|neighbors| neighbors.contains(&outer_destination))
    );

    let required = vec![None; 7];
    let forbidden = vec![ColorMask::empty(); 7];
    let colors = vec![
        None,
        None,
        None,
        None,
        Some(PhysReg::RAX),
        Some(PhysReg::RDX),
        Some(PhysReg::RBX),
    ];
    let result = choose_phi_bundle_colors(
        BlockId(2),
        &[&inner],
        &[PhysReg::RAX, PhysReg::RDX, PhysReg::RBX],
        &required,
        &forbidden,
        &preferences,
        &colors,
        &ActiveColors::default(),
    )
    .unwrap();

    assert_eq!(result, vec![PhysReg::RAX]);
}

#[test]
fn ordinary_join_does_not_contract_an_edge_snapshot() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let original = vregs.alloc();
    let snapshot = vregs.alloc();
    let other = vregs.alloc();
    let destination = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: original,
        value: 2,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    func.push_block(entry);

    let mut true_block = MBlock::new(BlockId(1));
    true_block.push(MInst::Mov {
        dst: snapshot,
        src: original,
    });
    true_block.push(MInst::Jump { target: BlockId(3) });
    func.push_block(true_block);

    let mut false_block = MBlock::new(BlockId(2));
    false_block.push(MInst::LoadImm {
        dst: other,
        value: 3,
    });
    false_block.push(MInst::Jump { target: BlockId(3) });
    func.push_block(false_block);

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: destination,
        sources: vec![(BlockId(1), snapshot), (BlockId(2), other)],
    });
    join.push(MInst::Return);
    func.push_block(join);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let preferences = phi_preferences(&func, &cfg);
    let destination_preferences = preferences.get(&destination).unwrap();
    assert!(destination_preferences.contains(&snapshot));
    assert!(destination_preferences.contains(&other));
    assert!(!destination_preferences.contains(&original));
}

#[test]
fn fixed_use_is_colored_locally_without_global_precoloring() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let amount = vregs.alloc();
    let fixed = vregs.alloc();
    let shifted = vregs.alloc();
    let later = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    func.target_features = X86Features::for_test(false);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 8 });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Mov {
        dst: fixed,
        src: amount,
    });
    block.push(MInst::Shl {
        dst: shifted,
        lhs,
        rhs: fixed,
    });
    block.push(MInst::Add {
        dst: later,
        lhs,
        rhs: shifted,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let colored = analyze_and_color(&mut func).unwrap();
    assert_eq!(colored.assignment.get(fixed), Some(PhysReg::RCX));
    assert_ne!(colored.assignment.get(lhs), Some(PhysReg::RCX));
}

#[test]
fn perm_rows_are_assigned_by_one_local_perfect_matching() {
    let mut vregs = VRegAllocator::new();
    let left = vregs.alloc();
    let amount = vregs.alloc();
    let shifted = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    func.target_features = X86Features::for_test(false);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: left,
        value: 8,
    });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Shl {
        dst: shifted,
        lhs: left,
        rhs: amount,
    });
    block.push(MInst::Add {
        dst: result,
        lhs: left,
        rhs: shifted,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let initial_cfg = super::super::cfg::normalize(&mut func).unwrap();
    let (cfg, perms) = super::super::legalize::materialize_constraint_perms(
        &mut func,
        &initial_cfg,
        super::super::NUM_REGS,
    )
    .unwrap();
    let analysis = super::super::analysis::analyze(&func);
    let colored = color_ssa(&func, &cfg, &analysis, &perms, super::super::NUM_REGS).unwrap();
    let boundary = &perms.boundaries[0];
    let boundary_block = &func.blocks[cfg.block_index[&boundary.block]];
    let fixed = match boundary_block.insts.first() {
        Some(MInst::Shl { rhs, .. }) => *rhs,
        instruction => panic!("expected constrained shift, got {instruction:?}"),
    };
    let left_after = boundary
        .rows
        .iter()
        .find(|row| row.source == left)
        .unwrap()
        .destination;

    assert_eq!(colored.perm_matching.get(&fixed), Some(&PhysReg::RCX));
    assert_eq!(colored.assignment.get(fixed), Some(PhysReg::RCX));
    assert_eq!(
        colored.assignment.get(left_after),
        colored.assignment.get(left)
    );
}
