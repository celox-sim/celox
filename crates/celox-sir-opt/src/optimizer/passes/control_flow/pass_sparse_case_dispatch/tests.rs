#![allow(
    clippy::disallowed_types,
    reason = "test-only: the tests pass the pass's lazily filled caches"
)]
use super::*;
use crate::ir::{InstanceId, SIRValue};
use celox_design::StateObjectId as VarId;

struct FixtureBuilder {
    next_register: usize,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: HashMap<RegisterId, RegisterType>,
}

impl FixtureBuilder {
    fn new() -> Self {
        Self {
            next_register: 0,
            instructions: Vec::new(),
            register_map: HashMap::default(),
        }
    }

    fn register(&mut self, width: usize) -> RegisterId {
        let reg = RegisterId(self.next_register);
        self.next_register += 1;
        self.register_map.insert(
            reg,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        reg
    }

    fn immediate(&mut self, width: usize, value: u64) -> RegisterId {
        self.immediate_value(width, SIRValue::new(value))
    }

    fn immediate_value(&mut self, width: usize, value: SIRValue) -> RegisterId {
        let dst = self.register(width);
        self.instructions.push(SIRInstruction::Imm(dst, value));
        dst
    }

    fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let dst = self.register(width);
        self.instructions
            .push(SIRInstruction::Binary(dst, lhs, op, rhs));
        dst
    }

    fn exact_condition(&mut self, selector: RegisterId, key: u64, op: BinaryOp) -> RegisterId {
        let width = self.register_map[&selector].width();
        let key = self.immediate(width, key);
        self.binary(1, selector, op, key)
    }

    fn exact_condition_with_zero_extended_key(
        &mut self,
        selector: RegisterId,
        key_width: usize,
        key: u64,
        op: BinaryOp,
    ) -> RegisterId {
        let selector_width = self.register_map[&selector].width();
        assert!(key_width < selector_width);
        let high = self.immediate(selector_width - key_width, 0);
        let low = self.immediate(key_width, key);
        let key = self.concat(vec![high, low]);
        self.binary(1, selector, op, key)
    }

    fn grouped_condition(&mut self, conditions: &[RegisterId]) -> RegisterId {
        assert!(!conditions.is_empty());
        conditions[1..].iter().fold(conditions[0], |lhs, &rhs| {
            self.binary(1, lhs, BinaryOp::LogicOr, rhs)
        })
    }

    fn concat(&mut self, args: Vec<RegisterId>) -> RegisterId {
        let width = args.iter().map(|arg| self.register_map[arg].width()).sum();
        let dst = self.register(width);
        self.instructions.push(SIRInstruction::Concat(dst, args));
        dst
    }

    fn slice(&mut self, source: RegisterId, offset: usize, width: usize) -> RegisterId {
        let dst = self.register(width);
        self.instructions
            .push(SIRInstruction::Slice(dst, source, offset, width));
        dst
    }

    fn zero_extend(&mut self, source: RegisterId, extension: usize) -> RegisterId {
        assert!(extension > 0);
        let zero = self.immediate(extension, 0);
        self.concat(vec![zero, source])
    }

    fn expensive_value(&mut self, seed: u64, factor: RegisterId) -> RegisterId {
        let mut value = self.immediate(64, seed);
        for _ in 0..6 {
            value = self.binary(64, value, BinaryOp::Mul, factor);
        }
        value
    }

    fn mux(
        &mut self,
        cond: RegisterId,
        true_value: RegisterId,
        false_value: RegisterId,
    ) -> RegisterId {
        let width = self.register_map[&true_value].width();
        let dst = self.register(width);
        self.instructions
            .push(SIRInstruction::Mux(dst, cond, true_value, false_value));
        dst
    }

    fn ident(&mut self, source: RegisterId) -> RegisterId {
        let dst = self.register(self.register_map[&source].width());
        self.instructions
            .push(SIRInstruction::Unary(dst, UnaryOp::Ident, source));
        dst
    }

    fn observe(&mut self, source: RegisterId) {
        self.instructions.push(SIRInstruction::Store(
            address(usize::MAX),
            SIROffset::Static(0),
            self.register_map[&source].width(),
            source,
            Vec::new(),
            Vec::new(),
        ));
    }

    fn finish(self, params: Vec<RegisterId>) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let block = BasicBlock {
            id: BlockId(0),
            params,
            instructions: self.instructions,
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: self.register_map,
        }
    }
}

#[test]
fn cached_dominance_matches_path_removal_on_branching_and_looping_cfgs() {
    for pattern in 0..256usize {
        let mut builder = FixtureBuilder::new();
        let condition = builder.register(1);
        let mut eu = builder.finish(vec![condition]);
        for block in 0..4 {
            let id = BlockId(block * 7);
            let next = BlockId(((block + 1) % 4) * 7);
            let other = BlockId(((pattern >> (block * 2)) & 3) * 7);
            eu.blocks.insert(
                id,
                BasicBlock {
                    id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: condition,
                        true_block: (next, Vec::new()),
                        false_block: (other, Vec::new()),
                    },
                },
            );
        }
        let cache = OnceCell::new();
        for dominator in 0..5 {
            for target in 0..5 {
                let (dominator, target) = (BlockId(dominator * 7), BlockId(target * 7));
                assert_eq!(
                    cached_block_dominates(&eu, dominator, target, &cache),
                    block_dominates(&eu, dominator, target)
                );
            }
        }
        assert!(cache.get().unwrap().is_some());
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(99), Vec::new());
        let invalid = OnceCell::new();
        assert_eq!(
            cached_block_dominates(&eu, BlockId(7), BlockId(14), &invalid),
            block_dominates(&eu, BlockId(7), BlockId(14))
        );
        assert!(invalid.get().unwrap().is_none());
    }
}

