use super::*;
use crate::ir::{InstanceId, STABLE_REGION};
use celox_design::StateObjectId as VarId;

const LANE_WIDTH: usize = 6;
const LANE_COUNT: usize = 31;
const PACKED_WIDTH: usize = 192;

fn address_with_var(raw: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(raw),
    }
}

fn address() -> RegionedAbsoluteAddr {
    address_with_var(0)
}

struct FixtureBuilder {
    next: usize,
    registers: HashMap<RegisterId, RegisterType>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
}

impl FixtureBuilder {
    fn new() -> Self {
        Self {
            next: 0,
            registers: HashMap::default(),
            instructions: Vec::new(),
        }
    }

    fn register(&mut self, ty: RegisterType) -> RegisterId {
        let result = RegisterId(self.next);
        self.next += 1;
        self.registers.insert(result, ty);
        result
    }

    fn logic(&mut self, width: usize) -> RegisterId {
        self.register(RegisterType::Logic { width })
    }

    fn bit(&mut self, width: usize) -> RegisterId {
        self.register(RegisterType::Bit {
            width,
            signed: false,
        })
    }

    fn imm_logic(&mut self, width: usize, value: u64) -> RegisterId {
        let result = self.logic(width);
        self.instructions
            .push(SIRInstruction::Imm(result, SIRValue::new(value)));
        result
    }

    fn imm_bit(&mut self, width: usize, value: u64) -> RegisterId {
        let result = self.bit(width);
        self.instructions
            .push(SIRInstruction::Imm(result, SIRValue::new(value)));
        result
    }

    fn base_lane(&mut self, base: RegisterId, offset: usize) -> RegisterId {
        // Use the same constant-Shr/low-mask shape present in Heliodor,
        // and create distinct equivalent chains for the two Mux arms.
        let amount = self.imm_bit(64, offset as u64);
        let shifted = self.logic(PACKED_WIDTH);
        self.instructions
            .push(SIRInstruction::Binary(shifted, base, BinaryOp::Shr, amount));
        let mask = self.imm_logic(LANE_WIDTH, 0x3f);
        let lane = self.logic(LANE_WIDTH);
        self.instructions
            .push(SIRInstruction::Binary(lane, shifted, BinaryOp::And, mask));
        lane
    }
}

struct Fixture {
    eu: ExecutionUnit<RegionedAbsoluteAddr>,
    base: RegisterId,
    selector: RegisterId,
    value: RegisterId,
    gate: RegisterId,
}

#[allow(clippy::too_many_arguments)]
fn append_ladder(
    builder: &mut FixtureBuilder,
    base: RegisterId,
    selector: RegisterId,
    value: RegisterId,
    gate: RegisterId,
    keys: &[u64],
    capture_sites: Vec<u32>,
    extra_concat_use: bool,
) -> RegisterId {
    let mut lanes_lsb = Vec::new();
    for (lane, &key_value) in keys.iter().enumerate() {
        let inner_base = builder.base_lane(base, (lane + 1) * LANE_WIDTH);
        let outer_base = builder.base_lane(base, (lane + 1) * LANE_WIDTH);
        let key = builder.imm_logic(5, key_value);
        let equality = builder.logic(1);
        builder.instructions.push(SIRInstruction::Binary(
            equality,
            selector,
            BinaryOp::Eq,
            key,
        ));
        let truth = builder.logic(1);
        builder
            .instructions
            .push(SIRInstruction::Unary(truth, UnaryOp::Or, equality));
        let condition = builder.bit(1);
        builder
            .instructions
            .push(SIRInstruction::Unary(condition, UnaryOp::ToTwoState, truth));
        let inner = builder.logic(LANE_WIDTH);
        builder
            .instructions
            .push(SIRInstruction::Mux(inner, condition, value, inner_base));
        let outer = builder.logic(LANE_WIDTH);
        builder
            .instructions
            .push(SIRInstruction::Mux(outer, gate, inner, outer_base));
        lanes_lsb.push(outer);
    }
    let packed = builder.logic(LANE_COUNT * LANE_WIDTH);
    builder.instructions.push(SIRInstruction::Concat(
        packed,
        lanes_lsb.into_iter().rev().collect(),
    ));
    if extra_concat_use {
        builder.instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![packed],
        });
    }
    builder.instructions.push(SIRInstruction::Store(
        address(),
        SIROffset::Static(LANE_WIDTH),
        LANE_COUNT * LANE_WIDTH,
        packed,
        Vec::new(),
        capture_sites,
    ));
    packed
}

