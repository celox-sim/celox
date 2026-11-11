use super::*;
use celox_design::StateObjectId as VarId;
use num_traits::ToPrimitive;

const LANES: usize = 4;

fn address(raw: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(raw),
    }
}

fn test_pass() -> CircularPriorityPass {
    CircularPriorityPass {
        bit_array_elements: (0..3)
            .map(|raw| (address(raw).absolute_addr(), LANES))
            .collect(),
        array_shapes: HashMap::default(),
    }
}

struct Builder {
    next: usize,
    types: HashMap<RegisterId, RegisterType>,
}

impl Builder {
    fn new() -> Self {
        Self {
            next: 0,
            types: HashMap::default(),
        }
    }

    fn bit(&mut self, width: usize) -> RegisterId {
        let register = RegisterId(self.next);
        self.next += 1;
        self.types.insert(register, unsigned_type(width));
        register
    }

    fn logic(&mut self, width: usize) -> RegisterId {
        let register = RegisterId(self.next);
        self.next += 1;
        self.types.insert(register, RegisterType::Logic { width });
        register
    }

    fn imm(
        &mut self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        width: usize,
        value: u64,
    ) -> RegisterId {
        let register = self.bit(width);
        instructions.push(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    fn logic_imm(
        &mut self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        value: u64,
    ) -> RegisterId {
        let register = self.logic(1);
        instructions.push(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    fn binary(
        &mut self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        width: usize,
        lhs: RegisterId,
        operation: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.bit(width);
        instructions.push(SIRInstruction::Binary(register, lhs, operation, rhs));
        register
    }

    fn logic_binary(
        &mut self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        lhs: RegisterId,
        operation: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.logic(1);
        instructions.push(SIRInstruction::Binary(register, lhs, operation, rhs));
        register
    }

    fn truth(
        &mut self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        source: RegisterId,
    ) -> RegisterId {
        let register = self.bit(1);
        instructions.push(SIRInstruction::Unary(register, UnaryOp::ToTwoState, source));
        register
    }
}

fn fixture(index_step: u64, side_effect: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut builder = Builder::new();
    let head = builder.bit(2);

    let mut preheader_instructions = Vec::new();
    let initial_count = builder.imm(&mut preheader_instructions, 3, LANES as u64);
    let initial_index = builder.imm(&mut preheader_instructions, 2, 0);
    let initial_found = builder.logic_imm(&mut preheader_instructions, 0);
    let initial_best = builder.imm(&mut preheader_instructions, 2, 0);
    let one_count = builder.imm(&mut preheader_instructions, 3, 1);
    let one_index = builder.imm(&mut preheader_instructions, 2, index_step);
    let one_found = builder.logic_imm(&mut preheader_instructions, 1);
    let zero_count = builder.imm(&mut preheader_instructions, 3, 0);
    let zero_shift = builder.imm(&mut preheader_instructions, 2, 0);
    let lane_mask = builder.imm(&mut preheader_instructions, 2, 3);
    let preheader = BasicBlock {
        id: BlockId(0),
        params: vec![head],
        instructions: preheader_instructions,
        terminator: SIRTerminator::Jump(
            BlockId(1),
            vec![initial_count, initial_index, initial_found, initial_best],
        ),
    };

    let count = builder.bit(3);
    let index = builder.bit(2);
    let found = builder.logic(1);
    let best = builder.bit(2);
    let mut header_instructions = Vec::new();
    let mut loads = Vec::new();
    for raw in 0..3 {
        let load = builder.logic(1);
        header_instructions.push(SIRInstruction::Load(
            load,
            address(raw),
            SIROffset::Element {
                index,
                element_width: 1,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            1,
        ));
        loads.push(load);
    }
    let either = builder.logic_binary(
        &mut header_instructions,
        loads[1],
        BinaryOp::LogicOr,
        loads[2],
    );
    let predicate = builder.logic_binary(
        &mut header_instructions,
        loads[0],
        BinaryOp::LogicAnd,
        either,
    );
    if side_effect {
        header_instructions.push(SIRInstruction::Store(
            address(9),
            SIROffset::Static(0),
            1,
            predicate,
            Vec::new(),
            Vec::new(),
        ));
    }
    let predicate = builder.truth(&mut header_instructions, predicate);
    let header = BasicBlock {
        id: BlockId(1),
        params: vec![count, index, found, best],
        instructions: header_instructions,
        terminator: SIRTerminator::Branch {
            cond: predicate,
            true_block: (BlockId(2), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    };

    let mut candidate_instructions = Vec::new();
    let not_found = builder.logic(1);
    candidate_instructions.push(SIRInstruction::Unary(not_found, UnaryOp::LogicNot, found));
    let normalized_index = builder.binary(
        &mut candidate_instructions,
        2,
        index,
        BinaryOp::Shr,
        zero_shift,
    );
    let normalized_index = builder.binary(
        &mut candidate_instructions,
        2,
        normalized_index,
        BinaryOp::And,
        lane_mask,
    );
    let difference = builder.binary(
        &mut candidate_instructions,
        2,
        normalized_index,
        BinaryOp::Sub,
        head,
    );
    let age = builder.binary(
        &mut candidate_instructions,
        2,
        difference,
        BinaryOp::And,
        lane_mask,
    );
    let younger = builder.binary(&mut candidate_instructions, 1, age, BinaryOp::LtU, best);
    let update = builder.logic_binary(
        &mut candidate_instructions,
        not_found,
        BinaryOp::LogicOr,
        younger,
    );
    let update = builder.truth(&mut candidate_instructions, update);
    let candidate = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: candidate_instructions,
        terminator: SIRTerminator::Branch {
            cond: update,
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    };
    let update = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(5), vec![one_found, age]),
    };
    let skip = BasicBlock {
        id: BlockId(4),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(5), vec![found, best]),
    };

    let merged_found = builder.logic(1);
    let merged_best = builder.bit(2);
    let mut latch_instructions = Vec::new();
    let next_count = builder.binary(&mut latch_instructions, 3, count, BinaryOp::Sub, one_count);
    let keep_looping = builder.binary(
        &mut latch_instructions,
        1,
        next_count,
        BinaryOp::Ne,
        zero_count,
    );
    let next_index = builder.binary(&mut latch_instructions, 2, index, BinaryOp::Add, one_index);
    let latch = BasicBlock {
        id: BlockId(5),
        params: vec![merged_found, merged_best],
        instructions: latch_instructions,
        terminator: SIRTerminator::Branch {
            cond: keep_looping,
            true_block: (
                BlockId(1),
                vec![next_count, next_index, merged_found, merged_best],
            ),
            false_block: (BlockId(6), vec![merged_found, merged_best]),
        },
    };

    let result_found = builder.logic(1);
    let result_best = builder.bit(2);
    let exit = BasicBlock {
        id: BlockId(6),
        params: vec![result_found, result_best],
        instructions: vec![
            SIRInstruction::Store(
                address(3),
                SIROffset::Static(0),
                1,
                result_found,
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                address(4),
                SIROffset::Static(0),
                2,
                result_best,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };

    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [preheader, header, candidate, update, skip, latch, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: builder.types,
    }
}

fn bit_map_loop_fixture() -> ExecutionUnit<RegionedAbsoluteAddr> {
    const WIDTH: usize = 16;
    const ACCUMULATOR_WIDTH: usize = 32;
    let mut builder = Builder::new();
    let mut preheader_instructions = Vec::new();
    let initial_count = builder.imm(&mut preheader_instructions, 5, WIDTH as u64);
    let initial_index = builder.imm(&mut preheader_instructions, 4, 0);
    let initial_result = builder.imm(&mut preheader_instructions, ACCUMULATOR_WIDTH, 0xa55a_a55a);
    let one_count = builder.imm(&mut preheader_instructions, 5, 1);
    let zero_count = builder.imm(&mut preheader_instructions, 5, 0);
    let one_index = builder.imm(&mut preheader_instructions, 4, 1);
    let one_wide = builder.imm(&mut preheader_instructions, ACCUMULATOR_WIDTH, 1);
    let zero_padding = builder.imm(&mut preheader_instructions, ACCUMULATOR_WIDTH - 1, 0);
    let preheader = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: preheader_instructions,
        terminator: SIRTerminator::Jump(
            BlockId(1),
            vec![initial_count, initial_index, initial_result],
        ),
    };

    let count = builder.bit(5);
    let index = builder.bit(4);
    let current = builder.bit(ACCUMULATOR_WIDTH);
    let mut instructions = Vec::new();
    let bit = builder.bit(1);
    instructions.push(SIRInstruction::Load(
        bit,
        address(0),
        SIROffset::Dynamic(index),
        1,
    ));
    let onehot = builder.binary(
        &mut instructions,
        ACCUMULATOR_WIDTH,
        one_wide,
        BinaryOp::Shl,
        index,
    );
    let inverted = builder.bit(ACCUMULATOR_WIDTH);
    instructions.push(SIRInstruction::Unary(inverted, UnaryOp::BitNot, onehot));
    let cleared = builder.binary(
        &mut instructions,
        ACCUMULATOR_WIDTH,
        current,
        BinaryOp::And,
        inverted,
    );
    let extended = builder.bit(ACCUMULATOR_WIDTH);
    instructions.push(SIRInstruction::Concat(extended, vec![zero_padding, bit]));
    let shifted = builder.binary(
        &mut instructions,
        ACCUMULATOR_WIDTH,
        extended,
        BinaryOp::Shl,
        index,
    );
    let inserted = builder.binary(
        &mut instructions,
        ACCUMULATOR_WIDTH,
        shifted,
        BinaryOp::And,
        onehot,
    );
    let update = builder.binary(
        &mut instructions,
        ACCUMULATOR_WIDTH,
        cleared,
        BinaryOp::Or,
        inserted,
    );
    let next_count = builder.binary(&mut instructions, 5, count, BinaryOp::Sub, one_count);
    let more = builder.binary(&mut instructions, 1, next_count, BinaryOp::Ne, zero_count);
    let next_index = builder.binary(&mut instructions, 4, index, BinaryOp::Add, one_index);
    let header = BasicBlock {
        id: BlockId(1),
        params: vec![count, index, current],
        instructions,
        terminator: SIRTerminator::Branch {
            cond: more,
            true_block: (BlockId(1), vec![next_count, next_index, update]),
            false_block: (BlockId(2), vec![update]),
        },
    };
    let result = builder.bit(ACCUMULATOR_WIDTH);
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![result],
        instructions: vec![SIRInstruction::Store(
            address(3),
            SIROffset::Static(0),
            ACCUMULATOR_WIDTH,
            result,
            Vec::new(),
            Vec::new(),
        )],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [preheader, header, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: builder.types,
    }
}

fn execute(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    head: u64,
    valid: u64,
    a: u64,
    b: u64,
) -> (u64, u64) {
    let result = execute_with(
        eu,
        &[head],
        &[(address(0), valid), (address(1), a), (address(2), b)],
        &[address(3), address(4)],
    );
    (result[0], result[1])
}

fn execute_with(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    entry_values: &[u64],
    initial_memory: &[(RegionedAbsoluteAddr, u64)],
    outputs: &[RegionedAbsoluteAddr],
) -> Vec<u64> {
    let mut registers = HashMap::default();
    for (&parameter, &value) in eu.blocks[&eu.entry_block_id]
        .params
        .iter()
        .zip(entry_values)
    {
        registers.insert(parameter, value);
    }
    let mut memory = initial_memory.iter().copied().collect::<HashMap<_, _>>();
    let mut block_id = eu.entry_block_id;
    for _ in 0..128 {
        let block = &eu.blocks[&block_id];
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(destination, value) => {
                    registers.insert(*destination, value.payload.to_u64().unwrap_or(0));
                }
                SIRInstruction::Binary(destination, lhs, operation, rhs) => {
                    let lhs = registers[lhs];
                    let rhs = registers[rhs];
                    let value = match operation {
                        BinaryOp::Add => lhs.wrapping_add(rhs),
                        BinaryOp::Sub => lhs.wrapping_sub(rhs),
                        BinaryOp::And | BinaryOp::LogicAnd => lhs & rhs,
                        BinaryOp::Or | BinaryOp::LogicOr => lhs | rhs,
                        BinaryOp::Xor => lhs ^ rhs,
                        BinaryOp::Shl => lhs.checked_shl(rhs as u32).unwrap_or(0),
                        BinaryOp::Shr => lhs >> rhs,
                        BinaryOp::Eq | BinaryOp::EqWildcard => u64::from(lhs == rhs),
                        BinaryOp::Ne => u64::from(lhs != rhs),
                        BinaryOp::LtU => u64::from(lhs < rhs),
                        other => panic!("unsupported binary operation {other:?}"),
                    };
                    let width = eu.register_map[destination].width();
                    registers.insert(*destination, value & ((1u64 << width) - 1));
                }
                SIRInstruction::Unary(destination, operation, source) => {
                    let source_width = eu.register_map[source].width() as u32;
                    let source = registers[source];
                    let value = match operation {
                        UnaryOp::ToTwoState | UnaryOp::Ident => source,
                        UnaryOp::LogicNot => u64::from(source == 0),
                        UnaryOp::BitNot => !source,
                        UnaryOp::Or => u64::from(source != 0),
                        UnaryOp::PopCount => source.count_ones() as u64,
                        UnaryOp::CountTrailingZeros => source.trailing_zeros() as u64,
                        UnaryOp::CountLeadingZeros => {
                            if source == 0 {
                                u64::from(source_width)
                            } else {
                                u64::from(source.leading_zeros() - (64 - source_width))
                            }
                        }
                        other => panic!("unsupported unary operation {other:?}"),
                    };
                    let width = eu.register_map[destination].width();
                    registers.insert(*destination, value & ((1u64 << width) - 1));
                }
                SIRInstruction::Load(destination, address, offset, width) => {
                    let offset = match offset {
                        SIROffset::Static(offset)
                        | SIROffset::PackedElements {
                            bit_offset: offset, ..
                        } => *offset,
                        SIROffset::Dynamic(index) => registers[index] as usize,
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset,
                        } => {
                            registers[index] as usize * element_width
                                + bit_offset
                                + dynamic_bit_offset
                                    .map(|dynamic| registers[&dynamic] as usize)
                                    .unwrap_or(0)
                        }
                    };
                    let value = memory.get(address).copied().unwrap_or(0) >> offset;
                    registers.insert(*destination, value & ((1u64 << width) - 1));
                }
                SIRInstruction::Store(address, SIROffset::Static(offset), width, source, ..) => {
                    let mask = ((1u64 << width) - 1) << offset;
                    let old = memory.get(address).copied().unwrap_or(0);
                    memory.insert(
                        *address,
                        (old & !mask) | ((registers[source] << offset) & mask),
                    );
                }
                SIRInstruction::Concat(destination, arguments) => {
                    let value = arguments.iter().fold(0u64, |value, argument| {
                        (value << eu.register_map[argument].width()) | registers[argument]
                    });
                    registers.insert(*destination, value);
                }
                SIRInstruction::Slice(destination, source, offset, width) => {
                    registers.insert(
                        *destination,
                        (registers[source] >> offset) & ((1u64 << width) - 1),
                    );
                }
                SIRInstruction::Mux(destination, condition, on_true, on_false) => {
                    registers.insert(
                        *destination,
                        if registers[condition] != 0 {
                            registers[on_true]
                        } else {
                            registers[on_false]
                        },
                    );
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }
        let (next, arguments) = match &block.terminator {
            SIRTerminator::Jump(target, arguments) => (*target, arguments),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if registers[cond] != 0 {
                    (true_block.0, &true_block.1)
                } else {
                    (false_block.0, &false_block.1)
                }
            }
            SIRTerminator::Switch { .. } => {
                panic!("unexpected Switch in circular-priority test")
            }
            SIRTerminator::Return => {
                return outputs
                    .iter()
                    .map(|address| memory.get(address).copied().unwrap_or(0))
                    .collect();
            }
            SIRTerminator::Error(code) => panic!("unexpected error {code}"),
        };
        let values = arguments
            .iter()
            .map(|argument| registers[argument])
            .collect::<Vec<_>>();
        for (&parameter, value) in eu.blocks[&next].params.iter().zip(values) {
            registers.insert(parameter, value);
        }
        block_id = next;
    }
    panic!("test execution did not terminate");
}

fn make_lane_mask_escape(unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> RegisterId {
    let position = unit.blocks[&BlockId(0)]
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Imm(register, value)
                    if unit.register_map[register].width() == 2
                        && value.payload.to_u64() == Some(3)
            )
        })
        .unwrap();
    let instruction = unit
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .remove(position);
    let register = def_reg(&instruction).unwrap();
    unit.blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .instructions
        .insert(0, instruction);
    unit.blocks
        .get_mut(&BlockId(6))
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address(5),
            SIROffset::Static(0),
            2,
            register,
            Vec::new(),
            Vec::new(),
        ));
    register
}