fn address(instance: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(instance),
        var_id: VarId::default(),
    }
}

fn expensive_duplicate_fixture() -> (ExecutionUnit<RegionedAbsoluteAddr>, RegisterId, RegisterId) {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(17, factor);
    // The final stage is outermost and deliberately shadows key 1.
    for (stage, key) in [0, 2, 5, 7, 9, 11, 15, 2].into_iter().enumerate() {
        let cond = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
        let value = builder.expensive_value(30 + stage as u64, factor);
        previous = builder.mux(cond, value, previous);
    }
    let output = builder.ident(previous);
    builder.observe(output);
    (builder.finish(vec![selector]), selector, output)
}

fn cross_block_case_fixture(
    case_count: usize,
    keep_first_condition_live: bool,
) -> (
    ExecutionUnit<RegionedAbsoluteAddr>,
    RegisterId,
    RegisterId,
    Vec<RegisterId>,
) {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(7);
    let default_value = builder.immediate(64, 0xf0);
    let mut conditions = Vec::with_capacity(case_count);
    let mut values = Vec::with_capacity(case_count);
    for key in 0..case_count {
        conditions.push(builder.exact_condition(selector, key as u64, BinaryOp::EqWildcard));
        values.push(builder.immediate(64, 0x100 + key as u64));
    }
    if keep_first_condition_live {
        builder.instructions.push(SIRInstruction::Store(
            address(usize::MAX - 1),
            SIROffset::Static(0),
            1,
            conditions[0],
            Vec::new(),
            Vec::new(),
        ));
    }

    let split = builder.instructions.len();
    let mut previous = default_value;
    for (&condition, &value) in conditions.iter().zip(&values) {
        previous = builder.mux(condition, value, previous);
    }
    let output = builder.ident(previous);
    builder.observe(output);
    let mut eu = builder.finish(vec![selector]);
    let successor_instructions = eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .split_off(split);
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
        SIRTerminator::Jump(BlockId(1), Vec::new());
    eu.blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: successor_instructions,
            terminator: SIRTerminator::Return,
        },
    );
    (eu, selector, output, conditions)
}

fn has_definition(eu: &ExecutionUnit<RegionedAbsoluteAddr>, reg: RegisterId) -> bool {
    eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(reg))
    })
}

fn serial_cases_fixture(
    count: usize,
    width: usize,
) -> (
    ExecutionUnit<RegionedAbsoluteAddr>,
    RegisterId,
    Vec<RegisterId>,
) {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(width);
    let mut factor = builder.immediate(64, 3);
    let mut outputs = Vec::new();
    for chain in 0..count {
        let mut value = builder.expensive_value(17 + chain as u64, factor);
        for key in [1, 3, 7, 11] {
            let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
            let arm = builder.expensive_value(30 + key, factor);
            value = builder.mux(cond, arm, value);
        }
        factor = builder.ident(value);
        builder.observe(factor);
        outputs.push(factor);
    }
    (builder.finish(vec![selector]), selector, outputs)
}

