use super::*;
use celox_sir::{BasicBlock, SIRSwitchCase};

#[derive(Default)]
struct FakeMachine {
    cells: HashMap<(u32, usize, usize), SIRValue>,
    runtime_events: Vec<(u32, Vec<SIRValue>)>,
    comb_captures: Vec<usize>,
    trigger_notifications: Vec<usize>,
    /// The bases of the frames entered, innermost last.
    frames: Vec<usize>,
    /// Frame slot cells, by frame base.
    frame_cells: HashMap<(usize, u32, usize, usize), SIRValue>,
}

/// Addresses from this one on are frame slots, kept per frame.
const FRAME_SLOTS: u32 = 1000;

impl FakeMachine {
    /// The frame base of `addr` when it is a frame slot.
    fn frame_of(&self, addr: u32) -> Option<usize> {
        self.frames.last().copied().filter(|_| addr >= FRAME_SLOTS)
    }

    fn stored(&self, addr: u32, offset: usize, bits: usize) -> &SIRValue {
        self.cells.get(&(addr, offset, bits)).unwrap()
    }
}

impl InterpMachine<u32> for FakeMachine {
    fn load(
        &mut self,
        addr: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        let offset = match access.offset {
            SIROffset::Static(offset) => *offset,
            other => {
                return Err(InterpError::Machine(format!(
                    "fake machine cannot resolve {other}"
                )));
            }
        };
        let value = match self.frame_of(*addr) {
            Some(base) => self.frame_cells.get(&(base, *addr, offset, bits)),
            None => self.cells.get(&(*addr, offset, bits)),
        };
        Ok(value
            .cloned()
            .unwrap_or_else(|| SIRValue::new(BigUint::zero())))
    }

    fn push_frame(&mut self, base: usize, _function: FunctionId) -> Result<(), InterpError> {
        self.frames.push(base);
        Ok(())
    }

    fn pop_frame(&mut self) {
        self.frames.pop();
    }

    fn store(
        &mut self,
        addr: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: &SIRValue,
    ) -> Result<(), InterpError> {
        let offset = match access.offset {
            SIROffset::Static(offset) => *offset,
            other => {
                return Err(InterpError::Machine(format!(
                    "fake machine cannot resolve {other}"
                )));
            }
        };
        match self.frame_of(*addr) {
            Some(base) => {
                self.frame_cells
                    .insert((base, *addr, offset, bits), value.clone());
            }
            None => {
                self.cells.insert((*addr, offset, bits), value.clone());
            }
        }
        Ok(())
    }

    fn commit(
        &mut self,
        src: &u32,
        dst: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        let offset = match access.offset {
            SIROffset::Static(offset) => *offset,
            other => {
                return Err(InterpError::Machine(format!(
                    "fake machine cannot resolve {other}"
                )));
            }
        };
        let value = self.cells.get(&(*src, offset, bits)).cloned();
        if let Some(value) = value {
            self.cells.insert((*dst, offset, bits), value);
        }
        Ok(())
    }

    fn notify_triggers(
        &mut self,
        _addr: &u32,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        self.trigger_notifications.push(triggers.len());
        Ok(())
    }

    fn notify_trigger_only_store(
        &mut self,
        _addr: &u32,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        self.trigger_notifications.push(triggers.len());
        Ok(())
    }

    fn capture_store_range(
        &mut self,
        addr: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        let offset = match access.offset {
            SIROffset::Static(offset) => *offset,
            other => {
                return Err(InterpError::Machine(format!(
                    "fake machine cannot resolve {other}"
                )));
            }
        };
        Ok(StoreSnapshot {
            value_words: self
                .cells
                .get(&(*addr, offset, bits))
                .map(|value| Self::words(&value.payload))
                .unwrap_or_default(),
            mask_words: Vec::new(),
        })
    }

