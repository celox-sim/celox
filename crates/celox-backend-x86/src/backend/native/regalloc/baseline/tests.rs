use super::super::{
    RegallocResult, RegallocStrategy, is_cancellation, run_regalloc_for_codegen, verify_assignment,
};
use crate::native::features::{StateBaseStrategy, X86Features};
use crate::native::mir::*;
use crate::native::{emit, jit_mem};

const BASELINE: RegallocStrategy = RegallocStrategy::Baseline { spill_budget: 4096 };
const INPUT_WORDS: usize = 8;
const OUTPUT_WORDS: usize = 24;
const STATE_BYTES: usize = (INPUT_WORDS + OUTPUT_WORDS) * 8;

fn verified() -> crate::NativeDiagnostics {
    crate::NativeDiagnostics {
        verify_regalloc: true,
        ..crate::NativeDiagnostics::default()
    }
}

fn allocate(func: &MFunction, strategy: RegallocStrategy) -> (MFunction, RegallocResult) {
    let mut allocated = func.clone();
    let result = run_regalloc_for_codegen(
        &mut allocated,
        "baseline_test",
        None,
        &verified(),
        false,
        strategy,
        || false,
    )
    .unwrap_or_else(|error| panic!("{strategy:?} allocation failed: {error}\n{func}"));
    verify_assignment(&allocated, &result.assignment).unwrap();
    assert_eq!(allocated.verify_result(), Ok(()));
    if strategy == BASELINE {
        assert!(
            allocated.blocks.iter().all(|block| block.phis.is_empty()),
            "baseline allocation lowers every phi"
        );
    }
    (allocated, result)
}

fn execute(func: &MFunction, result: &RegallocResult, input: &[u8]) -> (i64, Vec<u8>) {
    let emitted = emit::emit(func, &result.assignment, result.spill_frame_size)
        .unwrap_or_else(|error| panic!("emission failed: {error}\n{func}"));
    let jit = jit_mem::JitCode::new(&emitted.code).expect("jit allocation failed");
    let mut state = input.to_vec();
    let status = unsafe { jit.call(&mut state) };
    (status, state)
}

/// Reference semantics of the generated MIR subset, phis included.
fn interpret(func: &MFunction, input: &[u8]) -> Vec<u8> {
    let mut state = input.to_vec();
    let mut values = vec![0u64; func.vregs.count() as usize];
    let index = |id: BlockId| func.blocks.iter().position(|block| block.id == id).unwrap();
    let word = |state: &[u8], offset: i32| {
        let offset = offset as usize;
        u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap())
    };
    let (mut block, mut previous) = (0, None::<BlockId>);
    loop {
        if let Some(predecessor) = previous {
            let incoming = func.blocks[block]
                .phis
                .iter()
                .map(|node| {
                    let (_, source) = node
                        .sources
                        .iter()
                        .find(|(candidate, _)| *candidate == predecessor)
                        .unwrap();
                    (node.dst, values[source.0 as usize])
                })
                .collect::<Vec<_>>();
            for (dst, value) in incoming {
                values[dst.0 as usize] = value;
            }
        }
        for inst in &func.blocks[block].insts {
            let v = |value: VReg| values[value.0 as usize];
            let result = match *inst {
                MInst::LoadImm { value, .. } => value,
                MInst::Load { offset, .. } => word(&state, offset),
                MInst::Store { offset, src, .. } => {
                    let offset = offset as usize;
                    state[offset..offset + 8].copy_from_slice(&v(src).to_le_bytes());
                    continue;
                }
                MInst::Add { lhs, rhs, .. } => v(lhs).wrapping_add(v(rhs)),
                MInst::Sub { lhs, rhs, .. } => v(lhs).wrapping_sub(v(rhs)),
                MInst::Xor { lhs, rhs, .. } => v(lhs) ^ v(rhs),
                MInst::And { lhs, rhs, .. } => v(lhs) & v(rhs),
                MInst::Or { lhs, rhs, .. } => v(lhs) | v(rhs),
                MInst::Mul { lhs, rhs, .. } => v(lhs).wrapping_mul(v(rhs)),
                MInst::Shl { lhs, rhs, .. } => v(lhs) << (v(rhs) & 63),
                MInst::Shr { lhs, rhs, .. } => v(lhs) >> (v(rhs) & 63),
                MInst::Sar { lhs, rhs, .. } => ((v(lhs) as i64) >> (v(rhs) & 63)) as u64,
                MInst::UDiv { lhs, rhs, .. } => v(lhs) / v(rhs),
                MInst::URem { lhs, rhs, .. } => v(lhs) % v(rhs),
                MInst::Cmp {
                    lhs,
                    rhs,
                    kind: CmpKind::LtU,
                    ..
                } => u64::from(v(lhs) < v(rhs)),
                MInst::CmpImm {
                    lhs,
                    imm,
                    kind: CmpKind::LtU,
                    ..
                } => u64::from(v(lhs) < imm as i64 as u64),
                MInst::Select {
                    cond,
                    true_val,
                    false_val,
                    ..
                } => {
                    if v(cond) != 0 {
                        v(true_val)
                    } else {
                        v(false_val)
                    }
                }
                MInst::AndImm { src, imm, .. } => v(src) & imm,
                MInst::OrImm { src, imm, .. } => v(src) | imm,
                MInst::AddImm { src, imm, .. } => v(src).wrapping_add(imm as i64 as u64),
                MInst::ShrImm { src, imm, .. } => v(src) >> imm,
                MInst::BitNot { src, .. } => !v(src),
                MInst::Mov { src, .. } => v(src),
                MInst::Branch {
                    cond,
                    true_bb,
                    false_bb,
                } => {
                    previous = Some(func.blocks[block].id);
                    block = index(if v(cond) != 0 { true_bb } else { false_bb });
                    break;
                }
                MInst::Jump { target } => {
                    previous = Some(func.blocks[block].id);
                    block = index(target);
                    break;
                }
                MInst::Return => return state,
                ref other => panic!("interpreter does not model {other}"),
            };
            values[inst.def().unwrap().0 as usize] = result;
        }
    }
}