fn assert_equivalent(
    original: &ExecutionUnit<RegionedAbsoluteAddr>,
    rewritten: &ExecutionUnit<RegionedAbsoluteAddr>,
) {
    for head in 0..LANES as u64 {
        for valid in 0..1u64 << LANES {
            for a in 0..1u64 << LANES {
                for b in 0..1u64 << LANES {
                    assert_eq!(
                        execute(rewritten, head, valid, a, b),
                        execute(original, head, valid, a, b),
                        "head={head} valid={valid:#x} a={a:#x} b={b:#x}"
                    );
                }
            }
        }
    }
}

fn linear_scan_fixture() -> ExecutionUnit<RegionedAbsoluteAddr> {
    const SCAN_LANES: usize = 8;
    let mut builder = Builder::new();
    let bound = builder.bit(8);
    let broadcast = builder.bit(1);
    let mut preheader_instructions = Vec::new();
    let trip = builder.imm(&mut preheader_instructions, 4, SCAN_LANES as u64);
    let initial_index = builder.imm(&mut preheader_instructions, 3, 0);
    let initial_count = builder.imm(&mut preheader_instructions, 4, 0);
    let initial_best = builder.imm(&mut preheader_instructions, 8, 0xff);
    let initial_found = builder.imm(&mut preheader_instructions, 1, 0);
    let one4 = builder.imm(&mut preheader_instructions, 4, 1);
    let one3 = builder.imm(&mut preheader_instructions, 3, 1);
    let one1 = builder.imm(&mut preheader_instructions, 1, 1);
    let zero4 = builder.imm(&mut preheader_instructions, 4, 0);
    let zero5 = builder.imm(&mut preheader_instructions, 5, 0);
    let preheader = BasicBlock {
        id: BlockId(0),
        params: vec![bound, broadcast],
        instructions: preheader_instructions,
        terminator: SIRTerminator::Jump(
            BlockId(1),
            vec![
                trip,
                initial_index,
                initial_count,
                initial_best,
                initial_found,
            ],
        ),
    };

    let remaining = builder.bit(4);
    let index = builder.bit(3);
    let count = builder.bit(4);
    let best = builder.bit(8);
    let found = builder.bit(1);
    let not_found = builder.bit(1);
    let header = BasicBlock {
        id: BlockId(1),
        params: vec![remaining, index, count, best, found],
        instructions: vec![SIRInstruction::Unary(not_found, UnaryOp::LogicNot, found)],
        terminator: SIRTerminator::Branch {
            cond: not_found,
            true_block: (BlockId(2), Vec::new()),
            false_block: (BlockId(3), Vec::new()),
        },
    };
    let first = builder.bit(8);
    let first_arm = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: vec![SIRInstruction::Concat(first, vec![zero5, index])],
        terminator: SIRTerminator::Jump(BlockId(4), vec![first, one1]),
    };
    let carry_arm = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(4), vec![best, found]),
    };

    let candidate_best = builder.bit(8);
    let candidate_found = builder.bit(1);
    let mut body_instructions = Vec::new();
    let a = builder.bit(1);
    body_instructions.push(SIRInstruction::Load(
        a,
        address(0),
        SIROffset::Dynamic(index),
        1,
    ));
    let b = builder.bit(1);
    body_instructions.push(SIRInstruction::Load(
        b,
        address(1),
        SIROffset::Dynamic(index),
        1,
    ));
    let wide_index = builder.bit(8);
    body_instructions.push(SIRInstruction::Concat(wide_index, vec![zero5, index]));
    let in_prefix = builder.binary(&mut body_instructions, 1, wide_index, BinaryOp::LtU, bound);
    let enabled = builder.binary(&mut body_instructions, 1, broadcast, BinaryOp::Or, a);
    let enabled = builder.binary(&mut body_instructions, 1, in_prefix, BinaryOp::And, enabled);
    let predicate = builder.binary(&mut body_instructions, 1, enabled, BinaryOp::And, b);
    let incremented = builder.binary(&mut body_instructions, 4, count, BinaryOp::Add, one4);
    let next_count = builder.bit(4);
    body_instructions.push(SIRInstruction::Mux(
        next_count,
        predicate,
        incremented,
        count,
    ));
    let next_best = builder.bit(8);
    body_instructions.push(SIRInstruction::Mux(
        next_best,
        predicate,
        candidate_best,
        best,
    ));
    let next_found = builder.bit(1);
    body_instructions.push(SIRInstruction::Mux(
        next_found,
        predicate,
        candidate_found,
        found,
    ));
    let next_remaining = builder.binary(&mut body_instructions, 4, remaining, BinaryOp::Sub, one4);
    let continues = builder.binary(
        &mut body_instructions,
        1,
        next_remaining,
        BinaryOp::Ne,
        zero4,
    );
    let next_index = builder.binary(&mut body_instructions, 3, index, BinaryOp::Add, one3);
    let body = BasicBlock {
        id: BlockId(4),
        params: vec![candidate_best, candidate_found],
        instructions: body_instructions,
        terminator: SIRTerminator::Branch {
            cond: continues,
            true_block: (
                BlockId(1),
                vec![
                    next_remaining,
                    next_index,
                    next_count,
                    next_best,
                    next_found,
                ],
            ),
            false_block: (BlockId(5), vec![next_count, next_best, next_found]),
        },
    };
    let result_count = builder.bit(4);
    let result_best = builder.bit(8);
    let result_found = builder.bit(1);
    let exit = BasicBlock {
        id: BlockId(5),
        params: vec![result_count, result_best, result_found],
        instructions: vec![
            SIRInstruction::Store(
                address(3),
                SIROffset::Static(0),
                4,
                result_count,
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                address(4),
                SIROffset::Static(0),
                8,
                result_best,
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                address(5),
                SIROffset::Static(0),
                1,
                result_found,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [preheader, header, first_arm, carry_arm, body, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: builder.types,
    }
}

fn last_payload_scan_fixture() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut builder = Builder::new();
    let query = builder.bit(3);
    let mut preheader_instructions = Vec::new();
    let trip = builder.imm(&mut preheader_instructions, 3, LANES as u64);
    let initial_index = builder.imm(&mut preheader_instructions, 2, 0);
    let initial_found = builder.imm(&mut preheader_instructions, 1, 0);
    let initial_payload = builder.imm(&mut preheader_instructions, 5, 0x1b);
    let one3 = builder.imm(&mut preheader_instructions, 3, 1);
    let one2 = builder.imm(&mut preheader_instructions, 2, 1);
    let one1 = builder.imm(&mut preheader_instructions, 1, 1);
    let zero3 = builder.imm(&mut preheader_instructions, 3, 0);
    let preheader = BasicBlock {
        id: BlockId(0),
        params: vec![query],
        instructions: preheader_instructions,
        terminator: SIRTerminator::Jump(
            BlockId(1),
            vec![trip, initial_index, initial_found, initial_payload],
        ),
    };

    let remaining = builder.bit(3);
    let index = builder.bit(2);
    let found = builder.bit(1);
    let payload = builder.bit(5);
    let lane = builder.bit(3);
    let predicate = builder.bit(1);
    let next_found = builder.bit(1);
    let mut header_instructions = vec![
        SIRInstruction::Load(
            lane,
            address(0),
            SIROffset::Element {
                index,
                element_width: 3,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            3,
        ),
        SIRInstruction::Binary(predicate, lane, BinaryOp::Eq, query),
        SIRInstruction::Mux(next_found, predicate, one1, found),
    ];
    let escaped_zero = builder.imm(&mut header_instructions, 5, 0);
    let header = BasicBlock {
        id: BlockId(1),
        params: vec![remaining, index, found, payload],
        instructions: header_instructions,
        terminator: SIRTerminator::Branch {
            cond: predicate,
            true_block: (BlockId(2), Vec::new()),
            false_block: (BlockId(3), Vec::new()),
        },
    };
    let selected_payload = builder.bit(5);
    let selected = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: vec![SIRInstruction::Load(
            selected_payload,
            address(1),
            SIROffset::Element {
                index,
                element_width: 5,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            5,
        )],
        terminator: SIRTerminator::Jump(BlockId(4), vec![selected_payload]),
    };
    let carry = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(4), vec![payload]),
    };
    let merged_payload = builder.bit(5);
    let mut latch_instructions = Vec::new();
    let next_remaining = builder.binary(&mut latch_instructions, 3, remaining, BinaryOp::Sub, one3);
    let keep_looping = builder.binary(
        &mut latch_instructions,
        1,
        next_remaining,
        BinaryOp::Ne,
        zero3,
    );
    let next_index = builder.binary(&mut latch_instructions, 2, index, BinaryOp::Add, one2);
    let latch = BasicBlock {
        id: BlockId(4),
        params: vec![merged_payload],
        instructions: latch_instructions,
        terminator: SIRTerminator::Branch {
            cond: keep_looping,
            true_block: (
                BlockId(1),
                vec![next_remaining, next_index, next_found, merged_payload],
            ),
            false_block: (BlockId(5), vec![next_found, merged_payload]),
        },
    };
    let result_found = builder.bit(1);
    let result_payload = builder.bit(5);
    let normalized_payload = builder.bit(5);
    let exit = BasicBlock {
        id: BlockId(5),
        params: vec![result_found, result_payload],
        instructions: vec![
            SIRInstruction::Binary(
                normalized_payload,
                result_payload,
                BinaryOp::Add,
                escaped_zero,
            ),
            SIRInstruction::Store(
                address(2),
                SIROffset::Static(0),
                1,
                result_found,
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                address(3),
                SIROffset::Static(0),
                5,
                normalized_payload,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [preheader, header, selected, carry, latch, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: builder.types,
    }
}

fn sparse_bitmap_loop_fixture() -> ExecutionUnit<RegionedAbsoluteAddr> {
    sparse_bitmap_loop_fixture_with(false)
}

/// With `escaping_constant`, the loop defines its own constant one and
/// the exit block uses it directly, which is valid only while the loop
/// dominates the exit.
fn sparse_bitmap_loop_fixture_with(escaping_constant: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    const SPARSE_LANES: usize = 16;
    let mut builder = Builder::new();
    let enable0 = builder.bit(1);
    let enable1 = builder.bit(1);
    let enable2 = builder.bit(1);
    let bound0 = builder.bit(4);
    let bound1 = builder.bit(4);
    let bound2 = builder.bit(4);
    let mut preheader_instructions = Vec::new();
    let trip = builder.imm(&mut preheader_instructions, 5, SPARSE_LANES as u64);
    let initial_index = builder.imm(&mut preheader_instructions, 4, 0);
    let initial_result = builder.imm(&mut preheader_instructions, 1, 0);
    let one_count = builder.imm(&mut preheader_instructions, 5, 1);
    let one_index = builder.imm(&mut preheader_instructions, 4, 1);
    let one_result = builder.imm(&mut preheader_instructions, 1, 1);
    let zero_count = builder.imm(&mut preheader_instructions, 5, 0);
    let preheader = BasicBlock {
        id: BlockId(0),
        params: vec![enable0, enable1, enable2, bound0, bound1, bound2],
        instructions: preheader_instructions,
        terminator: SIRTerminator::Jump(
            BlockId(1),
            vec![
                trip,
                initial_index,
                initial_result,
                initial_result,
                initial_result,
            ],
        ),
    };

    let remaining = builder.bit(5);
    let index = builder.bit(4);
    let result0 = builder.bit(1);
    let result1 = builder.bit(1);
    let result2 = builder.bit(1);
    let mut instructions = Vec::new();
    let one_result = if escaping_constant {
        builder.imm(&mut instructions, 1, 1)
    } else {
        one_result
    };
    let mut loaded = Vec::new();
    for raw in 0..4 {
        let lane = builder.bit(1);
        instructions.push(SIRInstruction::Load(
            lane,
            address(raw),
            SIROffset::Element {
                index,
                element_width: 1,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            1,
        ));
        loaded.push(lane);
    }
    let common01 = builder.binary(
        &mut instructions,
        1,
        loaded[0],
        BinaryOp::LogicAnd,
        loaded[1],
    );
    let common012 = builder.binary(
        &mut instructions,
        1,
        common01,
        BinaryOp::LogicAnd,
        loaded[2],
    );
    let not3 = builder.bit(1);
    instructions.push(SIRInstruction::Unary(not3, UnaryOp::LogicNot, loaded[3]));
    let common = builder.binary(&mut instructions, 1, common012, BinaryOp::LogicAnd, not3);
    let mut next_results = Vec::new();
    for (enable, bound, current) in [
        (enable0, bound0, result0),
        (enable1, bound1, result1),
        (enable2, bound2, result2),
    ] {
        let enabled = builder.binary(&mut instructions, 1, common, BinaryOp::LogicAnd, enable);
        let in_range = builder.binary(&mut instructions, 1, index, BinaryOp::LtU, bound);
        let condition = builder.binary(&mut instructions, 1, enabled, BinaryOp::LogicAnd, in_range);
        let next = builder.bit(1);
        instructions.push(SIRInstruction::Mux(next, condition, one_result, current));
        next_results.push(next);
    }
    let next_remaining = builder.binary(&mut instructions, 5, remaining, BinaryOp::Sub, one_count);
    let continues = builder.binary(
        &mut instructions,
        1,
        next_remaining,
        BinaryOp::Ne,
        zero_count,
    );
    let next_index = builder.binary(&mut instructions, 4, index, BinaryOp::Add, one_index);
    let header = BasicBlock {
        id: BlockId(1),
        params: vec![remaining, index, result0, result1, result2],
        instructions,
        terminator: SIRTerminator::Branch {
            cond: continues,
            true_block: (
                BlockId(1),
                vec![
                    next_remaining,
                    next_index,
                    next_results[0],
                    next_results[1],
                    next_results[2],
                ],
            ),
            false_block: (
                BlockId(2),
                vec![next_results[0], next_results[1], next_results[2]],
            ),
        },
    };

    let output0 = builder.bit(1);
    let output1 = builder.bit(1);
    let output2 = builder.bit(1);
    let mut exit_instructions = Vec::new();
    let mut outputs = vec![output0, output1, output2];
    if escaping_constant {
        outputs[0] = builder.binary(
            &mut exit_instructions,
            1,
            output0,
            BinaryOp::And,
            one_result,
        );
    }
    exit_instructions.extend(outputs.into_iter().enumerate().map(|(index, output)| {
        SIRInstruction::Store(
            address(4 + index as u32),
            SIROffset::Static(0),
            1,
            output,
            Vec::new(),
            Vec::new(),
        )
    }));
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![output0, output1, output2],
        instructions: exit_instructions,
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [preheader, header, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: builder.types,
    }
}

#[test]
fn iterates_only_set_bits_of_a_shared_loop_predicate() {
    const SPARSE_LANES: usize = 16;
    let mut unit = sparse_bitmap_loop_fixture();
    unit.verify_result().unwrap();
    let original = unit.clone();
    let pass = CircularPriorityPass {
        bit_array_elements: (0..4)
            .map(|raw| (address(raw).absolute_addr(), SPARSE_LANES))
            .collect(),
        array_shapes: HashMap::default(),
    };
    pass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();

    let header = &unit.blocks[&BlockId(1)];
    assert!(header.instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
        )
    }));
    assert!(!header.instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Load(_, _, SIROffset::Element { .. }, 1)
        )
    }));

    let mut masks = vec![0, 1, 0x00ff, 0x0f0f, 0x5555, 0xaaaa, 0x8000, 0xffff];
    for lane in 0..SPARSE_LANES {
        masks.push(1u64 << lane);
        masks.push((1u64 << (lane + 1)) - 1);
    }
    masks.sort_unstable();
    masks.dedup();
    for mask in masks {
        for enables in 0..8u64 {
            let inputs = &[
                enables & 1,
                (enables >> 1) & 1,
                (enables >> 2) & 1,
                3,
                8,
                15,
            ];
            let memory = &[
                (address(0), mask),
                (address(1), 0xffff),
                (address(2), 0xffff),
                (address(3), 0),
            ];
            let outputs = &[address(4), address(5), address(6)];
            assert_eq!(
                execute_with(&unit, inputs, memory, outputs),
                execute_with(&original, inputs, memory, outputs),
                "mask={mask:#06x} enables={enables:#x}"
            );
        }
    }
}