    fn enable_comb_captures(
        &mut self,
        _addr: &u32,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        _before: &StoreSnapshot,
        sites: &[u32],
    ) -> Result<(), InterpError> {
        self.comb_captures.push(sites.len());
        Ok(())
    }

    fn emit_runtime_event(&mut self, site_id: u32, args: &[SIRValue]) -> Result<(), InterpError> {
        self.runtime_events.push((site_id, args.to_vec()));
        Ok(())
    }

    fn emit_comb_capture_event(
        &mut self,
        _site_id: u32,
        _args: &[SIRValue],
        _fatal_error_code: Option<i64>,
        _consume_enabled: bool,
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn enable_comb_capture_if_changed(
        &mut self,
        _old: &SIRValue,
        _new: &SIRValue,
        _sites: &[u32],
    ) -> Result<(), InterpError> {
        Ok(())
    }
}

impl FakeMachine {
    fn words(_value: &BigUint) -> Vec<u64> {
        Vec::new()
    }
}

fn _addr_owned(addr: &u32) -> u32 {
    *addr
}

fn bit_regs(specs: &[(usize, usize)]) -> HashMap<RegisterId, RegisterType> {
    specs
        .iter()
        .map(|&(id, width)| {
            (
                RegisterId(id),
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            )
        })
        .collect()
}

#[test]
fn narrow_binary_alu_matches_generic_two_state_path() {
    const OPS: &[BinaryOp] = &[
        BinaryOp::Add,
        BinaryOp::Sub,
        BinaryOp::Mul,
        BinaryOp::DivU,
        BinaryOp::DivS,
        BinaryOp::RemU,
        BinaryOp::RemS,
        BinaryOp::And,
        BinaryOp::Or,
        BinaryOp::Xor,
        BinaryOp::Shl,
        BinaryOp::Shr,
        BinaryOp::Sar,
        BinaryOp::Eq,
        BinaryOp::Ne,
        BinaryOp::EqCase,
        BinaryOp::NeCase,
        BinaryOp::LtU,
        BinaryOp::LtS,
        BinaryOp::LeU,
        BinaryOp::LeS,
        BinaryOp::GtU,
        BinaryOp::GtS,
        BinaryOp::GeU,
        BinaryOp::GeS,
        BinaryOp::LogicAnd,
        BinaryOp::LogicOr,
        BinaryOp::EqWildcard,
        BinaryOp::NeWildcard,
    ];
    const WIDTHS: &[usize] = &[1, 4, 8, 32, 63, 64];
    const VALUES: &[u64] = &[
        0,
        1,
        2,
        3,
        7,
        0x80,
        0xffff_ffff,
        0x8000_0000_0000_0000,
        u64::MAX,
    ];

    for op in OPS {
        for &lhs_width in WIDTHS {
            for &rhs_width in WIDTHS {
                for &dst_width in WIDTHS {
                    for lhs_signed in [false, true] {
                        for &lhs in VALUES {
                            for &rhs in VALUES {
                                let lhs = lhs & mask_u64(lhs_width);
                                let rhs = rhs & mask_u64(rhs_width);
                                let expected = alu_binary(
                                    op,
                                    &SIRValue::new(lhs),
                                    &SIRValue::new(rhs),
                                    lhs_width,
                                    rhs_width,
                                    dst_width,
                                    lhs_signed,
                                    false,
                                )
                                .unwrap();
                                let actual = alu_binary_u64(
                                    op, lhs, rhs, lhs_width, rhs_width, dst_width, lhs_signed,
                                );
                                assert!(expected.mask.is_zero());
                                assert_eq!(
                                    BigUint::from(actual),
                                    expected.payload,
                                    "{op:?}: lhs={lhs:#x}/{lhs_width}, rhs={rhs:#x}/{rhs_width}, dst={dst_width}, signed={lhs_signed}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn narrow_unary_alu_matches_generic_two_state_path() {
    const OPS: &[UnaryOp] = &[
        UnaryOp::Ident,
        UnaryOp::ToTwoState,
        UnaryOp::Minus,
        UnaryOp::BitNot,
        UnaryOp::LogicNot,
        UnaryOp::And,
        UnaryOp::Or,
        UnaryOp::Xor,
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ];
    const WIDTHS: &[usize] = &[1, 4, 8, 32, 63, 64];
    const VALUES: &[u64] = &[
        0,
        1,
        2,
        3,
        7,
        0x80,
        0xffff_ffff,
        0x8000_0000_0000_0000,
        u64::MAX,
    ];

    for op in OPS {
        for &src_width in WIDTHS {
            for &dst_width in WIDTHS {
                for src_signed in [false, true] {
                    for &src in VALUES {
                        let src = src & mask_u64(src_width);
                        let expected =
                            alu_unary(op, &SIRValue::new(src), src_width, src_signed, dst_width)
                                .unwrap();
                        let actual = alu_unary_u64(op, src, src_width, src_signed, dst_width);
                        assert!(expected.mask.is_zero());
                        assert_eq!(
                            BigUint::from(actual),
                            expected.payload,
                            "{op:?}: src={src:#x}/{src_width}, dst={dst_width}, signed={src_signed}",
                        );
                    }
                }
            }
        }
    }
}

fn block(
    id: usize,
    params: Vec<usize>,
    instructions: Vec<SIRInstruction<u32>>,
    terminator: SIRTerminator,
) -> (BlockId, BasicBlock<u32>) {
    (
        BlockId(id),
        BasicBlock {
            id: BlockId(id),
            params: params.into_iter().map(RegisterId).collect(),
            instructions,
            terminator,
        },
    )
}

const SP: u32 = 1;
const N: u32 = FRAME_SLOTS;
const RESULT: u32 = FRAME_SLOTS + 1;

fn reg(id: usize) -> RegisterId {
    RegisterId(id)
}

fn store(addr: u32, bits: usize, src: usize) -> SIRInstruction<u32> {
    SIRInstruction::Store(
        addr,
        SIROffset::Static(0),
        bits,
        reg(src),
        Vec::new(),
        Vec::new(),
    )
}

fn load(dst: usize, addr: u32, bits: usize) -> SIRInstruction<u32> {
    SIRInstruction::Load(reg(dst), addr, SIROffset::Static(0), bits)
}

/// `fact(n)`: its frame holds `n` and the result, 32 bits each, and it
/// calls itself in a frame at the stack pointer `SP`, as an out-of-line
/// call site does.
fn factorial() -> FunctionUnit<u32> {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            block(
                0,
                vec![],
                vec![
                    load(0, N, 32),
                    SIRInstruction::Imm(reg(1), SIRValue::new(2u8)),
                    SIRInstruction::Binary(reg(2), reg(0), BinaryOp::LtU, reg(1)),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![],
                vec![
                    SIRInstruction::Imm(reg(3), SIRValue::new(1u8)),
                    store(RESULT, 32, 3),
                ],
                SIRTerminator::Return,
            ),
            block(
                2,
                vec![],
                vec![
                    load(4, SP, 32),
                    SIRInstruction::Imm(reg(5), SIRValue::new(8u8)),
                    SIRInstruction::Binary(reg(6), reg(4), BinaryOp::Add, reg(5)),
                    store(SP, 32, 6),
                    SIRInstruction::Imm(reg(7), SIRValue::new(1u8)),
                    SIRInstruction::Binary(reg(8), reg(0), BinaryOp::Sub, reg(7)),
                    SIRInstruction::Call {
                        function: FunctionId(0),
                        frame: reg(4),
                        args: vec![(0, reg(8))],
                        outputs: vec![1],
                        dst: Some(reg(9)),
                        effects: Default::default(),
                    },
                    store(SP, 32, 4),
                    // `n` keeps its value across the call.
                    SIRInstruction::Binary(reg(10), reg(0), BinaryOp::Mul, reg(9)),
                    store(RESULT, 32, 10),
                ],
                SIRTerminator::Return,
            ),
        ]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[
            (0, 32),
            (1, 32),
            (2, 1),
            (3, 32),
            (4, 32),
            (5, 32),
            (6, 32),
            (7, 32),
            (8, 32),
            (9, 32),
            (10, 32),
        ]),
    };
    FunctionUnit {
        unit,
        effects: Default::default(),
        frame: celox_sir::FrameLayout {
            size: 8,
            slots: [N, RESULT]
                .into_iter()
                .enumerate()
                .map(|(index, addr)| celox_sir::FrameSlot {
                    addr,
                    offset: index * 4,
                    width: 32,
                    four_state: false,
                    handle: false,
                    element_width: None,
                })
                .collect(),
        },
    }
}

/// A unit that calls function 0 with `arg` in slot 0 and stores its
/// output slot 1 to address 7, in a frame at `SP`.
fn caller(arg: SIRValue, width: usize, register: RegisterType) -> ExecutionUnit<u32> {
    let mut register_map = bit_regs(&[(0, 32), (1, 32), (2, 32)]);
    register_map.insert(reg(3), register.clone());
    register_map.insert(reg(4), register);
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                load(0, SP, 32),
                SIRInstruction::Imm(reg(1), SIRValue::new(8u8)),
                SIRInstruction::Binary(reg(2), reg(0), BinaryOp::Add, reg(1)),
                store(SP, 32, 2),
                SIRInstruction::Imm(reg(3), arg),
                SIRInstruction::Call {
                    function: FunctionId(0),
                    frame: reg(0),
                    args: vec![(0, reg(3))],
                    outputs: vec![1],
                    dst: Some(reg(4)),
                    effects: Default::default(),
                },
                store(SP, 32, 0),
                store(7, width, 4),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map,
    }
}

#[test]
fn calls_a_recursive_function() {
    let unit = caller(
        SIRValue::new(6u8),
        32,
        RegisterType::Bit {
            width: 32,
            signed: false,
        },
    );
    let mut machine = FakeMachine::default();
    execute_unit_with_functions(&unit, &mut machine, &[], false, &[factorial()]).unwrap();
    assert_eq!(machine.stored(7, 0, 32).payload, BigUint::from(720u32));
    assert!(machine.frames.is_empty());
}

#[test]
fn nests_calls_without_the_host_stack() {
    // As deep as calls may nest, each activation on the explicit stack.
    let bit = RegisterType::Bit {
        width: 32,
        signed: false,
    };
    let unit = caller(SIRValue::new(MAX_CALL_DEPTH as u32), 32, bit.clone());
    let mut machine = FakeMachine::default();
    execute_unit_with_functions(&unit, &mut machine, &[], false, &[factorial()]).unwrap();
    assert!(machine.frames.is_empty());
    // Deeper than the limit fails, leaving no frame entered.
    let unit = caller(SIRValue::new((MAX_CALL_DEPTH + 10) as u32), 32, bit);
    let mut machine = FakeMachine::default();
    let error =
        execute_unit_with_functions(&unit, &mut machine, &[], false, &[factorial()]).unwrap_err();
    assert!(matches!(error, InterpError::Machine(_)), "{error:?}");
    assert!(machine.frames.is_empty());
}

#[test]
fn a_callee_error_ends_the_caller() {
    let mut function = factorial();
    function
        .unit
        .blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .terminator = SIRTerminator::Error(7);
    let unit = caller(
        SIRValue::new(4u8),
        32,
        RegisterType::Bit {
            width: 32,
            signed: false,
        },
    );
    let mut machine = FakeMachine::default();
    let error =
        execute_unit_with_functions(&unit, &mut machine, &[], false, &[function]).unwrap_err();
    assert!(matches!(error, InterpError::Fatal(7)), "{error:?}");
    assert!(machine.frames.is_empty());
    assert!(!machine.cells.contains_key(&(7, 0, 32)));
}

#[test]
fn passes_wide_four_state_values_through_a_frame() {
    // `id(x)` copies its 100-bit four-state slot 0 to slot 1.
    let logic = RegisterType::Logic { width: 100 };
    let identity = FunctionUnit {
        effects: Default::default(),
        unit: ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![load(0, N, 100), store(RESULT, 100, 0)],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: [(reg(0), logic.clone())].into_iter().collect(),
        },
        frame: celox_sir::FrameLayout {
            size: 52,
            slots: [N, RESULT]
                .into_iter()
                .enumerate()
                .map(|(index, addr)| celox_sir::FrameSlot {
                    addr,
                    offset: index * 26,
                    width: 100,
                    four_state: true,
                    handle: false,
                    element_width: None,
                })
                .collect(),
        },
    };
    let value = SIRValue::new_four_state(
        (BigUint::from(1u8) << 99u32) | BigUint::from(5u8),
        BigUint::from(1u8) << 70u32,
    );
    let unit = caller(value.clone(), 100, logic);
    let mut machine = FakeMachine::default();
    execute_unit_with_functions(&unit, &mut machine, &[], true, &[identity]).unwrap();
    assert_eq!(machine.stored(7, 0, 100), &value);
}

#[test]
fn executes_straight_line_arithmetic_and_store() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8)),
                SIRInstruction::Binary(RegisterId(2), RegisterId(0), BinaryOp::Add, RegisterId(1)),
                SIRInstruction::Store(
                    7u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8), (1, 8), (2, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], true).unwrap();
    assert_eq!(machine.stored(7, 0, 8).payload, BigUint::from(8u8));
}

#[test]
fn reused_register_storage_clears_values_between_units() {
    let producer = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xabu8))],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8)]),
    };
    let consumer = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![SIRInstruction::Store(
                1u32,
                SIROffset::Static(0),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8)]),
    };

    let mut machine = FakeMachine::default();
    let mut registers = Registers::new(&producer.register_map, true);
    execute_prepared_unit(&producer, &mut machine, &[], true, &mut registers, &[]).unwrap();
    let error =
        execute_prepared_unit(&consumer, &mut machine, &[], true, &mut registers, &[]).unwrap_err();
    assert_eq!(error, InterpError::MissingRegister(RegisterId(0)));
}