/// Allocate `func` with the baseline strategy and require the interpreter's
/// effects from its machine code.
fn assert_matches_reference(func: &MFunction, inputs: &[Vec<u8>], context: &str) {
    let (baseline, baseline_result) = allocate(func, BASELINE);
    for input in inputs {
        let expected = (0, interpret(func, input));
        assert_eq!(
            execute(&baseline, &baseline_result, input),
            expected,
            "baseline result differs ({context})\ninput MIR:\n{func}\nbaseline MIR:\n{baseline}"
        );
    }
}

struct Builder {
    vregs: VRegAllocator,
    descs: Vec<SpillDesc>,
    blocks: Vec<MBlock>,
    current: usize,
    tables: Vec<Vec<u64>>,
}

impl Builder {
    fn new() -> Self {
        Self {
            vregs: VRegAllocator::new(),
            descs: Vec::new(),
            blocks: vec![MBlock::new(BlockId(0))],
            current: 0,
            tables: Vec::new(),
        }
    }

    fn value(&mut self) -> VReg {
        self.descs.push(SpillDesc::transient());
        self.vregs.alloc()
    }

    fn block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(MBlock::new(id));
        id
    }

    fn push(&mut self, inst: MInst) {
        self.blocks[self.current].push(inst);
    }

    fn enter(&mut self, block: BlockId) {
        self.current = block.0 as usize;
    }

    fn current(&self) -> BlockId {
        BlockId(self.current as u32)
    }

    fn def(&mut self, make: impl FnOnce(VReg) -> MInst) -> VReg {
        let dst = self.value();
        self.push(make(dst));
        dst
    }

    fn imm(&mut self, value: u64) -> VReg {
        self.def(|dst| MInst::LoadImm { dst, value })
    }

    fn input(&mut self, word: usize) -> VReg {
        self.def(|dst| MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset: (word * 8) as i32,
            size: OpSize::S64,
        })
    }

    fn output(&mut self, word: usize, src: VReg) {
        self.push(MInst::Store {
            base: BaseReg::SimState,
            offset: ((INPUT_WORDS + word) * 8) as i32,
            src,
            size: OpSize::S64,
        });
    }

    fn phi(&mut self, block: BlockId, sources: Vec<(BlockId, VReg)>) -> VReg {
        let dst = self.value();
        self.blocks[block.0 as usize]
            .phis
            .push(PhiNode { dst, sources });
        dst
    }

    fn finish(self, features: X86Features) -> MFunction {
        let mut func = MFunction::new(self.vregs, self.descs);
        func.target_features = features;
        for table in self.tables {
            func.intern_constant_table(table);
        }
        for block in self.blocks {
            func.push_block(block);
        }
        assert_eq!(func.verify_result(), Ok(()), "{func}");
        func
    }
}