#[test]
fn batches_disjoint_cases_and_preserves_cross_segment_values_and_tail() {
    for width in [4, 64] {
        for count in [1, 8, 32] {
            let (mut eu, selector, outputs) = serial_cases_fixture(count, width);
            eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
                SIRTerminator::Jump(BlockId(1), Vec::new());
            eu.blocks.insert(
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            );
            eu.verify();
            let plans = find_sparse_case_plans(&eu, &HashMap::default(), None);
            assert_eq!(
                plans.len(),
                count,
                "all disjoint cases should share one discovery"
            );
            let expected = (0..16)
                .flat_map(|input| outputs.iter().map(move |&output| (input, output)))
                .map(|(input, output)| evaluate(&eu, selector, input, output))
                .collect::<Vec<_>>();
            let mut next_block = 2;
            let mut next_register = eu.register_map.keys().map(|r| r.0).max().unwrap() + 1;
            let touched =
                apply_sparse_case_batch(&mut eu, plans, &mut next_block, &mut next_register);
            eu.verify();
            let candidates = touched.into_iter().collect();
            assert!(find_sparse_case_plans(&eu, &HashMap::default(), Some(&candidates)).is_empty());
            assert!(!eu.blocks.values().any(|b| {
                b.instructions
                    .iter()
                    .any(|i| matches!(i, SIRInstruction::Mux(..)))
            }));
            let actual = (0..16)
                .flat_map(|input| outputs.iter().map(move |&output| (input, output)))
                .map(|(input, output)| evaluate(&eu, selector, input, output))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn overlapping_cases_are_rediscovered_without_stale_instruction_indices() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut left = builder.expensive_value(17, factor);
    let mut right = builder.expensive_value(19, factor);
    for key in [1, 3, 7, 11] {
        let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
        let a = builder.expensive_value(30 + key, factor);
        let b = builder.expensive_value(40 + key, factor);
        left = builder.mux(cond, a, left);
        right = builder.mux(cond, b, right);
    }
    builder.observe(left);
    builder.observe(right);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    assert_eq!(
        find_sparse_case_plans(&eu, &HashMap::default(), None).len(),
        1
    );
    let expected = (0..16)
        .map(|input| {
            (
                evaluate(&eu, selector, input, left),
                evaluate(&eu, selector, input, right),
            )
        })
        .collect::<Vec<_>>();
    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    assert!(eu.blocks.len() > 1);
    // Interleaving can leave a chain whose arms no longer satisfy the
    // sinking proof. It must remain intact rather than reuse stale plans.
    assert!(find_sparse_case_plans(&eu, &HashMap::default(), None).is_empty());
    let actual = (0..16)
        .map(|input| {
            (
                evaluate(&eu, selector, input, left),
                evaluate(&eu, selector, input, right),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn shared_dead_prefix_is_removed_before_rediscovering_remaining_cases() {
    let (mut eu, selector, outputs) = serial_cases_fixture(8, 4);
    let unused = RegisterId(eu.register_map.keys().map(|r| r.0).max().unwrap() + 1);
    eu.register_map.insert(
        unused,
        RegisterType::Bit {
            width: 64,
            signed: false,
        },
    );
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
        0,
        SIRInstruction::Imm(unused, crate::ir::SIRValue::new(BigUint::from(123u64))),
    );
    eu.verify();
    // All initial plans remove the same dead prefix, so only one is safe
    // to apply from this snapshot. The others must be rediscovered.
    assert_eq!(
        find_sparse_case_plans(&eu, &HashMap::default(), None).len(),
        1
    );
    let expected = (0..16)
        .map(|input| evaluate(&eu, selector, input, outputs[7]))
        .collect::<Vec<_>>();
    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    assert!(!eu.blocks.values().any(|b| {
        b.instructions
            .iter()
            .any(|i| matches!(i, SIRInstruction::Mux(..)))
    }));
    let actual = (0..16)
        .map(|input| evaluate(&eu, selector, input, outputs[7]))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn batch_reserves_cumulative_block_id_capacity() {
    let (mut eu, selector, outputs) = serial_cases_fixture(2, 4);
    let sentinel = BlockId(usize::MAX - 10);
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Jump(sentinel, Vec::new());
    eu.blocks.insert(
        sentinel,
        BasicBlock {
            id: sentinel,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        },
    );
    eu.verify();
    assert_eq!(
        find_sparse_case_plans(&eu, &HashMap::default(), None).len(),
        2
    );
    let expected = (0..16)
        .map(|input| evaluate(&eu, selector, input, outputs[1]))
        .collect::<Vec<_>>();
    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    let actual = (0..16)
        .map(|input| evaluate(&eu, selector, input, outputs[1]))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert!(
        eu.blocks.values().any(|b| b
            .instructions
            .iter()
            .any(|i| matches!(i, SIRInstruction::Mux(..)))),
        "only one rewrite fits in the remaining ID space"
    );
}

#[test]
fn sparse_dead_seed_worklist_keeps_existing_dead_dags_and_live_uses() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let unused_leaf = builder.immediate(64, 123);
    let unused_root = builder.binary(64, unused_leaf, BinaryOp::Add, unused_leaf);
    let live = builder.immediate(64, 987);
    builder.observe(live);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(17, factor);
    for key in 0..4 {
        let cond = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
        let value = builder.expensive_value(30 + key, factor);
        previous = builder.mux(cond, value, previous);
    }
    let output = builder.ident(previous);
    builder.observe(output);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    let expected = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    let defs = local_definition_positions(&eu.blocks[&BlockId(0)]);
    let plans = find_sparse_case_plans(&eu, &HashMap::default(), None);
    assert_eq!(plans.len(), 1);
    assert!(plans[0].dead_defs.contains(&defs[&unused_root]));
    assert!(plans[0].dead_defs.contains(&defs[&unused_leaf]));
    assert!(!plans[0].dead_defs.contains(&defs[&live]));

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    let actual = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn lowers_prioritized_duplicates_to_direct_semantic_dispatch() {
    let (mut eu, selector, output) = expensive_duplicate_fixture();
    eu.verify();
    let expected = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }));
    assert!(matches!(
        eu.blocks[&eu.entry_block_id].terminator,
        SIRTerminator::Switch { .. }
    ));
    let actual = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    // The outer duplicate is stage 7 (seed 37), rather than stage 0.
    assert_eq!(actual[2], BigUint::from(37u8) * BigUint::from(3u8).pow(6));
}

#[test]
fn lowers_grouped_keys_across_exact_zero_extensions() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let selector_5 = builder.zero_extend(selector, 1);
    let selector_6 = builder.zero_extend(selector, 2);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(10, factor);

    let condition_13 = [
        builder.exact_condition(selector_5, 1, BinaryOp::EqWildcard),
        builder.exact_condition(selector_5, 3, BinaryOp::EqWildcard),
    ];
    let condition_13 = builder.grouped_condition(&condition_13);
    let value_13 = builder.expensive_value(20, factor);
    previous = builder.mux(condition_13, value_13, previous);

    let condition_246 = [
        builder.exact_condition_with_zero_extended_key(selector_6, 4, 2, BinaryOp::EqWildcard),
        builder.exact_condition_with_zero_extended_key(selector_6, 4, 4, BinaryOp::EqWildcard),
        builder.exact_condition_with_zero_extended_key(selector_6, 4, 6, BinaryOp::EqWildcard),
    ];
    let condition_246 = builder.grouped_condition(&condition_246);
    let value_246 = builder.expensive_value(30, factor);
    previous = builder.mux(condition_246, value_246, previous);

    let condition_89 = [
        builder.exact_condition(selector, 8, BinaryOp::Eq),
        builder.exact_condition(selector, 9, BinaryOp::Eq),
    ];
    let condition_89 = builder.grouped_condition(&condition_89);
    let value_89 = builder.expensive_value(40, factor);
    previous = builder.mux(condition_89, value_89, previous);

    let output = builder.ident(previous);
    builder.observe(output);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    let expected = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }));
    let actual = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn grouped_duplicates_and_overlaps_preserve_outer_priority() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(10, factor);

    for (keys, seed) in [
        (&[1, 2, 2][..], 20),
        (&[2, 3, 3][..], 30),
        (&[4, 5][..], 40),
    ] {
        let leaves = keys
            .iter()
            .map(|&key| builder.exact_condition(selector, key, BinaryOp::EqWildcard))
            .collect::<Vec<_>>();
        let condition = builder.grouped_condition(&leaves);
        let value = builder.expensive_value(seed, factor);
        previous = builder.mux(condition, value, previous);
    }
    let output = builder.ident(previous);
    builder.observe(output);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    let expected = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    let actual = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(actual[2], BigUint::from(30u8) * BigUint::from(3u8).pow(6));
}

