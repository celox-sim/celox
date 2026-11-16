use super::*;
use crate::ir::{InstanceId, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn addr() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    }
}

struct UnitBuilder {
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    types: HashMap<RegisterId, RegisterType>,
    next: usize,
}

impl UnitBuilder {
    fn new() -> Self {
        Self {
            instructions: Vec::new(),
            types: HashMap::default(),
            next: 0,
        }
    }

    fn reg(&mut self, width: usize) -> RegisterId {
        let reg = RegisterId(self.next);
        self.next += 1;
        self.types.insert(
            reg,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        reg
    }

    fn imm(&mut self, width: usize, value: u64) -> RegisterId {
        let reg = self.reg(width);
        self.instructions
            .push(SIRInstruction::Imm(reg, SIRValue::new(value)));
        reg
    }

    fn source(&mut self, width: usize) -> RegisterId {
        let reg = self.reg(width);
        self.instructions.push(SIRInstruction::Load(
            reg,
            addr(),
            SIROffset::Static(0),
            width,
        ));
        reg
    }

    fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let reg = self.reg(width);
        self.instructions
            .push(SIRInstruction::Binary(reg, lhs, op, rhs));
        reg
    }

    fn unary(&mut self, width: usize, op: UnaryOp, source: RegisterId) -> RegisterId {
        let reg = self.reg(width);
        self.instructions
            .push(SIRInstruction::Unary(reg, op, source));
        reg
    }

    fn slice(&mut self, source: RegisterId, offset: usize) -> RegisterId {
        let reg = self.reg(1);
        self.instructions
            .push(SIRInstruction::Slice(reg, source, offset, 1));
        reg
    }

    fn mux(
        &mut self,
        width: usize,
        cond: RegisterId,
        then_value: RegisterId,
        else_value: RegisterId,
    ) -> RegisterId {
        let reg = self.reg(width);
        self.instructions
            .push(SIRInstruction::Mux(reg, cond, then_value, else_value));
        reg
    }

    fn finish(mut self, result: RegisterId, width: usize) -> ExecutionUnit<RegionedAbsoluteAddr> {
        self.instructions.push(SIRInstruction::Store(
            addr(),
            SIROffset::Static(0),
            width,
            result,
            Vec::new(),
            Vec::new(),
        ));
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: self.instructions,
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: self.types,
        }
    }
}

fn run(unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    LoopIdiomPass.run(unit, &PassOptions::default());
    unit.verify();
}

