use super::*;
use celox_design::StateObjectId as VarId;
use num_traits::ToPrimitive;

const LANES: usize = 8;
const ELEMENT_WIDTH: usize = 12;
const KEYS: [u64; 3] = [0x100, 0x300, 0x180];

fn address(raw: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(raw),
    }
}

fn array_address() -> RegionedAbsoluteAddr {
    address(0)
}

fn output_address() -> RegionedAbsoluteAddr {
    address(1)
}

struct Builder {
    next_register: usize,
    register_map: HashMap<RegisterId, RegisterType>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
}

impl Builder {
    fn new() -> Self {
        Self {
            next_register: 0,
            register_map: HashMap::default(),
            instructions: Vec::new(),
        }
    }

    fn bit(&mut self, width: usize) -> RegisterId {
        let register = RegisterId(self.next_register);
        self.next_register += 1;
        self.register_map.insert(register, unsigned_bit_type(width));
        register
    }

    fn logic(&mut self, width: usize) -> RegisterId {
        let register = RegisterId(self.next_register);
        self.next_register += 1;
        self.register_map
            .insert(register, RegisterType::Logic { width });
        register
    }

    fn imm(&mut self, width: usize, value: u64) -> RegisterId {
        let register = self.bit(width);
        self.instructions
            .push(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.bit(width);
        self.instructions
            .push(SIRInstruction::Binary(register, lhs, op, rhs));
        register
    }

    fn logic_binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.logic(width);
        self.instructions
            .push(SIRInstruction::Binary(register, lhs, op, rhs));
        register
    }
}

struct Fixture {
    eu: ExecutionUnit<RegionedAbsoluteAddr>,
    outer: RegisterId,
    gate: RegisterId,
    flag_a: RegisterId,
    flag_b: RegisterId,
}

fn fixture(reordered_lanes: bool, writes_between: bool, writes_after: bool) -> Fixture {
    let mut builder = Builder::new();
    let outer = builder.bit(LANES);
    let gate = builder.bit(LANES);
    let flag_a = builder.bit(LANES);
    let flag_b = builder.bit(LANES);
    let params = vec![outer, gate, flag_a, flag_b];

    let mut loads = Vec::new();
    for lane in 0..LANES {
        let load = builder.logic(ELEMENT_WIDTH);
        let physical_lane = if reordered_lanes {
            match lane {
                0 => 1,
                1 => 0,
                _ => lane,
            }
        } else {
            lane
        };
        builder.instructions.push(SIRInstruction::Load(
            load,
            array_address(),
            SIROffset::Static(physical_lane * ELEMENT_WIDTH),
            ELEMENT_WIDTH,
        ));
        loads.push(load);
    }
    if writes_between {
        let write_value = builder.imm(ELEMENT_WIDTH, 0x55);
        builder.instructions.push(SIRInstruction::Store(
            array_address(),
            SIROffset::Static(0),
            ELEMENT_WIDTH,
            write_value,
            Vec::new(),
            Vec::new(),
        ));
    }

    let mut packs = Vec::new();
    for key in KEYS {
        let key_register = builder.imm(ELEMENT_WIDTH, key);
        let mut equalities = Vec::new();
        for &load in &loads {
            equalities.push(builder.logic_binary(1, load, BinaryOp::Eq, key_register));
        }
        let pack = builder.bit(LANES);
        builder.instructions.push(SIRInstruction::Concat(
            pack,
            equalities.into_iter().rev().collect(),
        ));
        packs.push(pack);
    }

    let compare_01 = builder.binary(LANES, packs[0], BinaryOp::Or, packs[1]);
    let compare_all = builder.binary(LANES, compare_01, BinaryOp::Or, packs[2]);
    let gated = builder.binary(LANES, gate, BinaryOp::And, compare_all);
    let flags = builder.binary(LANES, flag_a, BinaryOp::Or, flag_b);
    let body = builder.binary(LANES, gated, BinaryOp::Or, flags);
    let active = builder.binary(LANES, outer, BinaryOp::And, body);
    let result = builder.logic(1);
    builder
        .instructions
        .push(SIRInstruction::Unary(result, UnaryOp::Or, active));
    builder.instructions.push(SIRInstruction::Store(
        output_address(),
        SIROffset::Static(0),
        1,
        result,
        Vec::new(),
        Vec::new(),
    ));
    if writes_after {
        let write_value = builder.imm(ELEMENT_WIDTH, 0x55);
        builder.instructions.push(SIRInstruction::Store(
            array_address(),
            SIROffset::Static(0),
            ELEMENT_WIDTH,
            write_value,
            Vec::new(),
            Vec::new(),
        ));
    }

    let block = BasicBlock {
        id: BlockId(0),
        params,
        instructions: builder.instructions,
        terminator: SIRTerminator::Return,
    };
    Fixture {
        eu: ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: builder.register_map,
        },
        outer,
        gate,
        flag_a,
        flag_b,
    }
}

