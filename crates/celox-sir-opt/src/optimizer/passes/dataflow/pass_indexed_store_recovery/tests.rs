use super::*;
use crate::ir::{InstanceId, STABLE_REGION, WORKING_REGION};
use celox_design::StateObjectId as VarId;

const SELECTOR_WIDTH: usize = 3;
const ELEMENT_WIDTH: usize = 64;
const ELEMENT_COUNT: usize = 1 << SELECTOR_WIDTH;

fn address(region: u32, raw: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(raw),
    }
}

fn destination() -> RegionedAbsoluteAddr {
    address(WORKING_REGION, 1)
}

fn data_address() -> RegionedAbsoluteAddr {
    address(STABLE_REGION, 2)
}

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn logic(width: usize) -> RegisterType {
    RegisterType::Logic { width }
}

struct Fixture {
    eu: ExecutionUnit<RegionedAbsoluteAddr>,
    selector: RegisterId,
    arms: Vec<BlockId>,
}

struct Registers {
    next: usize,
    types: HashMap<RegisterId, RegisterType>,
}

impl Registers {
    fn new() -> Self {
        Self {
            next: 0,
            types: HashMap::default(),
        }
    }

    fn alloc(&mut self, register_type: RegisterType) -> RegisterId {
        let register = RegisterId(self.next);
        self.next += 1;
        self.types.insert(register, register_type);
        register
    }
}

fn fixture(stage_count: usize) -> Fixture {
    fixture_ladders(&[stage_count])
}