fn fixture(keys: &[u64], capture_sites: Vec<u32>, extra_concat_use: bool) -> Fixture {
    assert_eq!(keys.len(), LANE_COUNT);
    let mut builder = FixtureBuilder::new();
    let base = builder.logic(PACKED_WIDTH);
    let selector = builder.logic(5);
    let value = builder.logic(LANE_WIDTH);
    let gate = builder.bit(1);
    let _ = append_ladder(
        &mut builder,
        base,
        selector,
        value,
        gate,
        keys,
        capture_sites,
        extra_concat_use,
    );
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![base, selector, value, gate],
        instructions: builder.instructions,
        terminator: SIRTerminator::Return,
    };
    Fixture {
        eu: ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: builder.registers,
        },
        base,
        selector,
        value,
        gate,
    }
}

fn append_store_sink(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, address: RegionedAbsoluteAddr) {
    let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
    let store_index = block
        .instructions
        .iter()
        .rposition(|instruction| matches!(instruction, SIRInstruction::Store(..)))
        .unwrap();
    let mut store = block.instructions[store_index].clone();
    let SIRInstruction::Store(candidate, ..) = &mut store else {
        unreachable!();
    };
    *candidate = address;
    block.instructions.insert(store_index + 1, store);
}

fn canonical_keys() -> Vec<u64> {
    (1..=LANE_COUNT as u64).collect()
}

fn execute(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    inputs: &HashMap<RegisterId, BigUint>,
    initial_memory: BigUint,
) -> BigUint {
    execute_memories(
        eu,
        inputs,
        [(address(), initial_memory)].into_iter().collect(),
    )
    .remove(&address())
    .unwrap()
}

fn execute_memories(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    inputs: &HashMap<RegisterId, BigUint>,
    mut memories: HashMap<RegionedAbsoluteAddr, BigUint>,
) -> HashMap<RegionedAbsoluteAddr, BigUint> {
    let mut registers = inputs.clone();
    for memory in memories.values_mut() {
        *memory = truncate(memory.clone(), PACKED_WIDTH);
    }
    let mut block = eu.entry_block_id;
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps <= eu.blocks.len() + 1, "test SIR did not terminate");
        let current = &eu.blocks[&block];
        for instruction in &current.instructions {
            match instruction {
                SIRInstruction::Imm(destination, value) => {
                    assert!(value.mask.is_zero());
                    let width = eu.register_map[destination].width();
                    registers.insert(*destination, truncate(value.payload.clone(), width));
                }
                SIRInstruction::Binary(destination, lhs, op, rhs) => {
                    let lhs = registers[lhs].clone();
                    let rhs = registers[rhs].clone();
                    let width = eu.register_map[destination].width();
                    let result = match op {
                        BinaryOp::Add => lhs + rhs,
                        BinaryOp::Sub => {
                            let modulus = BigUint::one() << width;
                            (lhs + &modulus - rhs) % modulus
                        }
                        BinaryOp::Mul => lhs * rhs,
                        BinaryOp::And => lhs & rhs,
                        BinaryOp::Or => lhs | rhs,
                        BinaryOp::Xor => lhs ^ rhs,
                        BinaryOp::Shl => lhs << rhs.to_usize().unwrap(),
                        BinaryOp::Shr => lhs >> rhs.to_usize().unwrap(),
                        BinaryOp::Eq => BigUint::from((lhs == rhs) as u8),
                        BinaryOp::Ne => BigUint::from((lhs != rhs) as u8),
                        BinaryOp::LeU => BigUint::from((lhs <= rhs) as u8),
                        BinaryOp::GeU => BigUint::from((lhs >= rhs) as u8),
                        BinaryOp::LogicAnd => {
                            BigUint::from(((!lhs.is_zero()) && (!rhs.is_zero())) as u8)
                        }
                        other => panic!("unsupported test binary operation {other:?}"),
                    };
                    registers.insert(*destination, truncate(result, width));
                }
                SIRInstruction::Unary(destination, op, source) => {
                    let source = registers[source].clone();
                    let width = eu.register_map[destination].width();
                    let result = match op {
                        UnaryOp::Ident | UnaryOp::ToTwoState => source,
                        UnaryOp::Or => BigUint::from((!source.is_zero()) as u8),
                        other => panic!("unsupported test unary operation {other:?}"),
                    };
                    registers.insert(*destination, truncate(result, width));
                }
                SIRInstruction::Slice(destination, source, offset, width) => {
                    registers.insert(
                        *destination,
                        truncate(registers[source].clone() >> offset, *width),
                    );
                }
                SIRInstruction::Concat(destination, parts) => {
                    let mut result = BigUint::zero();
                    for part in parts {
                        let width = eu.register_map[part].width();
                        result = (result << width) | registers[part].clone();
                    }
                    registers.insert(*destination, result);
                }
                SIRInstruction::Mux(destination, condition, then_value, else_value) => {
                    let selected = if registers[condition].is_zero() {
                        registers[else_value].clone()
                    } else {
                        registers[then_value].clone()
                    };
                    registers.insert(*destination, selected);
                }
                SIRInstruction::Store(address, offset, width, source, triggers, captures) => {
                    assert!(triggers.is_empty() && captures.is_empty());
                    let offset = match offset {
                        SIROffset::Static(offset)
                        | SIROffset::PackedElements {
                            bit_offset: offset, ..
                        } => *offset,
                        SIROffset::Dynamic(offset) => registers[offset].to_usize().unwrap(),
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset,
                        } => {
                            registers[index].to_usize().unwrap() * element_width
                                + bit_offset
                                + dynamic_bit_offset
                                    .map(|register| registers[&register].to_usize().unwrap())
                                    .unwrap_or(0)
                        }
                    };
                    let memory = memories.entry(*address).or_insert_with(BigUint::zero);
                    let field_mask = low_mask(*width) << offset;
                    let preserved = low_mask(PACKED_WIDTH) ^ &field_mask;
                    *memory = (&*memory & preserved)
                        | ((registers[source].clone() & low_mask(*width)) << offset);
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }
        match &current.terminator {
            SIRTerminator::Return => {
                for memory in memories.values_mut() {
                    *memory = truncate(memory.clone(), PACKED_WIDTH);
                }
                return memories;
            }
            SIRTerminator::Jump(next, args) => {
                assert!(args.is_empty());
                block = *next;
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let target = if registers[cond].is_zero() {
                    false_block
                } else {
                    true_block
                };
                assert!(target.1.is_empty());
                block = target.0;
            }
            SIRTerminator::Switch { .. } => {
                panic!("unexpected Switch in packed-scatter test")
            }
            SIRTerminator::Error(code) => panic!("unexpected Error({code})"),
        }
    }
}