/// Deterministic generator of structured random MIR: straight-line ALU
/// code, diamonds with phis, and bounded loops with carried (and swapped)
/// phis, reading inputs from and writing outputs to simulation state.
struct Generator {
    seed: u64,
    builder: Builder,
}

impl Generator {
    fn next(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick(&mut self, values: &[VReg]) -> VReg {
        values[self.below(values.len())]
    }

    fn operations(&mut self, count: usize, live: &mut Vec<VReg>) {
        for _ in 0..count {
            let lhs = self.pick(live);
            let rhs = self.pick(live);
            let other = self.pick(live);
            let choice = self.next() % 16;
            let variant = self.next() % 3;
            let random = self.next();
            let input = self.below(INPUT_WORDS);
            let output = self.below(OUTPUT_WORDS);
            let b = &mut self.builder;
            let value = match choice {
                0 => b.imm(random),
                1 => b.input(input),
                2 => b.def(|dst| MInst::Add { dst, lhs, rhs }),
                3 => b.def(|dst| MInst::Sub { dst, lhs, rhs }),
                4 => b.def(|dst| MInst::Xor { dst, lhs, rhs }),
                5 => b.def(|dst| MInst::And { dst, lhs, rhs }),
                6 => b.def(|dst| MInst::Or { dst, lhs, rhs }),
                7 => b.def(|dst| MInst::Mul { dst, lhs, rhs }),
                8 => {
                    let count = b.def(|dst| MInst::AndImm {
                        dst,
                        src: rhs,
                        imm: 63,
                    });
                    match variant {
                        0 => b.def(|dst| MInst::Shl {
                            dst,
                            lhs,
                            rhs: count,
                        }),
                        1 => b.def(|dst| MInst::Shr {
                            dst,
                            lhs,
                            rhs: count,
                        }),
                        _ => b.def(|dst| MInst::Sar {
                            dst,
                            lhs,
                            rhs: count,
                        }),
                    }
                }
                9 => {
                    let divisor = b.def(|dst| MInst::OrImm {
                        dst,
                        src: rhs,
                        imm: 1,
                    });
                    if variant == 0 {
                        b.def(|dst| MInst::UDiv {
                            dst,
                            lhs,
                            rhs: divisor,
                        })
                    } else {
                        b.def(|dst| MInst::URem {
                            dst,
                            lhs,
                            rhs: divisor,
                        })
                    }
                }
                10 => {
                    let cond = b.def(|dst| MInst::Cmp {
                        dst,
                        lhs,
                        rhs,
                        kind: CmpKind::LtU,
                    });
                    b.def(|dst| MInst::Select {
                        dst,
                        cond,
                        true_val: lhs,
                        false_val: other,
                    })
                }
                11 => {
                    b.output(output, lhs);
                    continue;
                }
                12 => b.def(|dst| MInst::AddImm {
                    dst,
                    src: lhs,
                    imm: 0x1234,
                }),
                13 => b.def(|dst| MInst::ShrImm {
                    dst,
                    src: lhs,
                    imm: 7,
                }),
                14 => b.def(|dst| MInst::BitNot { dst, src: lhs }),
                _ => b.def(|dst| MInst::Mov { dst, src: lhs }),
            };
            live.push(value);
        }
    }

    fn region(&mut self, depth: usize, live: &mut Vec<VReg>) {
        for _ in 0..1 + self.below(3) {
            match if depth == 0 { 0 } else { self.below(4) } {
                0 => {
                    let count = 3 + self.below(16);
                    self.operations(count, live);
                }
                1 => self.diamond(depth, live),
                2 => self.counted_loop(depth, live),
                _ => self.two_entry_cycle(depth, live),
            }
        }
    }

    fn diamond(&mut self, depth: usize, live: &mut Vec<VReg>) {
        let tested = self.pick(live);
        let cond = self.builder.def(|dst| MInst::AndImm {
            dst,
            src: tested,
            imm: 1,
        });
        let (then_block, else_block, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.push(MInst::Branch {
            cond,
            true_bb: then_block,
            false_bb: else_block,
        });
        let mut arms = Vec::new();
        for arm in [then_block, else_block] {
            self.builder.enter(arm);
            let mut arm_live = live.clone();
            self.region(depth - 1, &mut arm_live);
            arms.push((self.builder.current(), arm_live));
            self.builder.push(MInst::Jump { target: join });
        }
        let dominating = live.len();
        for _ in 0..1 + self.below(6) {
            // Some rows merge one value that dominates both arms.
            let (left, right) = if self.next().is_multiple_of(4) {
                let common = self.pick(&live[..dominating]);
                (common, common)
            } else {
                (self.pick(&arms[0].1), self.pick(&arms[1].1))
            };
            let merged = self
                .builder
                .phi(join, vec![(arms[0].0, left), (arms[1].0, right)]);
            live.push(merged);
        }
        self.builder.enter(join);
    }

    fn counted_loop(&mut self, depth: usize, live: &mut Vec<VReg>) {
        let preheader = self.builder.current();
        let zero = self.builder.imm(0);
        let initial = (0..1 + self.below(4))
            .map(|_| self.pick(live))
            .collect::<Vec<_>>();
        let (header, body, exit) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.push(MInst::Jump { target: header });
        // Phi sources from the latch are patched in once the body exists.
        let counter = self.builder.phi(header, vec![(preheader, zero)]);
        let carried = initial
            .iter()
            .map(|&value| self.builder.phi(header, vec![(preheader, value)]))
            .collect::<Vec<_>>();
        self.builder.enter(header);
        self.builder.push(MInst::Jump { target: body });
        self.builder.enter(body);
        let mut body_live = live.clone();
        body_live.push(counter);
        body_live.extend(&carried);
        self.region(depth - 1, &mut body_live);
        let next = self.builder.def(|dst| MInst::AddImm {
            dst,
            src: counter,
            imm: 1,
        });
        let trips = 1 + self.below(4) as i32;
        let again = self.builder.def(|dst| MInst::CmpImm {
            dst,
            lhs: next,
            imm: trips,
            kind: CmpKind::LtU,
        });
        let latch = self.builder.current();
        let mut sources = vec![next];
        for index in 0..carried.len() {
            // Feeding one carried phi from another builds parallel-copy swaps.
            sources.push(if self.next().is_multiple_of(3) {
                carried[(index + 1) % carried.len()]
            } else {
                self.pick(&body_live)
            });
        }
        for (node, source) in self.builder.blocks[header.0 as usize]
            .phis
            .iter_mut()
            .zip(sources)
        {
            node.sources.push((latch, source));
        }
        self.builder.push(MInst::Branch {
            cond: again,
            true_bb: header,
            false_bb: exit,
        });
        self.builder.enter(exit);
        *live = body_live;
    }
}

impl Generator {
    /// An irreducible cycle: the preheader branches into either of two
    /// blocks which jump to each other until a counter runs out. Neither
    /// block dominates the other, so the cycle has no natural-loop header.
    fn two_entry_cycle(&mut self, depth: usize, live: &mut Vec<VReg>) {
        let preheader = self.builder.current();
        let zero = self.builder.imm(0);
        let tested = self.pick(live);
        let cond = self.builder.def(|dst| MInst::AndImm {
            dst,
            src: tested,
            imm: 1,
        });
        let (first, second, exit) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.push(MInst::Branch {
            cond,
            true_bb: first,
            false_bb: second,
        });
        let carried = 1 + self.below(3);
        let initial = (0..carried).map(|_| self.pick(live)).collect::<Vec<_>>();
        let mut heads = Vec::new();
        for block in [first, second] {
            let counter = self.builder.phi(block, vec![(preheader, zero)]);
            let values = initial
                .iter()
                .map(|&value| self.builder.phi(block, vec![(preheader, value)]))
                .collect::<Vec<_>>();
            heads.push((counter, values));
        }
        let mut ends = Vec::new();
        for (index, block) in [first, second].into_iter().enumerate() {
            self.builder.enter(block);
            let mut body_live = live.clone();
            body_live.push(heads[index].0);
            body_live.extend(&heads[index].1);
            self.region(depth - 1, &mut body_live);
            let next = self.builder.def(|dst| MInst::AddImm {
                dst,
                src: heads[index].0,
                imm: 1,
            });
            let sources = (0..carried)
                .map(|_| self.pick(&body_live))
                .collect::<Vec<_>>();
            ends.push((self.builder.current(), next, sources, body_live));
        }
        // Each block's phis take the other block's values on the cycle edge.
        for (index, block) in [first, second].into_iter().enumerate() {
            let (end, next, sources, _) = &ends[1 - index];
            let rows = std::iter::once(*next).chain(sources.iter().copied());
            for (node, source) in self.builder.blocks[block.0 as usize]
                .phis
                .iter_mut()
                .zip(rows)
            {
                node.sources.push((*end, source));
            }
        }
        self.builder.enter(ends[0].0);
        self.builder.push(MInst::Jump { target: second });
        self.builder.enter(ends[1].0);
        let trips = 2 + self.below(4) as i32;
        let again = self.builder.def(|dst| MInst::CmpImm {
            dst,
            lhs: ends[1].1,
            imm: trips,
            kind: CmpKind::LtU,
        });
        self.builder.push(MInst::Branch {
            cond: again,
            true_bb: first,
            false_bb: exit,
        });
        self.builder.enter(exit);
        *live = ends[1].3.clone();
    }
}

fn random_function(seed: u64, features: X86Features) -> MFunction {
    let mut generator = Generator {
        seed: seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1,
        builder: Builder::new(),
    };
    let mut live = (0..INPUT_WORDS)
        .map(|word| generator.builder.input(word))
        .collect::<Vec<_>>();
    generator.region(3, &mut live);
    for word in 0..OUTPUT_WORDS {
        let value = generator.pick(&live);
        generator.builder.output(word, value);
    }
    generator.builder.push(MInst::Return);
    generator.builder.finish(features)
}

fn random_inputs(seed: u64) -> Vec<Vec<u8>> {
    (0..3u64)
        .map(|round| {
            let mut state = vec![0u8; STATE_BYTES];
            for word in 0..INPUT_WORDS {
                let value = (seed ^ round.wrapping_mul(0x5851_f42d))
                    .wrapping_mul(word as u64 * 2 + 1)
                    .rotate_left(word as u32 * 9 + round as u32);
                state[word * 8..word * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            state
        })
        .collect()
}

#[test]
fn random_programs_match_reference_semantics() {
    for seed in 0..120 {
        for bmi2 in [true, false] {
            let func = random_function(seed, X86Features::for_test(bmi2));
            assert_matches_reference(
                &func,
                &random_inputs(seed),
                &format!("seed {seed}, bmi2 {bmi2}"),
            );
        }
    }
}

#[test]
fn random_programs_allocate_with_fifteen_registers() {
    for seed in 0..60 {
        let func = random_function(
            seed,
            X86Features::for_test_with_state_base(false, StateBaseStrategy::Gs),
        );
        allocate(&func, BASELINE);
    }
}

/// A straight-line function with more simultaneously live values than
/// registers, consumed in reverse order so every reload is exercised.
fn pressure_function(features: X86Features) -> MFunction {
    let mut builder = Builder::new();
    let values = (0..40)
        .map(|index| {
            let input = builder.input(index % INPUT_WORDS);
            builder.def(|dst| MInst::AddImm {
                dst,
                src: input,
                imm: index as i32,
            })
        })
        .collect::<Vec<_>>();
    let mut sum = values[39];
    for &value in values[..39].iter().rev() {
        sum = builder.def(|dst| MInst::Xor {
            dst,
            lhs: sum,
            rhs: value,
        });
        let shifted = builder.def(|dst| MInst::Shl {
            dst,
            lhs: sum,
            rhs: value,
        });
        sum = builder.def(|dst| MInst::Add {
            dst,
            lhs: sum,
            rhs: shifted,
        });
    }
    builder.output(0, sum);
    builder.push(MInst::Return);
    builder.finish(features)
}

#[test]
fn high_pressure_block_spills_and_reloads() {
    for bmi2 in [true, false] {
        let func = pressure_function(X86Features::for_test(bmi2));
        let (allocated, result) = allocate(&func, BASELINE);
        assert!(result.spill_frame_size > 0, "40 live values must spill");
        assert!(allocated.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                base: BaseReg::StackFrame,
                ..
            }
        )));
        assert_matches_reference(&func, &random_inputs(7), &format!("bmi2 {bmi2}"));
    }
}

/// Values live across a division must leave RAX/RDX; values live across a
/// whole-file clobber must all leave their registers.
#[test]
fn clobbers_move_or_spill_live_values() {
    let mut builder = Builder::new();
    let values = (0..12)
        .map(|word| builder.input(word % INPUT_WORDS))
        .collect::<Vec<_>>();
    let divisor = builder.def(|dst| MInst::OrImm {
        dst,
        src: values[1],
        imm: 1,
    });
    let quotient = builder.def(|dst| MInst::UDiv {
        dst,
        lhs: values[0],
        rhs: divisor,
    });
    builder.output(0, quotient);
    builder.tables.push(vec![0; SparseCommitDescriptor::WORDS]);
    builder.push(MInst::SparseCommitWorklist {
        descriptor_table: ConstantTableId(0),
        active_bits_offset: 0,
        active_capacity: 1,
    });
    for (word, &value) in values.iter().enumerate() {
        builder.output(word + 1, value);
    }
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let (allocated, result) = allocate(&func, BASELINE);
    // Every value survives the whole-file clobber through a stack slot.
    assert!(result.spill_frame_size >= 8 * 12);
    assert!(allocated.verify_result().is_ok());
}

/// Two loop-carried phis exchange values every iteration; their homes are
/// written as one parallel copy.
#[test]
fn loop_carried_phi_swap_is_a_parallel_copy() {
    let mut builder = Builder::new();
    let a = builder.input(0);
    let b = builder.input(1);
    let zero = builder.imm(0);
    let (header, body, exit) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Jump { target: header });
    let counter = builder.phi(header, vec![(BlockId(0), zero)]);
    let left = builder.phi(header, vec![(BlockId(0), a)]);
    let right = builder.phi(header, vec![(BlockId(0), b)]);
    builder.enter(header);
    builder.push(MInst::Jump { target: body });
    builder.enter(body);
    let next = builder.def(|dst| MInst::AddImm {
        dst,
        src: counter,
        imm: 1,
    });
    let again = builder.def(|dst| MInst::CmpImm {
        dst,
        lhs: next,
        imm: 4,
        kind: CmpKind::LtU,
    });
    for (node, source) in builder.blocks[header.0 as usize]
        .phis
        .iter_mut()
        .zip([next, right, left])
    {
        node.sources.push((body, source));
    }
    builder.push(MInst::Branch {
        cond: again,
        true_bb: header,
        false_bb: exit,
    });
    builder.enter(exit);
    builder.output(0, left);
    builder.output(1, right);
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let (allocated, result) = allocate(&func, BASELINE);
    let mut input = vec![0u8; STATE_BYTES];
    input[..8].copy_from_slice(&11u64.to_le_bytes());
    input[8..16].copy_from_slice(&22u64.to_le_bytes());
    let (status, state) = execute(&allocated, &result, &input);
    assert_eq!(status, 0);
    let word = |index: usize| {
        let offset = (INPUT_WORDS + index) * 8;
        u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap())
    };
    // Four body iterations run three swaps, leaving the pair exchanged.
    assert_eq!((word(0), word(1)), (22, 11));
}