fn pass() -> MaskedArrayAnyPass {
    MaskedArrayAnyPass {
        unpacked_arrays: [(
            array_address().absolute_addr(),
            ArrayShape {
                element_width: ELEMENT_WIDTH,
                element_count: LANES,
            },
        )]
        .into_iter()
        .collect(),
    }
}

fn truncate(value: u64, width: usize) -> u64 {
    if width >= 64 {
        value
    } else {
        value & ((1u64 << width) - 1)
    }
}

fn execute(
    fixture: &Fixture,
    outer: u64,
    gate: u64,
    flag_a: u64,
    flag_b: u64,
    lanes: [u64; LANES],
) -> (u64, usize) {
    let eu = &fixture.eu;
    let mut registers = HashMap::default();
    registers.insert(fixture.outer, outer);
    registers.insert(fixture.gate, gate);
    registers.insert(fixture.flag_a, flag_a);
    registers.insert(fixture.flag_b, flag_b);
    let mut memory = HashMap::default();
    for (lane, value) in lanes.into_iter().enumerate() {
        memory.insert(
            (array_address(), lane * ELEMENT_WIDTH),
            truncate(value, ELEMENT_WIDTH),
        );
    }
    let mut current = eu.entry_block_id;
    let mut array_loads = 0usize;
    for _ in 0..1024 {
        let block = &eu.blocks[&current];
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(destination, value) => {
                    let value = value.payload.to_u64().unwrap_or(0);
                    registers.insert(
                        *destination,
                        truncate(value, eu.register_map[destination].width()),
                    );
                }
                SIRInstruction::Binary(destination, lhs, op, rhs) => {
                    let lhs = registers[lhs];
                    let rhs = registers[rhs];
                    let value = match op {
                        BinaryOp::And => lhs & rhs,
                        BinaryOp::Or => lhs | rhs,
                        BinaryOp::Sub => lhs.wrapping_sub(rhs),
                        BinaryOp::Eq => u64::from(lhs == rhs),
                        other => panic!("unsupported test binary operation {other:?}"),
                    };
                    registers.insert(
                        *destination,
                        truncate(value, eu.register_map[destination].width()),
                    );
                }
                SIRInstruction::Unary(destination, op, source) => {
                    let source = registers[source];
                    let value = match op {
                        UnaryOp::Or => u64::from(source != 0),
                        UnaryOp::CountTrailingZeros => source.trailing_zeros() as u64,
                        other => panic!("unsupported test unary operation {other:?}"),
                    };
                    registers.insert(
                        *destination,
                        truncate(value, eu.register_map[destination].width()),
                    );
                }
                SIRInstruction::Load(destination, address, offset, _) => {
                    if *address == array_address() {
                        array_loads += 1;
                    }
                    let offset = match offset {
                        SIROffset::Static(offset)
                        | SIROffset::PackedElements {
                            bit_offset: offset, ..
                        } => *offset,
                        SIROffset::Dynamic(register) => registers[register] as usize,
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset,
                        } => {
                            registers[index] as usize * element_width
                                + bit_offset
                                + dynamic_bit_offset
                                    .map(|register| registers[&register] as usize)
                                    .unwrap_or(0)
                        }
                    };
                    registers.insert(
                        *destination,
                        memory.get(&(*address, offset)).copied().unwrap_or(0),
                    );
                }
                SIRInstruction::Store(address, SIROffset::Static(offset), _, source, _, _) => {
                    memory.insert((*address, *offset), registers[source]);
                }
                SIRInstruction::Concat(destination, parts) => {
                    let mut value = 0u64;
                    for part in parts {
                        let width = eu.register_map[part].width();
                        value = (value << width) | registers[part];
                    }
                    registers.insert(
                        *destination,
                        truncate(value, eu.register_map[destination].width()),
                    );
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }

        let (target, arguments) = match &block.terminator {
            SIRTerminator::Jump(target, arguments) => (*target, arguments.clone()),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if registers[cond] != 0 {
                    (true_block.0, true_block.1.clone())
                } else {
                    (false_block.0, false_block.1.clone())
                }
            }
            SIRTerminator::Switch { .. } => {
                panic!("unexpected Switch in masked-array test")
            }
            SIRTerminator::Return => {
                return (
                    memory.get(&(output_address(), 0)).copied().unwrap_or(0),
                    array_loads,
                );
            }
            SIRTerminator::Error(code) => panic!("unexpected test error {code}"),
        };
        let values = arguments
            .iter()
            .map(|register| registers[register])
            .collect::<Vec<_>>();
        assert_eq!(eu.blocks[&target].params.len(), values.len());
        for (&parameter, value) in eu.blocks[&target].params.iter().zip(values) {
            registers.insert(parameter, value);
        }
        current = target;
    }
    panic!("test SIR did not terminate");
}