fn count_op(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> Option<(UnaryOp, RegisterId)> {
    unit.blocks[&BlockId(0)]
        .instructions
        .iter()
        .find_map(|inst| match inst {
            SIRInstruction::Unary(_, op, source)
                if matches!(
                    op,
                    UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros
                ) =>
            {
                Some((*op, *source))
            }
            _ => None,
        })
}

fn priority_unit(
    guarded: bool,
    procedural_truth: bool,
    bit_order: &[usize],
    values: &[usize],
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let width = bit_order.len();
    let result_width = UnaryOp::CountLeadingZeros.result_width(width);
    let mut b = UnitBuilder::new();
    let source = b.source(width);
    let default = b.imm(result_width, width as u64);
    let one = b.imm(1, 1);
    let mut acc = default;
    for (&bit_index, &value) in bit_order.iter().zip(values) {
        let bit = b.slice(source, bit_index);
        let guard = b.binary(1, bit, BinaryOp::Eq, one);
        let mut cond = if guarded {
            let unmatched = b.binary(1, acc, BinaryOp::Eq, default);
            b.binary(1, guard, BinaryOp::LogicAnd, unmatched)
        } else {
            guard
        };
        if procedural_truth {
            let truth = b.unary(1, UnaryOp::Or, cond);
            cond = b.unary(1, UnaryOp::ToTwoState, truth);
        }
        let value = b.imm(result_width, value as u64);
        acc = b.mux(result_width, cond, value, acc);
    }
    b.finish(acc, result_width)
}

#[test]
fn recovers_guarded_first_write_clz() {
    let mut unit = priority_unit(true, false, &[3, 2, 1, 0], &[0, 1, 2, 3]);
    let source = RegisterId(0);
    run(&mut unit);
    assert_eq!(count_op(&unit), Some((UnaryOp::CountLeadingZeros, source)));
    assert!(
        !unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    );
}

#[test]
fn recovers_priority_count_through_one_bit_procedural_truth() {
    let mut unit = priority_unit(true, true, &[3, 2, 1, 0], &[0, 1, 2, 3]);
    let source = RegisterId(0);
    run(&mut unit);
    assert_eq!(count_op(&unit), Some((UnaryOp::CountLeadingZeros, source)));
}

#[test]
fn recovers_last_write_clz_and_ctz() {
    let mut clz = priority_unit(false, false, &[0, 1, 2, 3], &[3, 2, 1, 0]);
    run(&mut clz);
    assert_eq!(
        count_op(&clz).map(|(op, _)| op),
        Some(UnaryOp::CountLeadingZeros)
    );

    let mut ctz = priority_unit(false, false, &[3, 2, 1, 0], &[3, 2, 1, 0]);
    run(&mut ctz);
    assert_eq!(
        count_op(&ctz).map(|(op, _)| op),
        Some(UnaryOp::CountTrailingZeros)
    );
}

#[test]
fn does_not_change_permuted_priority_order() {
    let mut unit = priority_unit(true, false, &[3, 1, 2, 0], &[0, 1, 2, 3]);
    run(&mut unit);
    assert_eq!(count_op(&unit), None);
    assert!(
        unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    );
}

#[test]
fn recovers_conditional_increment_popcount() {
    let width = 8;
    let result_width = UnaryOp::PopCount.result_width(width);
    let mut b = UnitBuilder::new();
    let source = b.source(width);
    let zero = b.imm(result_width, 0);
    let one = b.imm(result_width, 1);
    let mut acc = zero;
    for bit_index in 0..width {
        let cond = b.slice(source, bit_index);
        let incremented = b.binary(result_width, acc, BinaryOp::Add, one);
        acc = b.mux(result_width, cond, incremented, acc);
    }
    let mut unit = b.finish(acc, result_width);
    run(&mut unit);
    assert_eq!(count_op(&unit), Some((UnaryOp::PopCount, source)));
}

#[test]
fn keeps_shared_complex_predicates_and_removes_the_recurrence() {
    let width = 4;
    let result_width = UnaryOp::PopCount.result_width(width);
    let mut b = UnitBuilder::new();
    let source = b.source(width);
    let enable = b.source(1);
    let zero = b.imm(result_width, 0);
    let one = b.imm(result_width, 1);
    let mut acc = zero;
    for bit_index in 0..width {
        let bit = b.slice(source, bit_index);
        let cond = b.binary(1, bit, BinaryOp::LogicAnd, enable);
        let incremented = b.binary(result_width, acc, BinaryOp::Add, one);
        acc = b.mux(result_width, cond, incremented, acc);
    }
    let mut unit = b.finish(acc, result_width);
    run(&mut unit);

    assert_eq!(count_op(&unit).map(|(op, _)| op), Some(UnaryOp::PopCount));
    assert!(
        unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(_, args) if args.len() == width))
    );
    assert!(
        !unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    );
}

#[test]
fn recovers_zero_extended_additive_popcount() {
    let width = 4;
    let result_width = UnaryOp::PopCount.result_width(width);
    let mut b = UnitBuilder::new();
    let source = b.source(width);
    let zero = b.imm(result_width, 0);
    let padding = b.imm(result_width - 1, 0);
    let mut acc = zero;
    for bit_index in 0..width {
        let bit = b.slice(source, bit_index);
        let extended = b.reg(result_width);
        b.instructions
            .push(SIRInstruction::Concat(extended, vec![padding, bit]));
        acc = b.binary(result_width, acc, BinaryOp::Add, extended);
    }
    let mut unit = b.finish(acc, result_width);
    run(&mut unit);
    assert_eq!(count_op(&unit), Some((UnaryOp::PopCount, source)));
}