#[test]
fn lowers_bitwise_or_with_exact_concat_and_slice_keys() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let sliced_selector = builder.slice(selector, 0, 4);

    let key_high = builder.immediate(2, 0);
    let key_low = builder.immediate(2, 1);
    let concat_key = builder.concat(vec![key_high, key_low]);
    let concat_condition = builder.binary(1, sliced_selector, BinaryOp::Eq, concat_key);

    let key_source = builder.immediate(8, 0b10_10_01);
    let slice_key = builder.slice(key_source, 2, 4);
    let slice_condition = builder.binary(1, selector, BinaryOp::Eq, slice_key);
    let condition = builder.binary(1, concat_condition, BinaryOp::Or, slice_condition);

    let factor = builder.immediate(64, 3);
    let make_value = |builder: &mut FixtureBuilder, seed| {
        let mut value = builder.immediate(64, seed);
        for _ in 0..16 {
            value = builder.binary(64, value, BinaryOp::Mul, factor);
        }
        value
    };
    let default_value = make_value(&mut builder, 10);
    let case_value = make_value(&mut builder, 20);
    let result = builder.mux(condition, case_value, default_value);
    let output = builder.ident(result);
    builder.observe(output);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    let expected = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }));
    let actual = (0..16)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn rejects_grouped_mixed_selectors_and_nonzero_extensions() {
    let mut mixed = FixtureBuilder::new();
    let selector_a = mixed.register(4);
    let selector_b = mixed.register(4);
    let factor = mixed.immediate(64, 3);
    let mut previous = mixed.expensive_value(1, factor);
    for key in 0..4 {
        let leaves = [
            mixed.exact_condition(selector_a, key, BinaryOp::EqWildcard),
            mixed.exact_condition(selector_b, key + 4, BinaryOp::EqWildcard),
        ];
        let condition = mixed.grouped_condition(&leaves);
        let value = mixed.expensive_value(20 + key, factor);
        previous = mixed.mux(condition, value, previous);
    }
    mixed.ident(previous);
    let mut mixed = mixed.finish(vec![selector_a, selector_b]);
    mixed.verify();
    let original = mixed.clone();

    SparseCaseDispatchPass::default().run(&mut mixed, &PassOptions::default());

    assert_eq!(mixed.blocks, original.blocks);
    assert_eq!(mixed.register_map, original.register_map);

    let mut extended = FixtureBuilder::new();
    let selector = extended.register(4);
    let zero = extended.immediate(1, 0);
    let one = extended.immediate(1, 1);
    let zero_extended = extended.concat(vec![zero, selector]);
    let nonzero_extended = extended.concat(vec![one, selector]);
    let factor = extended.immediate(64, 3);
    let mut previous = extended.expensive_value(1, factor);
    for key in 0..4 {
        let leaves = [
            extended.exact_condition(zero_extended, key, BinaryOp::Eq),
            extended.exact_condition(nonzero_extended, 16 + key, BinaryOp::Eq),
        ];
        let condition = extended.grouped_condition(&leaves);
        let value = extended.expensive_value(30 + key, factor);
        previous = extended.mux(condition, value, previous);
    }
    extended.ident(previous);
    let mut extended = extended.finish(vec![selector]);
    extended.verify();
    let original = extended.clone();

    SparseCaseDispatchPass::default().run(&mut extended, &PassOptions::default());

    assert_eq!(extended.blocks, original.blocks);
    assert_eq!(extended.register_map, original.register_map);
}