#[test]
fn branch_selects_target_by_known_condition_bits() {
    for (cond, expected) in [(1u8, 10u8), (0, 20)] {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                block(
                    0,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(cond))],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(10u8))],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(
                    2,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(20u8))],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(
                    3,
                    vec![],
                    vec![SIRInstruction::Store(
                        1u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Return,
                ),
            ]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 1), (1, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.stored(1, 0, 8).payload, BigUint::from(expected));
    }
}

#[test]
fn switch_matches_cases_and_falls_back_to_default() {
    for (selector, expected) in [(2u8, 2u8), (9, 99)] {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                block(
                    0,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(selector))],
                    SIRTerminator::Switch {
                        selector: RegisterId(0),
                        cases: vec![
                            SIRSwitchCase {
                                value: BigUint::from(1u8),
                                target: BlockId(1),
                            },
                            SIRSwitchCase {
                                value: BigUint::from(2u8),
                                target: BlockId(2),
                            },
                        ],
                        default: BlockId(3),
                    },
                ),
                block(
                    1,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8))],
                    SIRTerminator::Jump(BlockId(4), vec![]),
                ),
                block(
                    2,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8))],
                    SIRTerminator::Jump(BlockId(4), vec![]),
                ),
                block(
                    3,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(99u8))],
                    SIRTerminator::Jump(BlockId(4), vec![]),
                ),
                block(
                    4,
                    vec![],
                    vec![SIRInstruction::Store(
                        1u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Return,
                ),
            ]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 4), (1, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.stored(1, 0, 8).payload, BigUint::from(expected));
    }
}