fn packed_pattern(seed: u64) -> BigUint {
    (0..32usize).fold(BigUint::zero(), |result, lane| {
        let value = (seed
            .wrapping_mul(17)
            .wrapping_add((lane as u64).wrapping_mul(29)))
            & 0x3f;
        result | (BigUint::from(value) << (lane * LANE_WIDTH))
    })
}

#[test]
fn byte_aligned_dynamic_lane_still_uses_worst_case_backend_footprint() {
    let dynamic_bytes = native_access_bytes(8 + 7).unwrap();
    assert_eq!(dynamic_bytes, 2);
    assert!(dynamic_lane_access_is_contained(0, 8, 2, dynamic_bytes, 4));
    assert!(!dynamic_lane_access_is_contained(0, 8, 3, dynamic_bytes, 4));
}

#[test]
fn rewrites_32x6_ladder_and_preserves_every_selector_and_enable() {
    let fixture = fixture(&canonical_keys(), Vec::new(), false);
    fixture.eu.verify();
    let original = fixture.eu.clone();
    let original_definitions = original
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(def_reg)
        .collect::<HashSet<_>>();
    let mut optimized = fixture.eu;
    PackedScatterStorePass::default().run(&mut optimized, &PassOptions::default());
    optimized.verify();

    let stores = optimized
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
        .count();
    let dynamic_stores = optimized
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Store(_, SIROffset::Dynamic(_), LANE_WIDTH, _, _, _)
            )
        })
        .count();
    assert_eq!(stores, 3);
    assert_eq!(dynamic_stores, 1);
    assert!(optimized.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Store(
                    _,
                    SIROffset::Static(186),
                    LANE_WIDTH,
                    _,
                    triggers,
                    captures,
                ) if triggers.is_empty() && captures.is_empty()
            )
        })
    }));
    assert!(optimized.blocks.values().all(|block| {
        block.instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Mux(..) | SIRInstruction::Concat(..)
            )
        })
    }));
    assert!(
        original_definitions
            .iter()
            .all(|register| !optimized.register_map.contains_key(register)),
        "dead ladder register types must not survive DCE"
    );

    for seed in 0..4u64 {
        let base = packed_pattern(seed + 1);
        let initial = packed_pattern(seed + 11);
        for enable in 0..=1u8 {
            for selector in 0..32u64 {
                let value = (seed.wrapping_mul(13).wrapping_add(selector * 7)) & 0x3f;
                let inputs = [
                    (fixture.base, base.clone()),
                    (fixture.selector, BigUint::from(selector)),
                    (fixture.value, BigUint::from(value)),
                    (fixture.gate, BigUint::from(enable)),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>();
                let before = execute(&original, &inputs, initial.clone());
                let after = execute(&optimized, &inputs, initial.clone());
                assert_eq!(
                    after, before,
                    "seed={seed} enable={enable} selector={selector}"
                );

                let mut expected = (&initial & low_mask(LANE_WIDTH))
                    | (&base & (low_mask(LANE_COUNT * LANE_WIDTH) << LANE_WIDTH));
                if enable != 0 && selector != 0 {
                    let offset = selector as usize * LANE_WIDTH;
                    let field = low_mask(LANE_WIDTH) << offset;
                    expected = (&expected & (low_mask(PACKED_WIDTH) ^ &field))
                        | (BigUint::from(value) << offset);
                }
                assert_eq!(after, expected);
            }
        }
    }

    let once = format!("{}", optimized);
    PackedScatterStorePass::default().run(&mut optimized, &PassOptions::default());
    assert_eq!(
        format!("{}", optimized),
        once,
        "the pass must be idempotent"
    );
}