#[test]
fn rejects_grouped_signed_width_changing_identity_casts() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    builder.register_map.insert(
        selector,
        RegisterType::Bit {
            width: 4,
            signed: true,
        },
    );
    let cast = builder.register(8);
    builder.register_map.insert(
        cast,
        RegisterType::Bit {
            width: 8,
            signed: true,
        },
    );
    builder
        .instructions
        .push(SIRInstruction::Unary(cast, UnaryOp::Ident, selector));
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(1, factor);
    for key in 0..4 {
        let leaves = [
            builder.exact_condition(selector, key, BinaryOp::Eq),
            builder.exact_condition(cast, key, BinaryOp::Eq),
        ];
        let condition = builder.grouped_condition(&leaves);
        let value = builder.expensive_value(40 + key, factor);
        previous = builder.mux(condition, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();
    let original = eu.clone();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    assert_eq!(eu.blocks, original.blocks);
    assert_eq!(eu.register_map, original.register_map);
}

#[test]
fn rejects_mixed_selectors_and_masked_or_reversed_wildcards() {
    let mut mixed = FixtureBuilder::new();
    let selector_a = mixed.register(4);
    let selector_b = mixed.register(4);
    let factor = mixed.immediate(64, 3);
    let mut previous = mixed.expensive_value(1, factor);
    for key in 0..8 {
        let selector = if key % 2 == 0 { selector_a } else { selector_b };
        let cond = mixed.exact_condition(selector, key, BinaryOp::EqWildcard);
        let value = mixed.expensive_value(10 + key, factor);
        previous = mixed.mux(cond, value, previous);
    }
    mixed.ident(previous);
    let mut mixed = mixed.finish(vec![selector_a, selector_b]);
    let original = mixed.clone();
    SparseCaseDispatchPass::default().run(&mut mixed, &PassOptions::default());
    assert_eq!(mixed.blocks, original.blocks);

    let mut inexact = FixtureBuilder::new();
    let selector = inexact.register(4);
    let factor = inexact.immediate(64, 3);
    let mut previous = inexact.expensive_value(1, factor);
    for key in 0..4 {
        let key_reg = inexact.immediate_value(
            4,
            SIRValue::new_four_state(key, if key % 2 == 0 { 1u8 } else { 0u8 }),
        );
        let cond = if key % 2 == 0 {
            inexact.binary(1, selector, BinaryOp::EqWildcard, key_reg)
        } else {
            // A constant on the wildcard LHS is not an exact case key.
            inexact.binary(1, key_reg, BinaryOp::EqWildcard, selector)
        };
        let value = inexact.expensive_value(20 + key, factor);
        previous = inexact.mux(cond, value, previous);
    }
    inexact.ident(previous);
    let mut inexact = inexact.finish(vec![selector]);
    let original = inexact.clone();
    SparseCaseDispatchPass::default().run(&mut inexact, &PassOptions::default());
    assert_eq!(inexact.blocks, original.blocks);
}

#[test]
fn rejects_width_changing_constant_casts() {
    let mut cast_key = FixtureBuilder::new();
    let selector = cast_key.register(8);
    let factor = cast_key.immediate(64, 3);
    let mut previous = cast_key.expensive_value(1, factor);
    for raw_key in 8..12 {
        let key_source = cast_key.immediate(4, raw_key);
        cast_key.register_map.insert(
            key_source,
            RegisterType::Bit {
                width: 4,
                signed: true,
            },
        );
        let key_cast = cast_key.register(8);
        cast_key
            .instructions
            .push(SIRInstruction::Unary(key_cast, UnaryOp::Ident, key_source));
        let cond = cast_key.binary(1, selector, BinaryOp::Eq, key_cast);
        let value = cast_key.expensive_value(20 + raw_key, factor);
        previous = cast_key.mux(cond, value, previous);
    }
    cast_key.ident(previous);
    let mut cast_key = cast_key.finish(vec![selector]);
    cast_key.verify();
    let original = cast_key.clone();

    SparseCaseDispatchPass::default().run(&mut cast_key, &PassOptions::default());

    assert_eq!(cast_key.blocks, original.blocks);

    let mut cast_selector = FixtureBuilder::new();
    let selector_source = cast_selector.register(4);
    cast_selector.register_map.insert(
        selector_source,
        RegisterType::Bit {
            width: 4,
            signed: true,
        },
    );
    let selector_cast = cast_selector.register(8);
    cast_selector.instructions.push(SIRInstruction::Unary(
        selector_cast,
        UnaryOp::Ident,
        selector_source,
    ));
    let factor = cast_selector.immediate(64, 3);
    let mut previous = cast_selector.expensive_value(1, factor);
    for key in 8..12 {
        // Keep the comparison itself canonical. Incorrectly stripping the
        // selector cast would still change the four-bit source's meaning.
        let key_reg = cast_selector.immediate(8, key);
        let cond = cast_selector.binary(1, selector_cast, BinaryOp::Eq, key_reg);
        let value = cast_selector.expensive_value(20 + key, factor);
        previous = cast_selector.mux(cond, value, previous);
    }
    let output = cast_selector.ident(previous);
    cast_selector.observe(output);
    let mut cast_selector = cast_selector.finish(vec![selector_source]);
    cast_selector.verify();
    let original = cast_selector.clone();
    let expected = (0..16)
        .map(|value| evaluate(&original, selector_source, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut cast_selector, &PassOptions::default());

    cast_selector.verify();
    assert_ne!(cast_selector.blocks, original.blocks);
    assert!(
        cast_selector.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| matches!(
                instruction,
                SIRInstruction::Unary(dst, UnaryOp::Ident, src)
                    if *dst == selector_cast && *src == selector_source
            ))
    );
    assert_eq!(
        (0..16)
            .map(|value| evaluate(&cast_selector, selector_source, value, output))
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn profitability_rejects_a_cheap_case_chain() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(2);
    let mut previous = builder.immediate(64, 100);
    for key in 0..4 {
        let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
        let value = builder.immediate(64, 10 + key);
        previous = builder.mux(cond, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    let original = eu.clone();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    assert_eq!(eu.blocks, original.blocks);
}

#[test]
fn counts_and_prunes_dead_cross_block_exact_conditions() {
    let (mut eu, selector, output, conditions) = cross_block_case_fixture(71, false);
    eu.verify();
    let expected = (0..128)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }));
    assert!(
        conditions
            .iter()
            .all(|condition| !has_definition(&eu, *condition))
    );
    let actual = (0..128)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn retains_a_cross_block_condition_with_an_external_effect_use() {
    let (mut eu, selector, output, conditions) = cross_block_case_fixture(71, true);
    eu.verify();
    let expected = (0..128)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    let shared = conditions[0];

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(has_definition(&eu, shared));
    assert!(
        conditions[1..]
            .iter()
            .all(|condition| !has_definition(&eu, *condition))
    );
    assert!(eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Store(_, _, _, src, _, _) if *src == shared))
    }));
    let actual = (0..128)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn cross_block_dead_cost_does_not_force_an_unprofitable_dispatch() {
    let (mut eu, selector, output, _) = cross_block_case_fixture(2, false);
    eu.verify();
    let expected = (0..8)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    let original = eu.clone();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert_eq!(eu.blocks, original.blocks);
    assert_eq!(eu.register_map, original.register_map);
    let actual = (0..8)
        .map(|value| evaluate(&eu, selector, value, output))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn does_not_partially_rewrite_a_full_domain_constant_lookup() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(7);
    let guard = builder.register(1);
    let mut previous = builder.immediate(8, 0xee);
    let mut shared_middle = None;
    for key in 0..128 {
        let cond = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
        let value = builder.immediate(8, (key * 3) & 0xff);
        previous = builder.mux(cond, value, previous);
        if key == 63 {
            shared_middle = Some(previous);
        }
    }
    // Embed the dense chain under a non-case outer mux and give one
    // intermediate stage an outside use.  Protecting only the observable
    // root, or stopping the protection walk at shared intermediates,
    // would let an inner sparse prefix destroy the table shape.
    let outer_value = builder.immediate(8, 0xa5);
    let outer = builder.mux(guard, outer_value, previous);
    builder.ident(outer);
    builder.ident(shared_middle.unwrap());
    let mut eu = builder.finish(vec![selector, guard]);
    eu.verify();
    let original = eu.clone();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    assert_eq!(eu.blocks, original.blocks);
    assert_eq!(eu.register_map, original.register_map);
}