#[test]
fn cancelled_baseline_allocation_reports_cancellation() {
    let func = random_function(3, X86Features::for_test(true));
    let mut allocated = func.clone();
    let error = match run_regalloc_for_codegen(
        &mut allocated,
        "baseline_cancelled",
        None,
        &verified(),
        false,
        BASELINE,
        || true,
    ) {
        Ok(_) => panic!("a cancelled allocation must not return a result"),
        Err(error) => error,
    };
    assert!(is_cancellation(&error));
}

/// A loop whose carried phis rotate: every source is another phi of the
/// header, more of them than registers, so some go through scratch slots.
#[test]
fn wide_phi_rotation_reads_every_source_before_writing_homes() {
    const CARRIED: usize = 20;
    let mut builder = Builder::new();
    let initial = (0..CARRIED)
        .map(|index| {
            let input = builder.input(index % INPUT_WORDS);
            builder.def(|dst| MInst::AddImm {
                dst,
                src: input,
                imm: index as i32,
            })
        })
        .collect::<Vec<_>>();
    let zero = builder.imm(0);
    let (header, body, exit) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Jump { target: header });
    let counter = builder.phi(header, vec![(BlockId(0), zero)]);
    let carried = initial
        .iter()
        .map(|&value| builder.phi(header, vec![(BlockId(0), value)]))
        .collect::<Vec<_>>();
    builder.enter(header);
    builder.push(MInst::Jump { target: body });
    builder.enter(body);
    let next = builder.def(|dst| MInst::AddImm {
        dst,
        src: counter,
        imm: 1,
    });
    let again = builder.def(|dst| MInst::CmpImm {
        dst,
        lhs: next,
        imm: 5,
        kind: CmpKind::LtU,
    });
    let sources = std::iter::once(next)
        .chain((0..CARRIED).map(|index| carried[(index + 1) % CARRIED]))
        .collect::<Vec<_>>();
    for (node, source) in builder.blocks[header.0 as usize]
        .phis
        .iter_mut()
        .zip(sources)
    {
        node.sources.push((body, source));
    }
    builder.push(MInst::Branch {
        cond: again,
        true_bb: header,
        false_bb: exit,
    });
    builder.enter(exit);
    for (word, &value) in carried.iter().enumerate().take(OUTPUT_WORDS) {
        builder.output(word, value);
    }
    builder.push(MInst::Return);
    for bmi2 in [true, false] {
        let func = builder_clone(&builder).finish(X86Features::for_test(bmi2));
        assert_matches_reference(&func, &random_inputs(11), "phi rotation");
    }
}