#[test]
fn unpacked_array_rewrite_preserves_element_selection() {
    let fixture = fixture(&canonical_keys(), Vec::new(), false);
    let original = fixture.eu.clone();
    let mut optimized = fixture.eu;
    let mut pass = PackedScatterStorePass::default();
    pass.unpacked_element_widths
        .insert(address().absolute_addr(), LANE_WIDTH);
    pass.run(&mut optimized, &PassOptions::default());
    optimized.verify();

    assert!(optimized.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Store(
                    _,
                    SIROffset::Element {
                        element_width: LANE_WIDTH,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                        ..
                    },
                    LANE_WIDTH,
                    _,
                    _,
                    _,
                )
            )
        })
    }));
    assert!(optimized.blocks.values().all(|block| {
        block.instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Store(_, SIROffset::Dynamic(_), ..)
            )
        })
    }));

    for selector in 0..32u64 {
        let inputs = [
            (fixture.base, packed_pattern(1)),
            (fixture.selector, BigUint::from(selector)),
            (fixture.value, BigUint::from(0x2a_u8)),
            (fixture.gate, BigUint::one()),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        assert_eq!(
            execute(&optimized, &inputs, packed_pattern(11)),
            execute(&original, &inputs, packed_pattern(11)),
            "selector={selector}"
        );
    }
}

#[test]
fn unpacked_array_rewrite_rejects_packed_lane_width() {
    let mut fixture = fixture(&canonical_keys(), Vec::new(), false);
    let before = format!("{}", fixture.eu);
    let mut pass = PackedScatterStorePass::default();
    pass.unpacked_element_widths
        .insert(address().absolute_addr(), LANE_WIDTH * 2);
    pass.run(&mut fixture.eu, &PassOptions::default());
    assert_eq!(format!("{}", fixture.eu), before);
}