#[test]
fn sparse_scan_keeps_loops_whose_values_escape_to_the_exit() {
    const SPARSE_LANES: usize = 16;
    let mut unit = sparse_bitmap_loop_fixture_with(true);
    unit.verify_result().unwrap();
    let original = unit.clone();
    let pass = CircularPriorityPass {
        bit_array_elements: (0..4)
            .map(|raw| (address(raw).absolute_addr(), SPARSE_LANES))
            .collect(),
        array_shapes: HashMap::default(),
    };
    pass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();
    assert_eq!(unit, original);
}

#[test]
fn recovers_last_matching_lane_payload_from_multibit_array() {
    let mut unit = last_payload_scan_fixture();
    unit.verify_result().unwrap();
    let original = unit.clone();
    let pass = CircularPriorityPass {
        bit_array_elements: HashMap::default(),
        array_shapes: [
            (
                address(0).absolute_addr(),
                ArrayShape {
                    element_width: 3,
                    element_count: LANES,
                },
            ),
            (
                address(1).absolute_addr(),
                ArrayShape {
                    element_width: 5,
                    element_count: LANES,
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    pass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();
    assert_eq!(unit.blocks.len(), 4);
    assert!(unit.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
            )
        })
    }));

    for query in 0..8 {
        for lanes in 0..1u64 << (3 * LANES) {
            for payloads in [0, 0x12345, 0xfffff] {
                let memory = &[(address(0), lanes), (address(1), payloads)];
                let outputs = &[address(2), address(3)];
                assert_eq!(
                    execute_with(&unit, &[query], memory, outputs),
                    execute_with(&original, &[query], memory, outputs),
                    "query={query} lanes={lanes:#x} payloads={payloads:#x}"
                );
            }
        }
    }
}

