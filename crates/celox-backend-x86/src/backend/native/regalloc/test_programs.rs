//! Executable test programs for register allocation: a MIR builder, a
//! generator of structured random MIR, and a reference interpreter for the
//! MIR subset they produce.

use super::{RegallocResult, RegallocStrategy, run_regalloc_for_codegen, verify_assignment};
use crate::native::features::X86Features;
use crate::native::mir::*;
use crate::native::{emit, jit_mem};

pub(super) const INPUT_WORDS: usize = 8;
pub(super) const OUTPUT_WORDS: usize = 24;
pub(super) const STATE_BYTES: usize = (INPUT_WORDS + OUTPUT_WORDS) * 8;

pub(super) fn verified() -> crate::NativeDiagnostics {
    crate::NativeDiagnostics {
        verify_regalloc: true,
        ..crate::NativeDiagnostics::default()
    }
}

pub(super) fn execute(func: &MFunction, result: &RegallocResult, input: &[u8]) -> (i64, Vec<u8>) {
    let emitted = emit::emit(func, &result.assignment, result.spill_frame_size)
        .unwrap_or_else(|error| panic!("emission failed: {error}\n{func}"));
    let jit = jit_mem::JitCode::new(&emitted.code).expect("jit allocation failed");
    let mut state = input.to_vec();
    let status = unsafe { jit.call(&mut state) };
    (status, state)
}

/// Allocate a copy of `func` with every allocator verifier enabled.
pub(super) fn allocate(
    func: &MFunction,
    strategy: RegallocStrategy,
) -> (MFunction, RegallocResult) {
    let mut allocated = func.clone();
    let result = run_regalloc_for_codegen(
        &mut allocated,
        "regalloc_test",
        None,
        &verified(),
        false,
        strategy,
        || false,
    )
    .unwrap_or_else(|error| panic!("{strategy:?} allocation failed: {error}\n{func}"));
    verify_assignment(&allocated, &result.assignment).unwrap();
    assert_eq!(allocated.verify_result(), Ok(()));
    (allocated, result)
}