#[test]
fn jump_binds_target_block_parameters() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            block(
                0,
                vec![],
                vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8))],
                SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
            ),
            block(
                1,
                vec![1],
                vec![SIRInstruction::Store(
                    3u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Return,
            ),
        ]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8), (1, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], true).unwrap();
    assert_eq!(machine.stored(3, 0, 8).payload, BigUint::from(7u8));
}

#[test]
fn entry_parameters_bind_caller_supplied_arguments() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![0],
            vec![SIRInstruction::Store(
                5u32,
                SIROffset::Static(0),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[SIRValue::new(0xDEu16)], true).unwrap();
    assert_eq!(machine.stored(5, 0, 8).payload, BigUint::from(0xDEu16));
}

#[test]
fn concat_and_slice_roundtrip_msbf_order() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xABu8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xCDu8)),
                SIRInstruction::Concat(RegisterId(2), vec![RegisterId(0), RegisterId(1)]),
                SIRInstruction::Slice(RegisterId(3), RegisterId(2), 4, 8),
                SIRInstruction::Store(
                    9u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8), (1, 8), (2, 16), (3, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], true).unwrap();
    // 0xABCD sliced [4 +: 8] == 0xBC.
    assert_eq!(machine.stored(9, 0, 8).payload, BigUint::from(0xBCu8));
}