/// Many phis at one join, each fed by a different constant per arm.
#[test]
fn many_join_phis_store_every_row() {
    const PHIS: usize = 32;
    let mut builder = Builder::new();
    let selector = builder.input(0);
    let cond = builder.def(|dst| MInst::AndImm {
        dst,
        src: selector,
        imm: 1,
    });
    let (left, right, join) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Branch {
        cond,
        true_bb: left,
        false_bb: right,
    });
    let mut arms = Vec::new();
    for (arm, base) in [(left, 1u64), (right, 101)] {
        builder.enter(arm);
        let values = (0..PHIS as u64)
            .map(|index| builder.imm(base + index))
            .collect::<Vec<_>>();
        builder.push(MInst::Jump { target: join });
        arms.push(values);
    }
    let merged = (0..PHIS)
        .map(|index| builder.phi(join, vec![(left, arms[0][index]), (right, arms[1][index])]))
        .collect::<Vec<_>>();
    builder.enter(join);
    let mut sum = merged[0];
    for &value in &merged[1..] {
        sum = builder.def(|dst| MInst::Add {
            dst,
            lhs: sum,
            rhs: value,
        });
    }
    builder.output(0, sum);
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let mut inputs = random_inputs(5);
    inputs[0][0] = 0;
    inputs[1][0] = 1;
    assert_matches_reference(&func, &inputs, "join phis");
}

fn builder_clone(builder: &Builder) -> Builder {
    Builder {
        vregs: builder.vregs.clone(),
        descs: builder.descs.clone(),
        blocks: builder.blocks.clone(),
        current: builder.current,
        tables: builder.tables.clone(),
    }
}