/// Reference semantics of the generated MIR subset, phis included.
pub(super) fn interpret(func: &MFunction, input: &[u8]) -> Vec<u8> {
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

pub(super) struct Builder {
    pub(super) vregs: VRegAllocator,
    pub(super) descs: Vec<SpillDesc>,
    pub(super) blocks: Vec<MBlock>,
    pub(super) current: usize,
    pub(super) tables: Vec<Vec<u64>>,
}

impl Builder {
    pub(super) fn new() -> Self {
        Self {
            vregs: VRegAllocator::new(),
            descs: Vec::new(),
            blocks: vec![MBlock::new(BlockId(0))],
            current: 0,
            tables: Vec::new(),
        }
    }

    pub(super) fn value(&mut self) -> VReg {
        self.descs.push(SpillDesc::transient());
        self.vregs.alloc()
    }

    pub(super) fn block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(MBlock::new(id));
        id
    }

    pub(super) fn push(&mut self, inst: MInst) {
        self.blocks[self.current].push(inst);
    }

    pub(super) fn enter(&mut self, block: BlockId) {
        self.current = block.0 as usize;
    }

    pub(super) fn current(&self) -> BlockId {
        BlockId(self.current as u32)
    }

    pub(super) fn def(&mut self, make: impl FnOnce(VReg) -> MInst) -> VReg {
        let dst = self.value();
        self.push(make(dst));
        dst
    }

    pub(super) fn imm(&mut self, value: u64) -> VReg {
        self.def(|dst| MInst::LoadImm { dst, value })
    }

    pub(super) fn input(&mut self, word: usize) -> VReg {
        self.def(|dst| MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset: (word * 8) as i32,
            size: OpSize::S64,
        })
    }

    pub(super) fn output(&mut self, word: usize, src: VReg) {
        self.push(MInst::Store {
            base: BaseReg::SimState,
            offset: ((INPUT_WORDS + word) * 8) as i32,
            src,
            size: OpSize::S64,
        });
    }

    pub(super) fn phi(&mut self, block: BlockId, sources: Vec<(BlockId, VReg)>) -> VReg {
        let dst = self.value();
        self.blocks[block.0 as usize]
            .phis
            .push(PhiNode { dst, sources });
        dst
    }

    pub(super) fn finish(self, features: X86Features) -> MFunction {
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

pub(super) fn random_function(seed: u64, features: X86Features) -> MFunction {
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

pub(super) fn random_inputs(seed: u64) -> Vec<Vec<u8>> {
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

pub(super) fn builder_clone(builder: &Builder) -> Builder {
    Builder {
        vregs: builder.vregs.clone(),
        descs: builder.descs.clone(),
        blocks: builder.blocks.clone(),
        current: builder.current,
        tables: builder.tables.clone(),
    }
}

/// Parse the MIR text of the instruction subset [`interpret`] models, in the
/// form `MFunction`'s `Display` prints it, so regression tests can keep a
/// minimized program verbatim.
pub(super) fn parse_mir(text: &str, features: X86Features) -> MFunction {
    fn vreg(token: &str) -> VReg {
        let token = token.trim().trim_end_matches(',');
        VReg(
            token
                .strip_prefix('v')
                .unwrap_or_else(|| panic!("expected a VReg, found {token}"))
                .parse()
                .unwrap(),
        )
    }
    fn block(token: &str) -> BlockId {
        let token = token.trim().trim_end_matches([',', ':']);
        BlockId(
            token
                .strip_prefix("bb")
                .unwrap_or_else(|| panic!("expected a block, found {token}"))
                .parse()
                .unwrap(),
        )
    }
    fn number(token: &str) -> u64 {
        let token = token.trim();
        match token.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).unwrap(),
            None => token.parse().unwrap(),
        }
    }
    fn offset(operand: &str) -> i32 {
        let inner = operand
            .trim()
            .trim_start_matches("[sim + ")
            .trim_end_matches([']', ',']);
        inner.parse().unwrap()
    }
    let mut blocks = Vec::<MBlock>::new();
    let mut values = 0u32;
    let mut note = |value: VReg| values = values.max(value.0 + 1);
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(label) = line.strip_suffix(':') {
            blocks.push(MBlock::new(block(label)));
            continue;
        }
        let current = blocks
            .last_mut()
            .expect("instruction before the first block");
        if let Some(rest) = line.strip_prefix("store.i64 ") {
            let (address, source) = rest.split_once("], ").unwrap();
            let src = vreg(source);
            note(src);
            current.push(MInst::Store {
                base: BaseReg::SimState,
                offset: offset(address),
                src,
                size: OpSize::S64,
            });
            continue;
        }
        if let Some(target) = line.strip_prefix("jmp ") {
            current.push(MInst::Jump {
                target: block(target),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("br ") {
            let parts = rest.split(", ").collect::<Vec<_>>();
            let cond = vreg(parts[0]);
            note(cond);
            current.push(MInst::Branch {
                cond,
                true_bb: block(parts[1]),
                false_bb: block(parts[2]),
            });
            continue;
        }
        if line == "ret" {
            current.push(MInst::Return);
            continue;
        }
        let (dst, rest) = line
            .split_once(" = ")
            .unwrap_or_else(|| panic!("unparsed MIR: {line}"));
        let dst = vreg(dst);
        note(dst);
        if let Some(sources) = rest
            .strip_prefix("phi(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let sources = sources
                .split(", ")
                .map(|source| {
                    let (predecessor, value) = source.split_once(": ").unwrap();
                    (block(predecessor), vreg(value))
                })
                .collect::<Vec<_>>();
            for &(_, value) in &sources {
                note(value);
            }
            current.phis.push(PhiNode { dst, sources });
            continue;
        }
        let (opcode, operands) = rest.split_once(' ').unwrap();
        let operands = operands.split(", ").collect::<Vec<_>>();
        let is_value = |operand: &str| operand.trim().starts_with('v');
        let used = operands
            .iter()
            .copied()
            .filter(|operand| is_value(operand))
            .map(vreg)
            .collect::<Vec<_>>();
        for &value in &used {
            note(value);
        }
        let inst = match (opcode, operands.as_slice()) {
            ("imm", [value]) => MInst::LoadImm {
                dst,
                value: number(value),
            },
            ("load.i64", [address]) => MInst::Load {
                dst,
                base: BaseReg::SimState,
                offset: offset(address),
                size: OpSize::S64,
            },
            ("mov.w64", [src]) => MInst::Mov {
                dst,
                src: vreg(src),
            },
            ("not", [src]) => MInst::BitNot {
                dst,
                src: vreg(src),
            },
            ("select", [cond, true_val, false_val]) => MInst::Select {
                dst,
                cond: vreg(cond),
                true_val: vreg(true_val),
                false_val: vreg(false_val),
            },
            ("cmp.LtU", [lhs, rhs]) if is_value(rhs) => MInst::Cmp {
                dst,
                lhs: vreg(lhs),
                rhs: vreg(rhs),
                kind: CmpKind::LtU,
            },
            ("cmp.LtU", [lhs, imm]) => MInst::CmpImm {
                dst,
                lhs: vreg(lhs),
                imm: number(imm) as i32,
                kind: CmpKind::LtU,
            },
            (opcode, [lhs, rhs]) if is_value(rhs) => {
                let (lhs, rhs) = (vreg(lhs), vreg(rhs));
                match opcode {
                    "add.w64" => MInst::Add { dst, lhs, rhs },
                    "sub.w64" => MInst::Sub { dst, lhs, rhs },
                    "mul.w64" => MInst::Mul { dst, lhs, rhs },
                    "and.w64" => MInst::And { dst, lhs, rhs },
                    "or.w64" => MInst::Or { dst, lhs, rhs },
                    "xor.w64" => MInst::Xor { dst, lhs, rhs },
                    "shl" => MInst::Shl { dst, lhs, rhs },
                    "shr" => MInst::Shr { dst, lhs, rhs },
                    "sar" => MInst::Sar { dst, lhs, rhs },
                    "udiv" => MInst::UDiv { dst, lhs, rhs },
                    "urem" => MInst::URem { dst, lhs, rhs },
                    _ => panic!("unparsed MIR: {line}"),
                }
            }
            (opcode, [src, imm]) => {
                let (src, imm) = (vreg(src), number(imm));
                match opcode {
                    "add" => MInst::AddImm {
                        dst,
                        src,
                        imm: imm as i32,
                    },
                    "and.w64" => MInst::AndImm { dst, src, imm },
                    "or" => MInst::OrImm { dst, src, imm },
                    "shr" => MInst::ShrImm {
                        dst,
                        src,
                        imm: imm as u8,
                    },
                    _ => panic!("unparsed MIR: {line}"),
                }
            }
            _ => panic!("unparsed MIR: {line}"),
        };
        current.push(inst);
    }
    let mut vregs = VRegAllocator::new();
    while vregs.count() < values {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); values as usize]);
    func.target_features = features;
    for block in blocks {
        func.push_block(block);
    }
    assert_eq!(func.verify_result(), Ok(()), "{func}");
    func
}