#[test]
fn recovers_linear_bitmap_count_and_first_match() {
    let mut unit = linear_scan_fixture();
    unit.verify_result().unwrap();
    let original = unit.clone();
    let pass = CircularPriorityPass {
        bit_array_elements: [
            (address(0).absolute_addr(), 8),
            (address(1).absolute_addr(), 8),
        ]
        .into_iter()
        .collect(),
        array_shapes: HashMap::default(),
    };
    pass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();
    assert_eq!(unit.blocks.len(), 2);
    assert!(
        unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(instruction, SIRInstruction::Unary(_, UnaryOp::PopCount, _))
            })
    );
    assert!(
        unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
                )
            })
    );

    for bound in [0, 1, 4, 8, 9, 0xff] {
        for broadcast in [0, 1] {
            for a in [0, 1, 0x55, 0x80, 0xff] {
                for b in [0, 1, 0x33, 0x80, 0xff] {
                    let inputs = &[bound, broadcast];
                    let memory = &[(address(0), a), (address(1), b)];
                    let outputs = &[address(3), address(4), address(5)];
                    assert_eq!(
                        execute_with(&unit, inputs, memory, outputs),
                        execute_with(&original, inputs, memory, outputs),
                        "bound={bound} broadcast={broadcast} a={a:#x} b={b:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn recovers_packed_circular_priority_and_preserves_all_small_inputs() {
    let mut unit = fixture(1, false);
    unit.verify_result().unwrap();
    let original = unit.clone();
    test_pass().run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();

    assert_eq!(unit.blocks.len(), 4);
    assert!(!unit.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(_, _, SIROffset::Element { .. }, _)
            )
        })
    }));
    assert_eq!(
        unit.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(_, _, SIROffset::Static(0), LANES)
                )
            })
            .count(),
        3
    );
    assert!(unit.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
            )
        })
    }));

    assert_equivalent(&original, &unit);
}