#[test]
fn rewrites_two_store_sinks_in_order_for_distinct_and_duplicate_addresses() {
    let first = address();
    let second = address_with_var(1);
    for addresses in [[first, second], [first, first]] {
        let mut fixture = fixture(&canonical_keys(), Vec::new(), false);
        append_store_sink(&mut fixture.eu, addresses[1]);
        fixture.eu.verify();
        let original = fixture.eu.clone();
        let mut optimized = fixture.eu.clone();
        PackedScatterStorePass::default().run(&mut optimized, &PassOptions::default());
        optimized.verify();

        let store_groups = optimized
            .blocks
            .values()
            .filter_map(|block| {
                let stores = block
                    .instructions
                    .iter()
                    .filter_map(|instruction| match instruction {
                        SIRInstruction::Store(address, ..) => Some(*address),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                (!stores.is_empty()).then_some(stores)
            })
            .collect::<Vec<_>>();
        assert_eq!(store_groups.len(), 3);
        assert!(store_groups.iter().all(|stores| stores == &addresses));

        for seed in 0..2u64 {
            let base = packed_pattern(seed + 1);
            let mut initial = HashMap::default();
            initial.insert(first, packed_pattern(seed + 11));
            if addresses[1] != first {
                initial.insert(addresses[1], packed_pattern(seed + 21));
            }
            for enable in 0..=1u8 {
                for selector in 0..32u64 {
                    let value = (seed.wrapping_mul(13).wrapping_add(selector * 7)) & 0x3f;
                    let inputs = [
                        (fixture.base, base.clone()),
                        (fixture.selector, BigUint::from(selector)),
                        (fixture.value, BigUint::from(value)),
                        (fixture.gate, BigUint::from(enable)),
                    ]
                    .into_iter()
                    .collect::<HashMap<_, _>>();
                    assert_eq!(
                        execute_memories(&optimized, &inputs, initial.clone()),
                        execute_memories(&original, &inputs, initial.clone()),
                        "addresses={addresses:?} seed={seed} enable={enable} selector={selector}"
                    );
                }
            }
        }

        let once = format!("{optimized}");
        PackedScatterStorePass::default().run(&mut optimized, &PassOptions::default());
        assert_eq!(format!("{optimized}"), once);
    }
}

#[test]
fn rejects_non_contiguous_keys_extra_sink_and_capture_store() {
    let mut keys = canonical_keys();
    keys[9] = keys[10];
    let mut triggered = fixture(&canonical_keys(), Vec::new(), false);
    let store = triggered
        .eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .iter_mut()
        .find(|instruction| matches!(instruction, SIRInstruction::Store(..)))
        .unwrap();
    let SIRInstruction::Store(_, _, _, _, triggers, _) = store else {
        unreachable!();
    };
    triggers.push(TriggerIdWithKind {
        kind: DomainKind::ClockPosedge,
        id: 0,
    });

    for mut fixture in [
        fixture(&keys, Vec::new(), false),
        fixture(&canonical_keys(), Vec::new(), true),
        fixture(&canonical_keys(), vec![7], false),
        triggered,
    ] {
        let before = format!("{}", fixture.eu);
        PackedScatterStorePass::default().run(&mut fixture.eu, &PassOptions::default());
        assert_eq!(format!("{}", fixture.eu), before);
        fixture.eu.verify();
    }
}

#[test]
fn rejects_non_contiguous_or_heterogeneous_store_sink_groups_atomically() {
    let mut interrupted = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut interrupted.eu, address_with_var(1));
    let marker = RegisterId(
        interrupted
            .eu
            .register_map
            .keys()
            .map(|register| register.0)
            .max()
            .unwrap()
            + 1,
    );
    interrupted.eu.register_map.insert(
        marker,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let block = interrupted.eu.blocks.get_mut(&BlockId(0)).unwrap();
    let first_store = block
        .instructions
        .iter()
        .position(|instruction| matches!(instruction, SIRInstruction::Store(..)))
        .unwrap();
    block.instructions.insert(
        first_store + 1,
        SIRInstruction::Imm(marker, SIRValue::new(0u8)),
    );

    let mut different_offset = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut different_offset.eu, address_with_var(1));
    let SIRInstruction::Store(_, offset, ..) = different_offset
        .eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .last_mut()
        .unwrap()
    else {
        unreachable!();
    };
    *offset = SIROffset::Static(LANE_WIDTH + 1);

    let mut different_width = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut different_width.eu, address_with_var(1));
    let SIRInstruction::Store(_, _, width, ..) = different_width
        .eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .last_mut()
        .unwrap()
    else {
        unreachable!();
    };
    *width -= 1;

    for mut fixture in [interrupted, different_offset, different_width] {
        fixture.eu.verify();
        let before = format!("{}", fixture.eu);
        PackedScatterStorePass::default().run(&mut fixture.eu, &PassOptions::default());
        assert_eq!(format!("{}", fixture.eu), before);
        fixture.eu.verify();
    }
}

#[test]
fn leaves_four_state_selector_ladder_unchanged() {
    let mut fixture = fixture(&canonical_keys(), Vec::new(), false);
    let before = format!("{}", fixture.eu);
    let options = PassOptions {
        four_state: true,
        ..Default::default()
    };
    PackedScatterStorePass::default().run(&mut fixture.eu, &options);
    assert_eq!(format!("{}", fixture.eu), before);
    fixture.eu.verify();
}

