//! Regression coverage for moving a memory read to a later extraction.
use super::{BitExtractPeepholePass, ConcatFoldingPass};
use crate::ir::*;
use crate::optimizer::pipeline::pass_manager::ExecutionUnitPass;
use crate::{HashMap, PassOptions};

fn addr(id: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(id),
        var_id: celox_design::StateObjectId::default(),
    }
}

fn writes(address: RegionedAbsoluteAddr) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
    vec![
        SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            32,
            RegisterId(9),
            vec![],
            vec![],
        ),
        SIRInstruction::Commit(addr(2), address, SIROffset::Static(0), 32, vec![]),
        SIRInstruction::Call {
            function: FunctionId(0),
            frame: RegisterId(9),
            args: vec![],
            outputs: vec![],
            dst: None,
            effects: std::sync::Arc::new(CallEffects {
                writes: vec![address],
                ..Default::default()
            }),
        },
        SIRInstruction::ExternCall {
            dst: None,
            func: crate::ir::extern_abi::NATIVE_KERNEL_CALL_TAG,
            args: vec![],
        },
    ]
}

#[test]
fn concat_loads_keep_the_original_snapshot_across_state_writes() {
    check_snapshot(0);
}

#[test]
fn concat_slices_keep_the_original_snapshot_across_state_writes() {
    check_snapshot(1);
}

#[test]
fn slice_keeps_the_original_snapshot_across_state_writes() {
    check_snapshot(2);
}

#[test]
fn shift_mask_keeps_the_original_snapshot_across_state_writes() {
    check_snapshot(3);
}

fn check_snapshot(form: usize) {
    for write in writes(addr(0)) {
        for cross_block in [false, true] {
            let mut unit = extraction_unit(form, Some(write.clone()), cross_block);
            let original = unit.clone();
            run(form, &mut unit);
            assert_eq!(
                evaluate(
                    &unit,
                    &[],
                    [(addr(0), 0x12345678), (addr(2), 0xdeadbeef)]
                        .into_iter()
                        .collect()
                ),
                evaluate(
                    &original,
                    &[],
                    [(addr(0), 0x12345678), (addr(2), 0xdeadbeef)]
                        .into_iter()
                        .collect()
                ),
                "form={form}, write={write:?}, cross_block={cross_block}"
            );
        }
    }
}

#[test]
fn extraction_still_folds_without_an_aliasing_write() {
    for form in 0..4 {
        // A disjoint object does not invalidate the snapshot.
        for write in [
            None,
            Some(writes(addr(3)).remove(0)),
            Some(writes(addr(3)).remove(2)),
        ] {
            let mut unit = extraction_unit(form, write, false);
            run(form, &mut unit);
            assert!(unit.blocks[&BlockId(0)].instructions.iter().any(|inst| matches!(inst,
                SIRInstruction::Load(_, address, SIROffset::Static(offset), width)
                if *address == addr(0) && ((*offset == 8 && *width == 8) || (*offset == 0 && *width == 12)))), "form={form}");
        }
    }
}