#[test]
fn recognizes_two_state_one_bit_predicate_normalization() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(1, factor);
    for key in 0..8 {
        let comparison = builder.exact_condition(selector, key * 2, BinaryOp::EqWildcard);
        let reduced = builder.register(1);
        builder
            .instructions
            .push(SIRInstruction::Unary(reduced, UnaryOp::Or, comparison));
        let condition = builder.register(1);
        builder.instructions.push(SIRInstruction::Unary(
            condition,
            UnaryOp::ToTwoState,
            reduced,
        ));
        let value = builder.expensive_value(20 + key, factor);
        previous = builder.mux(condition, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Switch { .. }
    ));
}

#[test]
fn exhaustive_nonconstant_switch_does_not_create_an_orphan_default_arm() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(100, factor);
    for key in 0..16 {
        let condition = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
        let value = builder.expensive_value(200 + key, factor);
        previous = builder.mux(condition, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    let SIRTerminator::Switch { cases, default, .. } = &eu.blocks[&BlockId(0)].terminator else {
        panic!("full nonconstant case chain should become a switch");
    };
    assert_eq!(cases.len(), 16);
    assert!(cases.iter().any(|case| case.target == *default));
}

#[test]
fn rejects_predicate_casts_through_zero_width() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(1, factor);
    for key in 0..4 {
        let raw = builder.exact_condition(selector, key, BinaryOp::Eq);
        let zero_width = builder.register(0);
        builder
            .instructions
            .push(SIRInstruction::Unary(zero_width, UnaryOp::Ident, raw));
        let cond = builder.register(1);
        builder
            .instructions
            .push(SIRInstruction::Unary(cond, UnaryOp::Ident, zero_width));
        let value = builder.expensive_value(20 + key, factor);
        previous = builder.mux(cond, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    let original = eu.clone();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    assert_eq!(eu.blocks, original.blocks);
    assert_eq!(eu.register_map, original.register_map);
}

#[test]
fn keeps_shared_and_alias_sensitive_arm_definitions_in_the_head() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let shared = builder.expensive_value(9, factor);
    let loaded = builder.register(64);
    builder.instructions.push(SIRInstruction::Load(
        loaded,
        address(0),
        SIROffset::Static(usize::MAX),
        64,
    ));
    let alias_sensitive = builder.binary(64, loaded, BinaryOp::Add, factor);
    let stored = builder.immediate(64, 44);
    builder.instructions.push(SIRInstruction::Store(
        address(0),
        SIROffset::Static(usize::MAX),
        64,
        stored,
        Vec::new(),
        Vec::new(),
    ));
    let mut previous = builder.expensive_value(1, factor);
    for key in 0..8 {
        let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
        let value = match key {
            0 => shared,
            1 => alias_sensitive,
            _ => builder.expensive_value(20 + key, factor),
        };
        previous = builder.mux(cond, value, previous);
    }
    let selected = builder.ident(previous);
    // The second use makes `shared` ineligible for arm sinking.
    builder.binary(64, shared, BinaryOp::Add, selected);
    let mut eu = builder.finish(vec![selector]);

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    let head = &eu.blocks[&BlockId(0)];
    assert!(
        head.instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(shared))
    );
    assert!(
        head.instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(loaded))
    );
    assert!(matches!(head.terminator, SIRTerminator::Switch { .. }));
}