#[test]
fn narrow_singleton_64_bit_concat_does_not_shift_by_64() {
    let value = 0xdead_beef_cafe_babeu64;
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(value)),
                SIRInstruction::Concat(RegisterId(1), vec![RegisterId(0)]),
                SIRInstruction::Store(
                    9u32,
                    SIROffset::Static(0),
                    64,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 64), (1, 64)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], false).unwrap();
    assert_eq!(machine.stored(9, 0, 64).payload, BigUint::from(value));
}

#[test]
fn concat_truncates_to_destination_width() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xFFu8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xFFu8)),
                SIRInstruction::Concat(RegisterId(2), vec![RegisterId(0), RegisterId(1)]),
                SIRInstruction::Store(
                    9u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        // Destination is narrower than the concatenated sources: the
        // declared-register-width invariant keeps the low 8 bits.
        register_map: bit_regs(&[(0, 8), (1, 8), (2, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], true).unwrap();
    assert_eq!(machine.stored(9, 0, 8).payload, BigUint::from(0xFFu8));
}

#[test]
fn mux_follows_four_state_selection_contract() {
    let known_one = SIRValue::new(1u8);
    let known_zero = SIRValue::new(0u8);
    let unknown = SIRValue::new_four_state(0u8, 1u8);

    // Known-one condition selects the then-arm exactly, mask included.
    let mixed_arm = SIRValue::new_four_state(0b01u8, 0b10u8);
    assert_eq!(
        eval_mux(&known_one, &mixed_arm, &SIRValue::new(0u8), 1, 2),
        mixed_arm
    );
    // Known-zero condition selects the else-arm.
    assert_eq!(
        eval_mux(&known_zero, &mixed_arm, &SIRValue::new(0b11u8), 1, 2),
        SIRValue::new(0b11u8)
    );
    // Unknown condition: agreeing bits preserved, differing bits become X
    // with a normalized (all-ones) payload.
    let out = eval_mux(
        &unknown,
        &SIRValue::new_four_state(0b1010u8, 0b0000u8),
        &SIRValue::new_four_state(0b0011u8, 0b0100u8),
        1,
        4,
    );
    // difference = 1101 → mask keeps those bits unknown, payload absorbs
    // them so X bits read as one.
    assert_eq!(out.payload, BigUint::from(0b1111u8));
    assert_eq!(out.mask, BigUint::from(0b1101u8));
}

#[test]
fn mux_condition_is_evaluated_in_its_own_width() {
    // An 8-bit condition with a known one above the 4-bit destination
    // width must still select the then-arm.
    let wide_cond = SIRValue::new(0b1_0000u8);
    let out = eval_mux(
        &wide_cond,
        &SIRValue::new(0xAu8),
        &SIRValue::new(0x5u8),
        8,
        4,
    );
    assert_eq!(out.payload, BigUint::from(0xAu8));
}

#[test]
fn error_terminator_surfaces_fatal_code() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(0, vec![], vec![], SIRTerminator::Error(42))]
            .into_iter()
            .collect(),
        register_map: HashMap::default(),
    };

    let mut machine = FakeMachine::default();
    assert_eq!(
        execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
        InterpError::Fatal(42)
    );
}