#[test]
fn hoists_pure_loop_invariants_that_are_reused_after_the_loop() {
    let mut unit = fixture(1, false);
    let lane_mask = make_lane_mask_escape(&mut unit);
    unit.verify_result().unwrap();
    let original = unit.clone();

    test_pass().run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();

    assert_eq!(unit.blocks.len(), 4);
    assert!(
        unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(lane_mask))
    );
    assert_equivalent(&original, &unit);
}

#[test]
fn accepts_an_equivalent_unmasked_lane_expression() {
    let mut unit = fixture(1, false);
    let candidate = unit.blocks.get_mut(&BlockId(2)).unwrap();
    let (masked_index, unmasked_index) = match candidate.instructions.remove(2) {
        SIRInstruction::Binary(destination, source, BinaryOp::And, _) => (destination, source),
        other => panic!("unexpected fixture instruction {other:?}"),
    };
    let SIRInstruction::Binary(_, lane, BinaryOp::Sub, _) = &mut candidate.instructions[2] else {
        panic!("fixture difference is not a subtraction");
    };
    assert_eq!(*lane, masked_index);
    *lane = unmasked_index;
    unit.verify_result().unwrap();
    let original = unit.clone();

    test_pass().run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();

    assert_eq!(unit.blocks.len(), 4);
    assert_equivalent(&original, &unit);
}