#[test]
fn preserves_arm_definitions_with_uses_in_a_successor_block() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let shared = builder.expensive_value(9, factor);
    let mut previous = builder.expensive_value(1, factor);
    for key in 0..8 {
        let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
        let value = if key == 0 {
            shared
        } else {
            builder.expensive_value(20 + key, factor)
        };
        previous = builder.mux(cond, value, previous);
    }
    let external_result = builder.register(64);
    let mut eu = builder.finish(vec![selector]);
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
        SIRTerminator::Jump(BlockId(1), Vec::new());
    eu.blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Binary(external_result, shared, BinaryOp::Add, previous),
                SIRInstruction::Store(
                    address(usize::MAX),
                    SIROffset::Static(0),
                    64,
                    external_result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        },
    );
    eu.verify();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(shared))
    );
    assert!(
        eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(external_result))
    );
}

#[test]
fn anticipates_stable_storage_aliases_when_sinking_loads() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let address_a = address(10);
    let address_b = address(11);
    let loaded_b = builder.register(64);
    builder.instructions.push(SIRInstruction::Load(
        loaded_b,
        address_b,
        SIROffset::Static(0),
        64,
    ));
    let alias_sensitive = builder.binary(64, loaded_b, BinaryOp::Add, factor);
    let duplicate_value = builder.immediate(64, 44);
    builder.instructions.push(SIRInstruction::Store(
        address_a,
        SIROffset::Static(0),
        64,
        duplicate_value,
        Vec::new(),
        Vec::new(),
    ));

    let mut previous = builder.expensive_value(1, factor);
    for key in 0..8 {
        let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
        let value = if key == 0 {
            alias_sensitive
        } else {
            builder.expensive_value(20 + key, factor)
        };
        previous = builder.mux(cond, value, previous);
    }
    // This store is after the mux root, so exact-address checking alone
    // does not stop the B load from crossing Store(A).  Layout later
    // removes Store(B) if its precomputed B -> A alias is validated.
    builder.instructions.push(SIRInstruction::Store(
        address_b,
        SIROffset::Static(0),
        64,
        duplicate_value,
        Vec::new(),
        Vec::new(),
    ));
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    let aliases = [(address_b.absolute_addr(), address_a.absolute_addr())]
        .into_iter()
        .collect();
    let pass = SparseCaseDispatchPass::new(&aliases);

    pass.run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| def_reg(inst) == Some(loaded_b))
    );
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Switch { .. }
    ));
}

#[test]
fn stable_alias_classes_are_transitive_but_do_not_cross_regions() {
    let stable_a = address(20);
    let stable_b = address(21);
    let stable_c = address(22);
    let aliases = [
        (stable_b.absolute_addr(), stable_a.absolute_addr()),
        (stable_c.absolute_addr(), stable_b.absolute_addr()),
    ]
    .into_iter()
    .collect();
    let pass = SparseCaseDispatchPass::new(&aliases);
    fn access(addr: &RegionedAbsoluteAddr) -> MemAccess<'_> {
        MemAccess {
            addr,
            offset: Some(0),
            width: 1,
        }
    }
    assert!(memory_may_alias(
        access(&stable_a),
        access(&stable_c),
        &pass.stable_alias_class,
    ));

    let mut working_a = stable_a;
    working_a.region = crate::ir::WORKING_REGION;
    let mut working_c = stable_c;
    working_c.region = crate::ir::WORKING_REGION;
    assert!(!memory_may_alias(
        access(&working_a),
        access(&working_c),
        &pass.stable_alias_class,
    ));
}

#[test]
fn leaves_exact_chains_unchanged_in_four_state_mode() {
    let (mut eu, _, _) = expensive_duplicate_fixture();
    let original = eu.clone();
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };

    SparseCaseDispatchPass::default().run(&mut eu, &options);

    assert_eq!(eu.blocks, original.blocks);
}