#[test]
fn jump_to_missing_block_reports_unknown_block() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![],
            SIRTerminator::Jump(BlockId(99), vec![]),
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    };

    let mut machine = FakeMachine::default();
    assert_eq!(
        execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
        InterpError::UnknownBlock(BlockId(99))
    );
}

#[test]
fn arity_mismatch_between_jump_and_target_params_is_rejected() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            block(0, vec![], vec![], SIRTerminator::Jump(BlockId(1), vec![])),
            block(1, vec![0], vec![], SIRTerminator::Return),
        ]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8)]),
    };

    let mut machine = FakeMachine::default();
    assert_eq!(
        execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
        InterpError::RegisterArityMismatch {
            expected: 1,
            found: 0,
        }
    );
}

#[test]
fn reading_unwritten_register_is_rejected() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![SIRInstruction::Store(
                1u32,
                SIROffset::Static(0),
                8,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(4, 8)]),
    };

    let mut machine = FakeMachine::default();
    assert_eq!(
        execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
        InterpError::MissingRegister(RegisterId(4))
    );
}

#[test]
fn runtime_events_receive_resolved_argument_values() {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(11u8)),
                SIRInstruction::RuntimeEvent {
                    site_id: 3,
                    args: vec![RegisterId(0)],
                },
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: bit_regs(&[(0, 8)]),
    };

    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], true).unwrap();
    assert_eq!(machine.runtime_events, vec![(3, vec![SIRValue::new(11u8)])]);
}

