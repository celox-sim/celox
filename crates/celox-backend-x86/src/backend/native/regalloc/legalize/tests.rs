use super::*;
use crate::native::features::X86Features;
use crate::native::mir::{BaseReg, OpSize, SpillDesc, VRegAllocator};

fn select_legacy_shifts(func: &mut MFunction) {
    func.target_features = X86Features::for_test(false);
}

#[test]
fn perm_copy_reports_vreg_exhaustion() {
    let mut vregs = VRegAllocator::new();
    vregs.set_next_for_test(u32::MAX);
    let mut spill_descs = Vec::new();
    let error = alloc_copy(&mut vregs, &mut spill_descs, VReg(0)).unwrap_err();

    assert_eq!(error.rule, "PERM.VREG_EXHAUSTED");
    assert_eq!(vregs.count(), u32::MAX);
}

#[test]
fn fixed_shift_starts_a_single_predecessor_perm_component() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let amount = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    select_legacy_shifts(&mut func);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 8 });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Shl {
        dst: result,
        lhs,
        rhs: amount,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    func.verify();
    let constrained = func
        .blocks
        .iter()
        .position(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
        .expect("shift component exists");
    assert_eq!(cfg.predecessors[constrained].len(), 1);
    assert_eq!(func.blocks[constrained].phis.len(), 2);
    assert_eq!(model.boundaries.len(), 1);
    let matching = model.boundaries[0].match_colors(|_| None).unwrap();
    let fixed = match func.blocks[constrained].insts[0] {
        MInst::Shl { rhs, .. } => rhs,
        _ => unreachable!(),
    };
    assert_eq!(matching[&fixed], PhysReg::RCX);
}

#[test]
fn perm_rows_never_match_the_reserved_state_base_register() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let amount = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    // Legacy shifts force a Perm boundary; the test features reserve R15
    // for the state base.
    select_legacy_shifts(&mut func);
    assert_eq!(func.target_features.allocatable_register_count(), 14);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 8 });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Shl {
        dst: result,
        lhs,
        rhs: amount,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (_cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    let boundary = &model.boundaries[0];
    assert!(boundary.rows.iter().all(|row| !row.allows(PhysReg::R15)));
    // Even a source that prefers R15 is matched elsewhere.
    let matching = boundary.match_colors(|_| Some(PhysReg::R15)).unwrap();
    assert!(matching.values().all(|&color| color != PhysReg::R15));
}

#[test]
fn bmi2_shift_does_not_materialize_a_perm_boundary() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let amount = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    func.target_features = X86Features::for_test(true);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 8 });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Shl {
        dst: result,
        lhs,
        rhs: amount,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let original_vregs = func.vregs.count();

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (_cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();

    assert!(model.boundaries.is_empty());
    assert_eq!(func.blocks.len(), 1);
    assert_eq!(func.vregs.count(), original_vregs);
    assert!(func.blocks[0].phis.is_empty());
}

#[test]
fn incomplete_perm_model_is_a_structured_error() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let amount = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    select_legacy_shifts(&mut func);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 8 });
    block.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    block.push(MInst::Shl {
        dst: result,
        lhs,
        rhs: amount,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (cfg, mut model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    model.boundaries[0].rows.pop();

    let error = model
        .verify(&func, &cfg, super::super::NUM_REGS)
        .unwrap_err();

    assert_eq!(error.rule, "PERM.ROWS_MATCH_PHIS");
    assert!(error.block.is_some());
}

#[test]
fn leading_clobber_is_moved_out_of_a_join_block() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let rhs = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm { dst: lhs, value: 8 });
    entry.push(MInst::LoadImm { dst: rhs, value: 2 });
    entry.push(MInst::Branch {
        cond: rhs,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut other = MBlock::new(BlockId(2));
    other.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::UDiv {
        dst: result,
        lhs,
        rhs,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, other, join];

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    let constrained = func
        .blocks
        .iter()
        .position(|block| matches!(block.insts.first(), Some(MInst::UDiv { .. })))
        .expect("division component exists");
    assert_eq!(cfg.predecessors[constrained].len(), 1);
    assert_eq!(model.boundaries.len(), 1);
}

#[test]
fn one_arm_perm_inserts_a_pruned_idf_merge_phi() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let value = vregs.alloc();
    let amount = vregs.alloc();
    let shifted = vregs.alloc();
    let observed = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    select_legacy_shifts(&mut func);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: value,
        value: 8,
    });
    entry.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut constrained_arm = MBlock::new(BlockId(1));
    constrained_arm.push(MInst::Shl {
        dst: shifted,
        lhs: value,
        rhs: amount,
    });
    constrained_arm.push(MInst::Jump { target: BlockId(3) });
    let mut other_arm = MBlock::new(BlockId(2));
    other_arm.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Mov {
        dst: observed,
        src: value,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, constrained_arm, other_arm, join];

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (_cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    func.verify();

    let constrained = func
        .blocks
        .iter()
        .find(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
        .expect("shift component exists");
    let perm_value = match constrained.insts[0] {
        MInst::Shl { lhs, .. } => lhs,
        _ => unreachable!(),
    };
    assert_ne!(perm_value, value);

    let join = func
        .blocks
        .iter()
        .find(|block| block.id == BlockId(3))
        .expect("join exists");
    let merge = join
        .phis
        .iter()
        .find(|phi| {
            let sources = phi
                .sources
                .iter()
                .map(|(_, source)| *source)
                .collect::<BTreeSet<_>>();
            sources == BTreeSet::from([value, perm_value])
        })
        .expect("the split value is merged at its iterated dominance frontier");
    assert_eq!(merge.sources.len(), 2);
    assert!(matches!(
        join.insts.first(),
        Some(MInst::Mov { src, .. }) if *src == merge.dst
    ));
    assert_eq!(model.boundaries.len(), 1);
}

#[test]
fn loop_perm_inserts_header_phi_and_renames_the_backedge() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let value = vregs.alloc();
    let amount = vregs.alloc();
    let shifted = vregs.alloc();
    let observed = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    select_legacy_shifts(&mut func);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: value,
        value: 8,
    });
    entry.push(MInst::LoadImm {
        dst: amount,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::Shl {
        dst: shifted,
        lhs: value,
        rhs: amount,
    });
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Mov {
        dst: observed,
        src: value,
    });
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, body, exit];

    let initial = super::super::cfg::normalize(&mut func).unwrap();
    let (_cfg, model) =
        materialize_constraint_perms(&mut func, &initial, super::super::NUM_REGS).unwrap();
    func.verify();

    let constrained = func
        .blocks
        .iter()
        .find(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
        .expect("shift component exists");
    let perm_value = match constrained.insts[0] {
        MInst::Shl { lhs, .. } => lhs,
        _ => unreachable!(),
    };
    assert_ne!(perm_value, value);

    let header = func
        .blocks
        .iter()
        .find(|block| block.id == BlockId(1))
        .expect("loop header exists");
    let merge = header
        .phis
        .iter()
        .find(|phi| {
            phi.sources
                .iter()
                .map(|(_, source)| *source)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from([value, perm_value])
        })
        .expect("the loop-carried Perm representative is merged at the header");
    assert_eq!(merge.sources.len(), 2);

    let exit = func
        .blocks
        .iter()
        .find(|block| block.id == BlockId(3))
        .expect("loop exit exists");
    assert!(matches!(
        exit.insts.first(),
        Some(MInst::Mov { src, .. }) if *src == merge.dst
    ));
    assert_eq!(model.boundaries.len(), 1);
}