fn fixture_ladders(stage_counts: &[usize]) -> Fixture {
    assert!(!stage_counts.is_empty());
    assert!(stage_counts.iter().all(|&count| count <= ELEMENT_COUNT));
    let mut registers = Registers::new();
    let selector = registers.alloc(logic(SELECTOR_WIDTH));
    let mask = registers.alloc(logic(SELECTOR_WIDTH));
    let mut blocks = HashMap::default();
    let mut arms = Vec::new();
    let mut block_base = 0usize;

    for (ladder_index, &stage_count) in stage_counts.iter().enumerate() {
        let final_block = BlockId(block_base + stage_count * 3);
        for key_value in 0..stage_count {
            let decision = BlockId(block_base + key_value * 3);
            let arm = BlockId(block_base + key_value * 3 + 1);
            let empty = BlockId(block_base + key_value * 3 + 2);
            let next = if key_value + 1 == stage_count {
                final_block
            } else {
                BlockId(block_base + (key_value + 1) * 3)
            };
            arms.push(arm);

            let key = registers.alloc(logic(SELECTOR_WIDTH));
            let masked = registers.alloc(logic(SELECTOR_WIDTH));
            let alias = registers.alloc(logic(SELECTOR_WIDTH));
            let equality = registers.alloc(logic(1));
            let condition = registers.alloc(bit(1));
            let mut decision_instructions = Vec::new();
            if ladder_index == 0 && key_value == 0 {
                decision_instructions.push(SIRInstruction::Imm(
                    mask,
                    SIRValue::new((ELEMENT_COUNT - 1) as u64),
                ));
            }
            decision_instructions.extend([
                SIRInstruction::Imm(key, SIRValue::new(key_value as u64)),
                SIRInstruction::Binary(masked, key, BinaryOp::And, mask),
                SIRInstruction::Unary(alias, UnaryOp::Ident, masked),
                SIRInstruction::Binary(equality, selector, BinaryOp::Eq, alias),
                SIRInstruction::Unary(condition, UnaryOp::ToTwoState, equality),
            ]);
            blocks.insert(
                decision,
                BasicBlock {
                    id: decision,
                    params: if ladder_index == 0 && key_value == 0 {
                        vec![selector]
                    } else {
                        Vec::new()
                    },
                    instructions: decision_instructions,
                    terminator: SIRTerminator::Branch {
                        cond: condition,
                        true_block: (arm, Vec::new()),
                        false_block: (empty, Vec::new()),
                    },
                },
            );

            let value = registers.alloc(logic(ELEMENT_WIDTH));
            blocks.insert(
                arm,
                BasicBlock {
                    id: arm,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(
                            value,
                            data_address(),
                            SIROffset::Static(0),
                            ELEMENT_WIDTH,
                        ),
                        SIRInstruction::Store(
                            destination(),
                            SIROffset::Static(key_value * ELEMENT_WIDTH),
                            ELEMENT_WIDTH,
                            value,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Jump(next, Vec::new()),
                },
            );
            blocks.insert(
                empty,
                BasicBlock {
                    id: empty,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(next, Vec::new()),
                },
            );
        }
        let next_ladder = BlockId(final_block.0 + 1);
        blocks.insert(
            final_block,
            BasicBlock {
                id: final_block,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: if ladder_index + 1 == stage_counts.len() {
                    SIRTerminator::Return
                } else {
                    SIRTerminator::Jump(next_ladder, Vec::new())
                },
            },
        );
        block_base = next_ladder.0;
    }

    let eu = ExecutionUnit {
        blocks,
        entry_block_id: BlockId(0),
        register_map: registers.types,
    };
    eu.verify_result().unwrap();
    Fixture { eu, selector, arms }
}

fn pass() -> IndexedStoreRecoveryPass {
    IndexedStoreRecoveryPass {
        arrays: [(
            destination().absolute_addr(),
            ArrayShape {
                element_width: ELEMENT_WIDTH,
                element_count: ELEMENT_COUNT,
            },
        )]
        .into_iter()
        .collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Write {
    address: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
    value: u64,
}

fn execute(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    selector: RegisterId,
    value: u64,
) -> Vec<Write> {
    let mut registers = HashMap::default();
    registers.insert(selector, value);
    let mut writes = Vec::new();
    let mut current = eu.entry_block_id;
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps <= eu.blocks.len() + 1, "test SIR did not terminate");
        let block = &eu.blocks[&current];
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(destination, constant) => {
                    registers.insert(*destination, constant.payload.to_u64().unwrap_or(0));
                }
                SIRInstruction::Binary(destination, lhs, operation, rhs) => {
                    let lhs = registers[lhs];
                    let rhs = registers[rhs];
                    let result = match operation {
                        BinaryOp::And => lhs & rhs,
                        BinaryOp::Eq => u64::from(lhs == rhs),
                        other => panic!("unsupported test binary operation {other:?}"),
                    };
                    registers.insert(*destination, result);
                }
                SIRInstruction::Unary(destination, operation, source) => {
                    assert!(matches!(operation, UnaryOp::Ident | UnaryOp::ToTwoState));
                    registers.insert(*destination, registers[source]);
                }
                SIRInstruction::Load(destination, _, SIROffset::Static(0), width) => {
                    assert_eq!(*width, ELEMENT_WIDTH);
                    registers.insert(*destination, 0x5a5a_1234_dead_beef);
                }
                SIRInstruction::Store(address, offset, width, source, triggers, captures) => {
                    assert!(triggers.is_empty() && captures.is_empty());
                    let offset = match offset {
                        SIROffset::Static(offset) => *offset,
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset: None,
                        } => registers[index] as usize * element_width + bit_offset,
                        other => panic!("unsupported test store offset {other:?}"),
                    };
                    writes.push(Write {
                        address: *address,
                        offset,
                        width: *width,
                        value: registers[source],
                    });
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => {
                assert!(arguments.is_empty());
                current = *target;
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let target = if registers[cond] == 0 {
                    false_block
                } else {
                    true_block
                };
                assert!(target.1.is_empty());
                current = target.0;
            }
            SIRTerminator::Switch { .. } => {
                panic!("unexpected Switch in indexed-store test")
            }
            SIRTerminator::Return => return writes,
            SIRTerminator::Error(code) => panic!("unexpected Error({code})"),
        }
    }
}