#[test]
fn rejects_an_index_too_narrow_to_enumerate_every_lane() {
    let mut unit = fixture(1, false);
    let index = unit.blocks[&BlockId(1)].params[1];
    unit.register_map.insert(index, unsigned_type(1));
    let original = unit.to_string();

    test_pass().run(&mut unit, &PassOptions::default());

    assert_eq!(unit.to_string(), original);
}

#[test]
fn rejects_a_bit_array_shorter_than_the_scan_domain() {
    let mut unit = fixture(1, false);
    let original = unit.to_string();
    let mut pass = test_pass();
    pass.bit_array_elements
        .insert(address(0).absolute_addr(), LANES - 1);

    pass.run(&mut unit, &PassOptions::default());

    assert_eq!(unit.to_string(), original);
}

#[test]
fn rejects_loop_variant_values_reused_after_the_loop() {
    let mut unit = fixture(1, false);
    let index = unit.blocks[&BlockId(1)].params[1];
    unit.blocks
        .get_mut(&BlockId(6))
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address(5),
            SIROffset::Static(0),
            2,
            index,
            Vec::new(),
            Vec::new(),
        ));
    let original = unit.to_string();

    test_pass().run(&mut unit, &PassOptions::default());

    assert_eq!(unit.to_string(), original);
}