#[test]
fn repeatedly_discovers_independent_ladders_and_terminates() {
    let mut builder = FixtureBuilder::new();
    let base = builder.logic(PACKED_WIDTH);
    let selector = builder.logic(5);
    let value = builder.logic(LANE_WIDTH);
    let gate = builder.bit(1);
    let keys = canonical_keys();
    let initially_not_sole = append_ladder(
        &mut builder,
        base,
        selector,
        value,
        gate,
        &keys,
        Vec::new(),
        false,
    );
    // This dead value gives the first Concat a second use. It becomes a
    // sole Store source only after the other ladder is rewritten and DCE
    // runs, forcing discovery to rescan after the sweep.
    let dead_use = builder.logic(1);
    builder
        .instructions
        .push(SIRInstruction::Slice(dead_use, initially_not_sole, 0, 1));
    let _ = append_ladder(
        &mut builder,
        base,
        selector,
        value,
        gate,
        &keys,
        Vec::new(),
        false,
    );
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![base, selector, value, gate],
        instructions: builder.instructions,
        terminator: SIRTerminator::Return,
    };
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: builder.registers,
    };
    eu.verify();
    PackedScatterStorePass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    let (static_stores, dynamic_stores) = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(_, SIROffset::Static(_), ..) => Some((1usize, 0usize)),
            SIRInstruction::Store(_, SIROffset::Dynamic(_), ..) => Some((0, 1)),
            _ => None,
        })
        .fold((0usize, 0usize), |(sa, da), (sb, db)| (sa + sb, da + db));
    assert_eq!((static_stores, dynamic_stores), (4, 2));
    assert!(eu.blocks.values().all(|block| {
        block.instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Mux(..) | SIRInstruction::Concat(..)
            )
        })
    }));
    let once = format!("{}", eu);
    PackedScatterStorePass::default().run(&mut eu, &PassOptions::default());
    assert_eq!(format!("{}", eu), once);
}

#[test]
fn identifier_overflow_is_non_destructive() {
    let mut block_overflow = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut block_overflow.eu, address_with_var(1));
    let mut block = block_overflow.eu.blocks.remove(&BlockId(0)).unwrap();
    block.id = BlockId(usize::MAX);
    block_overflow.eu.entry_block_id = block.id;
    block_overflow.eu.blocks.insert(block.id, block);
    block_overflow.eu.verify();

    let mut native_block_overflow = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut native_block_overflow.eu, address_with_var(1));
    let mut block = native_block_overflow.eu.blocks.remove(&BlockId(0)).unwrap();
    block.id = BlockId(u32::MAX as usize);
    native_block_overflow.eu.entry_block_id = block.id;
    native_block_overflow.eu.blocks.insert(block.id, block);
    native_block_overflow.eu.verify();

    let mut register_overflow = fixture(&canonical_keys(), Vec::new(), false);
    append_store_sink(&mut register_overflow.eu, address_with_var(1));
    let overflow = RegisterId(usize::MAX);
    register_overflow.eu.register_map.insert(
        overflow,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    register_overflow
        .eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .params
        .push(overflow);
    register_overflow.eu.verify();

    for mut fixture in [block_overflow, native_block_overflow, register_overflow] {
        let before = format!("{}", fixture.eu);
        PackedScatterStorePass::default().run(&mut fixture.eu, &PassOptions::default());
        assert_eq!(format!("{}", fixture.eu), before);
        fixture.eu.verify();
    }
}

#[test]
fn slice_proof_rejects_a_mask_that_clears_bits_before_a_later_shift() {
    let mut builder = FixtureBuilder::new();
    let base = builder.logic(PACKED_WIDTH);
    let narrow_mask = builder.imm_logic(PACKED_WIDTH, 0x3f);
    let masked = builder.logic(PACKED_WIDTH);
    builder.instructions.push(SIRInstruction::Binary(
        masked,
        base,
        BinaryOp::And,
        narrow_mask,
    ));
    let six = builder.imm_bit(64, 6);
    let shifted = builder.logic(PACKED_WIDTH);
    builder
        .instructions
        .push(SIRInstruction::Binary(shifted, masked, BinaryOp::Shr, six));
    let lane_mask = builder.imm_logic(LANE_WIDTH, 0x3f);
    let lane = builder.logic(LANE_WIDTH);
    builder.instructions.push(SIRInstruction::Binary(
        lane,
        shifted,
        BinaryOp::And,
        lane_mask,
    ));
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![base],
        instructions: builder.instructions,
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: builder.registers,
    };
    eu.verify();
    let defs = definition_sites(&eu);
    assert_eq!(resolve_low_slice(&eu, &defs, lane, LANE_WIDTH), None);
}