#[test]
fn concat_checks_every_load_snapshot_in_a_run() {
    let mut unit = extraction_unit(0, Some(writes(addr(0)).remove(0)), false);
    let instructions = &mut unit.blocks.get_mut(&BlockId(0)).unwrap().instructions;
    // Only the low nibble is read after the Store. The adjacent high nibbles
    // still refer to the old state and cannot join a new wide Load.
    let low_nibble = instructions.remove(0);
    instructions.insert(4, low_nibble);
    let original = unit.clone();
    run(0, &mut unit);
    let state = [(addr(0), 0x12345678)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    assert_eq!(evaluate(&original, &[], state.clone())[&addr(1)], 0x67f);
    assert_eq!(evaluate(&unit, &[], state)[&addr(1)], 0x67f);
}

fn run(form: usize, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    if form < 2 {
        ConcatFoldingPass::default().run(unit, &PassOptions::default());
    } else {
        BitExtractPeepholePass.run(unit, &PassOptions::default());
    }
}

fn extraction_unit(
    form: usize,
    write: Option<SIRInstruction<RegionedAbsoluteAddr>>,
    cross_block: bool,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let r = RegisterId;
    let mut before = if form == 0 {
        (0..3)
            .map(|i| SIRInstruction::Load(r(i), addr(0), SIROffset::Static(i * 4), 4))
            .collect::<Vec<_>>()
    } else {
        vec![SIRInstruction::Load(
            r(0),
            addr(0),
            SIROffset::Static(0),
            32,
        )]
    };
    before.push(SIRInstruction::Imm(r(9), SIRValue::new(0xdeadbeefu64)));
    let mut after = write.into_iter().collect::<Vec<_>>();
    match form {
        0 => after.push(SIRInstruction::Concat(r(6), vec![r(2), r(1), r(0)])),
        1 => {
            for i in 0..3 {
                after.push(SIRInstruction::Slice(r(i + 1), r(0), i * 4, 4));
            }
            after.push(SIRInstruction::Concat(r(6), vec![r(3), r(2), r(1)]));
        }
        2 => after.push(SIRInstruction::Slice(r(6), r(0), 8, 8)),
        3 => after.extend([
            SIRInstruction::Imm(r(1), SIRValue::new(8u64)),
            SIRInstruction::Binary(r(2), r(0), BinaryOp::Shr, r(1)),
            SIRInstruction::Imm(r(3), SIRValue::new(0xffu64)),
            SIRInstruction::Binary(r(6), r(2), BinaryOp::And, r(3)),
        ]),
        _ => unreachable!(),
    }
    after.push(SIRInstruction::Store(
        addr(1),
        SIROffset::Static(0),
        if form < 2 { 12 } else { 8 },
        r(6),
        vec![],
        vec![],
    ));
    let mut blocks = HashMap::default();
    if cross_block {
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: after,
                terminator: SIRTerminator::Return,
            },
        );
    } else {
        before.extend(after);
    }
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: before,
            terminator: if cross_block {
                SIRTerminator::Jump(BlockId(1), vec![])
            } else {
                SIRTerminator::Return
            },
        },
    );
    let mut register_map = (0..10)
        .map(|i| (r(i), RegisterType::Logic { width: 32 }))
        .collect::<HashMap<_, _>>();
    if form < 2 {
        for i in if form == 0 { 0..3 } else { 1..4 } {
            register_map.insert(r(i), RegisterType::Logic { width: 4 });
        }
    }
    register_map.insert(
        r(6),
        RegisterType::Logic {
            width: if form < 2 { 12 } else { 8 },
        },
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

// Deliberately small independent evaluator: unknown operations fail rather than
// being silently ignored. Calls in these fixtures write a fixed sentinel value.
pub(super) fn evaluate(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    inputs: &[u64],
    mut state: HashMap<RegionedAbsoluteAddr, u64>,
) -> HashMap<RegionedAbsoluteAddr, u64> {
    let mut values = HashMap::default();
    let mut block_id = unit.entry_block_id;
    let mut arguments = inputs.to_vec();
    for _ in 0..100 {
        let block = &unit.blocks[&block_id];
        for (param, value) in block.params.iter().zip(&arguments) {
            values.insert(*param, *value);
        }
        for inst in &block.instructions {
            let result = match inst {
                SIRInstruction::Imm(dst, value) => Some((
                    *dst,
                    value.payload.to_u64_digits().first().copied().unwrap_or(0),
                )),
                SIRInstruction::Load(dst, address, SIROffset::Static(offset), width) => {
                    Some((*dst, state[address] >> offset & mask(*width)))
                }
                SIRInstruction::Slice(dst, source, offset, width) => {
                    Some((*dst, values[source] >> offset & mask(*width)))
                }
                SIRInstruction::Concat(dst, sources) => Some((
                    *dst,
                    sources.iter().fold(0, |v, src| {
                        (v << unit.register_map[src].width()) | values[src]
                    }),
                )),
                SIRInstruction::Binary(dst, lhs, op, rhs) => Some((
                    *dst,
                    match op {
                        BinaryOp::Shr => values[lhs] >> values[rhs],
                        BinaryOp::And => values[lhs] & values[rhs],
                        BinaryOp::Eq => u64::from(values[lhs] == values[rhs]),
                        BinaryOp::Add => values[lhs].wrapping_add(values[rhs]),
                        BinaryOp::Sub => values[lhs].wrapping_sub(values[rhs]),
                        BinaryOp::Mul => values[lhs].wrapping_mul(values[rhs]),
                        _ => panic!("unsupported {inst:?}"),
                    },
                )),
                SIRInstruction::Unary(dst, UnaryOp::ToTwoState | UnaryOp::Ident, src) => {
                    Some((*dst, values[src]))
                }
                SIRInstruction::Mux(dst, cond, yes, no) => {
                    Some((*dst, values[if values[cond] != 0 { yes } else { no }]))
                }
                SIRInstruction::Store(address, SIROffset::Static(offset), width, src, _, _) => {
                    let old = state.get(address).copied().unwrap_or(0);
                    state.insert(
                        *address,
                        (old & !(mask(*width) << offset))
                            | ((values[src] & mask(*width)) << offset),
                    );
                    None
                }
                SIRInstruction::Commit(src, dst, SIROffset::Static(0), 32, _) => {
                    state.insert(*dst, state[src]);
                    None
                }
                SIRInstruction::Call { effects, .. } => {
                    for address in &effects.writes {
                        state.insert(*address, 0xdeadbeef);
                    }
                    None
                }
                SIRInstruction::ExternCall {
                    func: crate::ir::extern_abi::NATIVE_KERNEL_CALL_TAG,
                    ..
                } => {
                    state.insert(addr(0), 0xdeadbeef);
                    None
                }
                _ => panic!("unsupported {inst:?}"),
            };
            if let Some((dst, value)) = result {
                values.insert(dst, value & mask(unit.register_map[&dst].width()));
            }
        }
        let edge = match &block.terminator {
            SIRTerminator::Return => return state,
            SIRTerminator::Jump(id, args) => (*id, args.clone()),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if values[cond] != 0 {
                    true_block.clone()
                } else {
                    false_block.clone()
                }
            }
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => (
                cases
                    .iter()
                    .find(|case| case.value == values[selector].into())
                    .map_or(*default, |case| case.target),
                vec![],
            ),
            other => panic!("unsupported {other:?}"),
        };
        block_id = edge.0;
        arguments = edge.1.iter().map(|reg| values[reg]).collect();
    }
    panic!("fixture did not terminate");
}
fn mask(width: usize) -> u64 {
    u64::MAX >> (64 - width)
}