#[test]
fn recovers_full_domain_unpacked_array_store_and_preserves_every_selector() {
    let fixture = fixture(ELEMENT_COUNT);
    let original = fixture.eu.clone();
    let mut optimized = fixture.eu;

    pass().run(&mut optimized, &PassOptions::default());

    optimized.verify_result().unwrap();
    for selector_value in 0..ELEMENT_COUNT as u64 {
        assert_eq!(
            execute(&original, fixture.selector, selector_value),
            execute(&optimized, fixture.selector, selector_value),
            "selector={selector_value}"
        );
    }
    assert_eq!(optimized.blocks.len(), 2);
    assert_eq!(
        optimized
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count(),
        0
    );
    let stores = optimized
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, offset, width, _, _, _) => {
                Some((*address, offset, *width))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].0, destination());
    assert_eq!(stores[0].2, ELEMENT_WIDTH);
    assert!(matches!(
        stores[0].1,
        SIROffset::Element {
            index,
            element_width: ELEMENT_WIDTH,
            bit_offset: 0,
            dynamic_bit_offset: None,
        } if *index == fixture.selector
    ));
}

#[test]
fn recovers_multiple_disjoint_ladders_from_one_cfg_analysis() {
    let fixture = fixture_ladders(&[ELEMENT_COUNT, ELEMENT_COUNT]);
    let original = fixture.eu.clone();
    let mut optimized = fixture.eu;

    pass().run(&mut optimized, &PassOptions::default());

    optimized.verify_result().unwrap();
    for selector_value in 0..ELEMENT_COUNT as u64 {
        assert_eq!(
            execute(&original, fixture.selector, selector_value),
            execute(&optimized, fixture.selector, selector_value),
            "selector={selector_value}"
        );
    }
    assert_eq!(
        optimized
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count(),
        0
    );
    assert_eq!(
        optimized
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count(),
        2
    );
}

#[test]
fn leaves_an_incomplete_selector_domain_unchanged() {
    let mut fixture = fixture(ELEMENT_COUNT - 1);
    let original_blocks = fixture.eu.blocks.len();

    pass().run(&mut fixture.eu, &PassOptions::default());

    fixture.eu.verify_result().unwrap();
    assert_eq!(fixture.eu.blocks.len(), original_blocks);
    assert!(
        fixture
            .eu
            .blocks
            .values()
            .any(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
    );
}

#[test]
fn leaves_non_equivalent_value_arms_unchanged() {
    let mut fixture = fixture(ELEMENT_COUNT);
    let arm = fixture.arms[3];
    let SIRInstruction::Load(_, source, _, _) =
        &mut fixture.eu.blocks.get_mut(&arm).unwrap().instructions[0]
    else {
        unreachable!();
    };
    *source = address(STABLE_REGION, 99);
    let original_blocks = fixture.eu.blocks.len();

    pass().run(&mut fixture.eu, &PassOptions::default());

    fixture.eu.verify_result().unwrap();
    assert_eq!(fixture.eu.blocks.len(), original_blocks);
}

#[test]
fn leaves_an_observable_arm_effect_unchanged() {
    let mut fixture = fixture(ELEMENT_COUNT);
    let arm = fixture.arms[4];
    let value = match fixture.eu.blocks[&arm].instructions[0] {
        SIRInstruction::Load(value, ..) => value,
        _ => unreachable!(),
    };
    fixture
        .eu
        .blocks
        .get_mut(&arm)
        .unwrap()
        .instructions
        .insert(
            1,
            SIRInstruction::RuntimeEvent {
                site_id: 17,
                args: vec![value],
            },
        );
    let original_blocks = fixture.eu.blocks.len();

    pass().run(&mut fixture.eu, &PassOptions::default());

    fixture.eu.verify_result().unwrap();
    assert_eq!(fixture.eu.blocks.len(), original_blocks);
}

#[test]
fn leaves_four_state_equality_ladder_unchanged() {
    let mut fixture = fixture(ELEMENT_COUNT);
    let original_blocks = fixture.eu.blocks.len();
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };

    pass().run(&mut fixture.eu, &options);

    fixture.eu.verify_result().unwrap();
    assert_eq!(fixture.eu.blocks.len(), original_blocks);
}