#[test]
fn reuses_a_growing_or_reduction_prefix() {
    let mut b = UnitBuilder::new();
    let p0 = b.source(1);
    let p1 = b.source(1);
    let p2 = b.source(1);

    let first_concat = b.reg(2);
    b.instructions
        .push(SIRInstruction::Concat(first_concat, vec![p1, p0]));
    let first_reduction = b.reg(1);
    b.instructions.push(SIRInstruction::Unary(
        first_reduction,
        UnaryOp::Or,
        first_concat,
    ));

    let growing_concat = b.reg(3);
    b.instructions
        .push(SIRInstruction::Concat(growing_concat, vec![p2, p1, p0]));
    let result = b.reg(1);
    b.instructions
        .push(SIRInstruction::Unary(result, UnaryOp::Or, growing_concat));
    let mut unit = b.finish(result, 1);

    run(&mut unit);

    assert!(unit.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(dst, lhs, BinaryOp::Or, rhs)
                if *dst == result && *lhs == p2 && *rhs == first_reduction
        )
    }));
    assert!(
        !unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(dst, _) if *dst == growing_concat))
    );
}

#[test]
fn does_not_reassociate_a_reordered_or_reduction() {
    let mut b = UnitBuilder::new();
    let p0 = b.source(1);
    let p1 = b.source(1);
    let p2 = b.source(1);

    let first_concat = b.reg(2);
    b.instructions
        .push(SIRInstruction::Concat(first_concat, vec![p1, p0]));
    let first_reduction = b.reg(1);
    b.instructions.push(SIRInstruction::Unary(
        first_reduction,
        UnaryOp::Or,
        first_concat,
    ));

    let reordered = b.reg(3);
    b.instructions
        .push(SIRInstruction::Concat(reordered, vec![p2, p0, p1]));
    let result = b.reg(1);
    b.instructions
        .push(SIRInstruction::Unary(result, UnaryOp::Or, reordered));
    let mut unit = b.finish(result, 1);

    run(&mut unit);

    assert!(unit.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(dst, UnaryOp::Or, src)
                if *dst == result && *src == reordered
        )
    }));
}

#[test]
fn does_not_reuse_a_multi_bit_growing_or_prefix() {
    let mut b = UnitBuilder::new();
    let p0 = b.source(1);
    let p1 = b.source(1);
    let wide_prefix = b.source(2);

    let first_concat = b.reg(2);
    b.instructions
        .push(SIRInstruction::Concat(first_concat, vec![p1, p0]));
    let first_reduction = b.reg(1);
    b.instructions.push(SIRInstruction::Unary(
        first_reduction,
        UnaryOp::Or,
        first_concat,
    ));

    let growing_concat = b.reg(4);
    b.instructions.push(SIRInstruction::Concat(
        growing_concat,
        vec![wide_prefix, p1, p0],
    ));
    let result = b.reg(1);
    b.instructions
        .push(SIRInstruction::Unary(result, UnaryOp::Or, growing_concat));
    let mut unit = b.finish(result, 1);

    run(&mut unit);

    assert!(unit.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(dst, UnaryOp::Or, src)
                if *dst == result && *src == growing_concat
        )
    }));
}

#[test]
fn leaves_growing_or_reductions_unchanged_in_four_state_mode() {
    let mut b = UnitBuilder::new();
    let p0 = b.source(1);
    let p1 = b.source(1);
    let p2 = b.source(1);

    let first_concat = b.reg(2);
    b.instructions
        .push(SIRInstruction::Concat(first_concat, vec![p1, p0]));
    let first_reduction = b.reg(1);
    b.instructions.push(SIRInstruction::Unary(
        first_reduction,
        UnaryOp::Or,
        first_concat,
    ));

    let growing_concat = b.reg(3);
    b.instructions
        .push(SIRInstruction::Concat(growing_concat, vec![p2, p1, p0]));
    let result = b.reg(1);
    b.instructions
        .push(SIRInstruction::Unary(result, UnaryOp::Or, growing_concat));
    let mut unit = b.finish(result, 1);
    let original = unit.clone();
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };

    LoopIdiomPass.run(&mut unit, &options);

    unit.verify();
    assert_eq!(unit.blocks, original.blocks);
    assert_eq!(unit.register_map, original.register_map);
}