#[test]
fn leaves_four_state_mode_unchanged() {
    let mut unit = fixture(1, false);
    let original = unit.to_string();
    test_pass().run(
        &mut unit,
        &PassOptions {
            four_state: true,
            ..PassOptions::default()
        },
    );
    assert_eq!(unit.to_string(), original);
}

#[test]
fn rejects_non_unit_lane_recurrence() {
    let mut unit = fixture(2, false);
    let original = unit.to_string();
    test_pass().run(&mut unit, &PassOptions::default());
    assert_eq!(unit.to_string(), original);
}

#[test]
fn rejects_loop_side_effects() {
    let mut unit = fixture(1, true);
    let original = unit.to_string();
    test_pass().run(&mut unit, &PassOptions::default());
    assert_eq!(unit.to_string(), original);
}

#[test]
fn replaces_fixed_bit_insert_loop_with_one_packed_value() {
    let mut unit = bit_map_loop_fixture();
    unit.verify_result().unwrap();
    let original = unit.clone();

    test_pass().run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();

    assert!(!unit.blocks.contains_key(&BlockId(1)));
    assert!(unit.blocks[&BlockId(0)].instructions.iter().any(
        |instruction| matches!(instruction, SIRInstruction::Concat(_, lanes) if lanes.len() == 16)
    ));
    for input in [0u64, 1, 0x8000, 0x55aa, 0xa55a, 0xffff] {
        let before = execute_with(&original, &[], &[(address(0), input)], &[address(3)]);
        let after = execute_with(&unit, &[], &[(address(0), input)], &[address(3)]);
        assert_eq!(after, before, "input={input:#06x}");
        assert_eq!(after, vec![0xa55a_0000 | input]);
    }
}

#[test]
fn rejects_fixed_bit_insert_loop_with_escaping_definition() {
    let mut unit = bit_map_loop_fixture();
    let escaped = match unit.blocks[&BlockId(1)].instructions[0] {
        SIRInstruction::Load(destination, ..) => destination,
        ref instruction => panic!("expected loop load, got {instruction:?}"),
    };
    unit.blocks
        .get_mut(&BlockId(2))
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address(4),
            SIROffset::Static(0),
            1,
            escaped,
            Vec::new(),
            Vec::new(),
        ));
    unit.verify_result().unwrap();
    let original = unit.to_string();

    test_pass().run(&mut unit, &PassOptions::default());

    unit.verify_result().unwrap();
    assert_eq!(unit.to_string(), original);
}