#[test]
fn allocation_perm_accepts_pre_spill_pressure_above_the_register_count() {
    const LIVE: usize = super::super::NUM_REGS + 2;
    let mut vregs = VRegAllocator::new();
    let inputs = (0..LIVE).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let quotient = vregs.alloc();
    let mut sums = Vec::new();
    for _ in 2..LIVE {
        sums.push(vregs.alloc());
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); LIVE + 1 + sums.len()]);
    let mut block = MBlock::new(BlockId(0));
    for (value, &destination) in inputs.iter().enumerate() {
        block.push(MInst::LoadImm {
            dst: destination,
            value: value as u64 + 1,
        });
    }
    block.push(MInst::UDiv {
        dst: quotient,
        lhs: inputs[0],
        rhs: inputs[1],
    });
    let mut sum = quotient;
    for (&input, &destination) in inputs[2..].iter().zip(&sums) {
        block.push(MInst::Add {
            dst: destination,
            lhs: sum,
            rhs: input,
        });
        sum = destination;
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: sum,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let mut post_spill = func.clone();
    let cfg = super::super::cfg::normalize(&mut post_spill).unwrap();
    let error =
        materialize_constraint_perms(&mut post_spill, &cfg, super::super::NUM_REGS).unwrap_err();
    assert_eq!(error.rule, "PERM.PRESSURE_BOUND");

    let mut allocation_input = func;
    let cfg = super::super::cfg::normalize(&mut allocation_input).unwrap();
    let (_cfg, model) =
        materialize_allocation_constraint_perms(&mut allocation_input, &cfg).unwrap();
    assert_eq!(model.boundaries.len(), 1);
    assert!(model.boundaries[0].rows.len() > super::super::NUM_REGS);
}