fn assert_unchanged(
    before: &ExecutionUnit<RegionedAbsoluteAddr>,
    after: &ExecutionUnit<RegionedAbsoluteAddr>,
) {
    assert_eq!(after.entry_block_id, before.entry_block_id);
    assert_eq!(after.register_map, before.register_map);
    assert_eq!(after.blocks, before.blocks);
}

#[test]
fn recovers_active_lane_search_and_preserves_results() {
    let mut fixture = fixture(false, false, false);
    fixture.eu.verify_result().unwrap();
    let before = Fixture {
        eu: fixture.eu.clone(),
        outer: fixture.outer,
        gate: fixture.gate,
        flag_a: fixture.flag_a,
        flag_b: fixture.flag_b,
    };

    pass().run(&mut fixture.eu, &PassOptions::default());
    fixture.eu.verify_result().unwrap();

    let static_array_loads = fixture
        .eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(_, address, SIROffset::Static(_), _)
                    if *address == array_address()
            )
        })
        .count();
    let dynamic_array_loads = fixture
        .eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(_, address, SIROffset::Element { .. }, _)
                    if *address == array_address()
            )
        })
        .count();
    assert_eq!(static_array_loads, 0);
    assert_eq!(dynamic_array_loads, 1);

    let cases = [
        (0b0000_0100, 0, 0b0000_0100, 0, [0; LANES], true),
        (0, 0, 0b0000_0100, 0, [0; LANES], false),
        (
            0b0000_0001,
            0b0000_0001,
            0,
            0,
            [KEYS[0], 0, 0, 0, 0, 0, 0, 0],
            true,
        ),
        (
            0b0001_0000,
            0b0001_0000,
            0,
            0,
            [0, 0, 0, 0, KEYS[1], 0, 0, 0],
            true,
        ),
        (
            0b1000_0000,
            0b1000_0000,
            0,
            0,
            [0, 0, 0, 0, 0, 0, 0, KEYS[2]],
            true,
        ),
        (
            0b1010_0101,
            0b1010_0101,
            0,
            0,
            [1, 2, 3, 4, 5, 6, 7, 8],
            false,
        ),
        (
            0b1010_0101,
            0b1010_0101,
            0,
            0,
            [1, 2, KEYS[2], 4, 5, 6, 7, 8],
            true,
        ),
        (
            0b0000_0001,
            0b0000_0010,
            0,
            0,
            [0, KEYS[0], 0, 0, 0, 0, 0, 0],
            false,
        ),
    ];
    for (outer, gate, flag_a, flag_b, lanes, expected) in cases {
        let before_result = execute(&before, outer, gate, flag_a, flag_b, lanes);
        let after_result = execute(&fixture, outer, gate, flag_a, flag_b, lanes);
        assert_eq!(before_result.0, u64::from(expected));
        assert_eq!(after_result.0, before_result.0);
    }

    let (_, flag_loads) = execute(&fixture, 0b0000_0100, 0, 0b0000_0100, 0, [0; LANES]);
    let (_, empty_loads) = execute(&fixture, 0, u64::MAX, 0, 0, [KEYS[0]; LANES]);
    let (_, two_candidate_loads) = execute(
        &fixture,
        0b0000_0101,
        0b0000_0101,
        0,
        0,
        [1, 0, KEYS[1], 0, 0, 0, 0, 0],
    );
    assert_eq!(flag_loads, 0);
    assert_eq!(empty_loads, 0);
    assert_eq!(two_candidate_loads, 2);
}

#[test]
fn leaves_four_state_mode_unchanged() {
    let mut fixture = fixture(false, false, false);
    let before = fixture.eu.clone();
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };
    pass().run(&mut fixture.eu, &options);
    assert_unchanged(&before, &fixture.eu);
}

#[test]
fn requires_declared_unpacked_array_shape() {
    let mut fixture = fixture(false, false, false);
    let before = fixture.eu.clone();
    MaskedArrayAnyPass::default().run(&mut fixture.eu, &PassOptions::default());
    assert_unchanged(&before, &fixture.eu);
}

#[test]
fn rejects_reordered_lane_mapping() {
    let mut fixture = fixture(true, false, false);
    let before = fixture.eu.clone();
    pass().run(&mut fixture.eu, &PassOptions::default());
    assert_unchanged(&before, &fixture.eu);
}

#[test]
fn preserves_load_memory_version_when_array_is_written() {
    let mut fixture = fixture(false, true, false);
    let before = fixture.eu.clone();
    pass().run(&mut fixture.eu, &PassOptions::default());
    assert_unchanged(&before, &fixture.eu);
}

#[test]
fn write_after_reduction_does_not_constrain_load_placement() {
    let mut fixture = fixture(false, false, true);
    fixture.eu.verify_result().unwrap();
    pass().run(&mut fixture.eu, &PassOptions::default());
    fixture.eu.verify_result().unwrap();
    assert!(fixture.eu.blocks.len() > 1);
    assert!(fixture.eu.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(_, address, SIROffset::Element { .. }, _)
                    if *address == array_address()
            )
        })
    }));
}