#[test]
fn id_exhaustion_rejects_the_rewrite_without_panicking_or_mutating() {
    let (mut block_ids, _, _) = expensive_duplicate_fixture();
    let mut block = block_ids.blocks.remove(&BlockId(0)).unwrap();
    block.id = BlockId(usize::MAX);
    block_ids.entry_block_id = block.id;
    block_ids.blocks.insert(block.id, block);
    let original = block_ids.clone();

    SparseCaseDispatchPass::default().run(&mut block_ids, &PassOptions::default());

    assert_eq!(block_ids.blocks, original.blocks);
    assert_eq!(block_ids.register_map, original.register_map);

    let (mut register_ids, _, _) = expensive_duplicate_fixture();
    register_ids.register_map.insert(
        RegisterId(usize::MAX),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let original = register_ids.clone();

    SparseCaseDispatchPass::default().run(&mut register_ids, &PassOptions::default());

    assert_eq!(register_ids.blocks, original.blocks);
    assert_eq!(register_ids.register_map, original.register_map);
}

#[test]
fn enormous_width_profitability_saturates_instead_of_overflowing() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(2);
    let factor = builder.immediate(usize::MAX, 3);
    let make_value = |builder: &mut FixtureBuilder, seed| {
        let mut value = builder.immediate(usize::MAX, seed);
        for _ in 0..100 {
            value = builder.binary(usize::MAX, value, BinaryOp::DivU, factor);
        }
        value
    };
    let mut previous = make_value(&mut builder, 17);
    for key in 0..2 {
        let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
        let value = make_value(&mut builder, 30 + key);
        previous = builder.mux(cond, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(eu.blocks.len() > 1);
}

#[test]
fn rewrites_two_state_case_equality_chains() {
    let mut builder = FixtureBuilder::new();
    let selector = builder.register(4);
    let factor = builder.immediate(64, 3);
    let mut previous = builder.expensive_value(17, factor);
    for key in 0..2 {
        let cond = builder.exact_condition(selector, key, BinaryOp::EqCase);
        let value = builder.expensive_value(30 + key, factor);
        previous = builder.mux(cond, value, previous);
    }
    builder.ident(previous);
    let mut eu = builder.finish(vec![selector]);
    eu.verify();

    SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(eu.blocks.len() > 1);
    assert!(
        eu.blocks.values().all(
            |block| block.instructions.iter().all(|instruction| !matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::EqCase, _)
            ))
        )
    );
}

fn evaluate(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    selector: RegisterId,
    selector_value: u64,
    output: RegisterId,
) -> BigUint {
    let mut values = HashMap::<RegisterId, BigUint>::default();
    values.insert(selector, BigUint::from(selector_value));
    let mut block_id = eu.entry_block_id;

    for _ in 0..10_000 {
        let block = &eu.blocks[&block_id];
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Imm(dst, value) => {
                    assert!(value.mask.is_zero());
                    values.insert(
                        *dst,
                        truncate_to_width(value.payload.clone(), eu.register_map[dst].width()),
                    );
                }
                SIRInstruction::Binary(dst, lhs, op, rhs) => {
                    let lhs = values[lhs].clone();
                    let rhs = values[rhs].clone();
                    let value = match op {
                        BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard => {
                            BigUint::from(u8::from(lhs == rhs))
                        }
                        BinaryOp::LogicOr => {
                            BigUint::from(u8::from(!lhs.is_zero() || !rhs.is_zero()))
                        }
                        BinaryOp::Or => lhs | rhs,
                        BinaryOp::LtU => BigUint::from(u8::from(lhs < rhs)),
                        BinaryOp::Mul => lhs * rhs,
                        BinaryOp::Add => lhs + rhs,
                        _ => panic!("unsupported test operation {op:?}"),
                    };
                    values.insert(*dst, truncate_to_width(value, eu.register_map[dst].width()));
                }
                SIRInstruction::Unary(dst, UnaryOp::Ident, src) => {
                    values.insert(*dst, values[src].clone());
                }
                SIRInstruction::Concat(dst, args) => {
                    let mut value = BigUint::zero();
                    for arg in args {
                        value <<= eu.register_map[arg].width();
                        value |= values[arg].clone();
                    }
                    values.insert(*dst, value);
                }
                SIRInstruction::Slice(dst, src, offset, width) => {
                    let value = values[src].clone() >> offset;
                    values.insert(*dst, truncate_to_width(value, *width));
                }
                SIRInstruction::Mux(dst, cond, true_value, false_value) => {
                    let selected = if values[cond].is_zero() {
                        false_value
                    } else {
                        true_value
                    };
                    values.insert(*dst, values[selected].clone());
                }
                SIRInstruction::Store(_, _, _, src, _, _) => {
                    assert!(values.contains_key(src));
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(target, args) => {
                assign_edge_params(eu, &mut values, *target, args);
                block_id = *target;
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let edge = if values[cond].is_zero() {
                    false_block
                } else {
                    true_block
                };
                assign_edge_params(eu, &mut values, edge.0, &edge.1);
                block_id = edge.0;
            }
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => {
                block_id = cases
                    .iter()
                    .find(|case| case.value == values[selector])
                    .map_or(*default, |case| case.target);
            }
            SIRTerminator::Return => return values[&output].clone(),
            SIRTerminator::Error(code) => panic!("unexpected error {code}"),
        }
    }
    panic!("test evaluator did not terminate")
}

fn assign_edge_params(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    values: &mut HashMap<RegisterId, BigUint>,
    target: BlockId,
    args: &[RegisterId],
) {
    let incoming = args
        .iter()
        .map(|arg| values[arg].clone())
        .collect::<Vec<_>>();
    for (&param, value) in eu.blocks[&target].params.iter().zip(incoming) {
        values.insert(param, value);
    }
}