#[test]
fn planes_move_value_and_mask_words_both_ways() {
    // X where both planes are set, Z where only the mask is.
    let value = BigUint::from(0xf0e1_d2c3_b4a5_9687_7869_5a4b_3c2d_1e0fu128);
    let mask = BigUint::from(0x00ff_0000_0000_f00f_8000_0000_0000_0003u128);
    let source = SIRValue::new_four_state(value.clone(), mask.clone());
    let planes = alu_unary(&UnaryOp::ToPlanes, &source, 128, false, 256).unwrap();
    assert_eq!(planes, SIRValue::new((&mask << 128usize) | &value));
    let back = alu_unary(&UnaryOp::FromPlanes, &planes, 256, false, 128).unwrap();
    assert_eq!(back, source);
}

#[test]
fn two_state_from_planes_drops_the_mask_plane() {
    // A two-state backend has no mask plane for `FromPlanes` to fill, so
    // `ToPlanes` reads it back as zero.
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![],
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new((1u128 << 64) | 1)),
                SIRInstruction::Unary(RegisterId(1), UnaryOp::FromPlanes, RegisterId(0)),
                SIRInstruction::Unary(RegisterId(2), UnaryOp::ToPlanes, RegisterId(1)),
                SIRInstruction::Store(
                    9u32,
                    SIROffset::Static(0),
                    128,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [
            (
                RegisterId(0),
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            ),
            (RegisterId(1), RegisterType::Logic { width: 64 }),
            (
                RegisterId(2),
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    let mut machine = FakeMachine::default();
    execute_unit(&unit, &mut machine, &[], false).unwrap();
    assert_eq!(machine.stored(9, 0, 128).payload, BigUint::from(1u8));
}
