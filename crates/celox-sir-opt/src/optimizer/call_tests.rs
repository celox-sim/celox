//! Every unit pass on units with function calls between state accesses.
//!
//! A small evaluator runs a unit before and after a pass and compares what
//! is observable: the final STABLE state and what each call reads. A call
//! writes every object its effects list with a value no other instruction
//! produces, so a pass that forwards, merges or moves an access across a
//! call writing its object changes the outcome.

use super::*;
use crate::HashMap;
use crate::ir::{BasicBlock, BlockId, InstanceId, StateObjectId};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) fn object(region: u32, id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}

pub(super) fn stable(id: u32) -> RegionedAbsoluteAddr {
    object(STABLE_REGION, id)
}

pub(super) fn working(id: u32) -> RegionedAbsoluteAddr {
    object(WORKING_REGION, id)
}

pub(super) fn r(id: usize) -> RegisterId {
    RegisterId(id)
}

pub(super) fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Imm(r(dst), SIRValue::new(value))
}

pub(super) fn load(
    dst: usize,
    addr: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Load(r(dst), addr, SIROffset::Static(offset), width)
}

pub(super) fn store(
    addr: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
    src: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        addr,
        SIROffset::Static(offset),
        width,
        r(src),
        Vec::new(),
        Vec::new(),
    )
}

pub(super) fn commit(
    src: RegionedAbsoluteAddr,
    dst: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Commit(src, dst, SIROffset::Static(offset), width, Vec::new())
}

/// A call through frame register `frame` whose function reads `reads` and
/// writes `writes`.
pub(super) fn call(
    frame: usize,
    reads: Vec<RegionedAbsoluteAddr>,
    writes: Vec<RegionedAbsoluteAddr>,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: r(frame),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: Arc::new(celox_sir::CallEffects {
            reads,
            writes,
            ..Default::default()
        }),
    }
}

pub(super) fn block(
    id: usize,
    params: Vec<usize>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) -> BasicBlock<RegionedAbsoluteAddr> {
    BasicBlock {
        id: BlockId(id),
        params: params.into_iter().map(r).collect(),
        instructions,
        terminator,
    }
}

pub(super) fn jump(target: usize) -> SIRTerminator {
    SIRTerminator::Jump(BlockId(target), Vec::new())
}

pub(super) fn branch(cond: usize, then: usize, otherwise: usize) -> SIRTerminator {
    SIRTerminator::Branch {
        cond: r(cond),
        true_block: (BlockId(then), Vec::new()),
        false_block: (BlockId(otherwise), Vec::new()),
    }
}

/// A unit of `blocks`, entered at the first, whose registers have the
/// given widths.
pub(super) fn unit(
    blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
    widths: &[(usize, usize)],
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: blocks[0].id,
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map: widths
            .iter()
            .map(|&(register, width)| {
                (
                    r(register),
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                )
            })
            .collect(),
    }
}

/// What running a unit makes observable.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Outcome {
    /// The final STABLE state, by object.
    stable: BTreeMap<RegionedAbsoluteAddr, u128>,
    /// For each call in execution order, the objects it read.
    calls: Vec<Vec<(RegionedAbsoluteAddr, u128)>>,
}

fn mask(width: usize) -> u128 {
    if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    }
}

/// A value that only `seed` produces.
fn pattern(seed: u64) -> u128 {
    let value =
        u128::from(seed.wrapping_add(1)).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835);
    value ^ (value >> 61)
}

fn offset_of(offset: &SIROffset, registers: &HashMap<RegisterId, u128>) -> usize {
    match offset {
        SIROffset::Static(offset)
        | SIROffset::PackedElements {
            bit_offset: offset, ..
        } => *offset,
        SIROffset::Dynamic(register) => registers[register] as usize,
        SIROffset::ElementRun {
            index,
            element_width,
        } => registers[index] as usize * element_width,
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => {
            registers[index] as usize * element_width
                + bit_offset
                + dynamic_bit_offset.map_or(0, |register| registers[&register] as usize)
        }
    }
}

/// Run `eu` with every object's initial value derived from `seed`.
pub(super) fn execute(eu: &ExecutionUnit<RegionedAbsoluteAddr>, seed: u64) -> Outcome {
    let mut memory = BTreeMap::<RegionedAbsoluteAddr, u128>::new();
    let initial = |addr: &RegionedAbsoluteAddr| {
        pattern(seed * 1_000 + u64::from(addr.region) * 100 + u64::from(addr.var_id.0))
    };
    let mut registers = HashMap::<RegisterId, u128>::default();
    let mut calls = Vec::new();
    let width = |register: &RegisterId| eu.register_map[register].width();
    let mut current = eu.entry_block_id;
    let mut steps = 0;
    loop {
        let block = &eu.blocks[&current];
        for instruction in &block.instructions {
            steps += 1;
            assert!(steps < 100_000, "the test unit does not terminate");
            match instruction {
                SIRInstruction::Imm(dst, value) => {
                    let digits = value.payload.to_u64_digits();
                    let value = u128::from(digits.first().copied().unwrap_or(0))
                        | u128::from(digits.get(1).copied().unwrap_or(0)) << 64;
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Binary(dst, lhs, op, rhs) => {
                    let (lhs, rhs) = (registers[lhs], registers[rhs]);
                    let value = match op {
                        BinaryOp::Add => lhs.wrapping_add(rhs),
                        BinaryOp::Sub => lhs.wrapping_sub(rhs),
                        BinaryOp::Mul => lhs.wrapping_mul(rhs),
                        BinaryOp::And => lhs & rhs,
                        BinaryOp::Or => lhs | rhs,
                        BinaryOp::Xor => lhs ^ rhs,
                        BinaryOp::Shl => lhs.checked_shl(rhs as u32).unwrap_or(0),
                        BinaryOp::Shr => lhs.checked_shr(rhs as u32).unwrap_or(0),
                        BinaryOp::Eq => u128::from(lhs == rhs),
                        BinaryOp::Ne => u128::from(lhs != rhs),
                        BinaryOp::LogicAnd => u128::from(lhs != 0 && rhs != 0),
                        BinaryOp::LogicOr => u128::from(lhs != 0 || rhs != 0),
                        other => panic!("unsupported binary operation {other:?}"),
                    };
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Unary(dst, op, source) => {
                    let source = registers[source];
                    let value = match op {
                        UnaryOp::Ident => source,
                        UnaryOp::BitNot => !source,
                        other => panic!("unsupported unary operation {other:?}"),
                    };
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Slice(dst, source, offset, slice_width) => {
                    let value = registers[source].checked_shr(*offset as u32).unwrap_or(0);
                    registers.insert(*dst, value & mask(*slice_width) & mask(width(dst)));
                }
                SIRInstruction::Concat(dst, arguments) => {
                    let mut value = 0u128;
                    for argument in arguments {
                        value = value.checked_shl(width(argument) as u32).unwrap_or(0)
                            | registers[argument];
                    }
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Mux(dst, cond, then, otherwise) => {
                    let value = if registers[cond] != 0 {
                        registers[then]
                    } else {
                        registers[otherwise]
                    };
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Load(dst, addr, offset, load_width) => {
                    let offset = offset_of(offset, &registers);
                    let value = *memory.entry(*addr).or_insert_with(|| initial(addr));
                    let value = value.checked_shr(offset as u32).unwrap_or(0) & mask(*load_width);
                    registers.insert(*dst, value & mask(width(dst)));
                }
                SIRInstruction::Store(addr, offset, store_width, source, _, _) => {
                    let offset = offset_of(offset, &registers);
                    let field = mask(*store_width) << offset;
                    let value = (registers[source] & mask(*store_width)) << offset;
                    let old = memory.entry(*addr).or_insert_with(|| initial(addr));
                    *old = (*old & !field) | value;
                }
                SIRInstruction::Commit(source, destination, offset, commit_width, _) => {
                    let offset = offset_of(offset, &registers);
                    let field = mask(*commit_width) << offset;
                    let value = *memory.entry(*source).or_insert_with(|| initial(source)) & field;
                    let old = memory
                        .entry(*destination)
                        .or_insert_with(|| initial(destination));
                    *old = (*old & !field) | value;
                }
                SIRInstruction::Call { effects, dst, .. } => {
                    let index = calls.len() as u64;
                    let read = effects
                        .reads
                        .iter()
                        .map(|addr| (*addr, *memory.entry(*addr).or_insert_with(|| initial(addr))))
                        .collect();
                    calls.push(read);
                    for addr in &effects.writes {
                        memory.insert(
                            *addr,
                            pattern(1_000_000 + index * 100 + u64::from(addr.var_id.0)),
                        );
                    }
                    if let Some(dst) = dst {
                        registers.insert(*dst, pattern(2_000_000 + index) & mask(width(dst)));
                    }
                }
                other => panic!("unsupported instruction {other}"),
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
                    true_block.clone()
                } else {
                    false_block.clone()
                }
            }
            SIRTerminator::Switch { .. } => panic!("unsupported switch"),
            SIRTerminator::Return => break,
            SIRTerminator::Error(code) => panic!("unexpected error {code}"),
        };
        let values = arguments
            .iter()
            .map(|argument| registers[argument])
            .collect::<Vec<_>>();
        for (parameter, value) in eu.blocks[&target].params.iter().zip(values) {
            registers.insert(*parameter, value);
        }
        current = target;
    }
    Outcome {
        stable: memory
            .into_iter()
            .filter(|(addr, _)| addr.region == STABLE_REGION)
            .collect(),
        calls,
    }
}

const X: u32 = 1;
const Y: u32 = 2;
const Z: u32 = 3;
const W: u32 = 4;
const C: u32 = 5;
/// An object no fixture accesses otherwise.
const UNRELATED: u32 = 9;

/// Whether the calls of a fixture write the objects around them, or only an
/// object nothing else accesses.
#[derive(Clone, Copy, Debug)]
pub(super) enum Calls {
    Writing,
    Unrelated,
}

impl Calls {
    fn effects(
        self,
        reads: Vec<RegionedAbsoluteAddr>,
        writes: Vec<RegionedAbsoluteAddr>,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        match self {
            Calls::Writing => call(FRAME, reads, writes),
            Calls::Unrelated => call(FRAME, Vec::new(), vec![stable(UNRELATED)]),
        }
    }
}

const FRAME: usize = 90;

/// Straight-line accesses of one block around two calls: forwarding,
/// redundant and contained loads, store and load coalescing, concat folding,
/// bit extraction and slices of loads all meet a call writing their object.
fn straight_line(calls: Calls) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let x = stable(X);
    let (y, z, w) = (stable(Y), stable(Z), stable(W));
    unit(
        vec![block(
            0,
            Vec::new(),
            vec![
                imm(0, 0x5A),
                imm(1, 0x3),
                imm(FRAME, 0),
                store(x, 0, 8, 0),
                store(x, 8, 4, 1),
                load(2, x, 0, 16),
                load(4, x, 0, 4),
                load(14, x, 4, 4),
                calls.effects(vec![y], vec![x]),
                load(3, x, 0, 16),
                load(5, x, 8, 4),
                load(15, x, 0, 8),
                store(x, 12, 4, 1),
                store(y, 0, 16, 2),
                store(y, 16, 16, 3),
                SIRInstruction::Concat(r(6), vec![r(5), r(14), r(4)]),
                store(z, 0, 12, 6),
                imm(7, 4),
                SIRInstruction::Binary(r(8), r(2), BinaryOp::Shr, r(7)),
                imm(9, 0xF),
                SIRInstruction::Binary(r(10), r(8), BinaryOp::And, r(9)),
                store(z, 12, 16, 10),
                load(11, w, 0, 8),
                SIRInstruction::Slice(r(12), r(11), 0, 4),
                calls.effects(vec![z, x], vec![w]),
                store(z, 28, 4, 12),
                store(w, 8, 8, 0),
                load(13, w, 0, 8),
                store(y, 0, 8, 13),
                store(y, 8, 8, 15),
            ],
            SIRTerminator::Return,
        )],
        &[
            (0, 8),
            (1, 4),
            (2, 16),
            (3, 16),
            (4, 4),
            (5, 4),
            (6, 12),
            (7, 16),
            (8, 16),
            (9, 16),
            (10, 16),
            (11, 8),
            (12, 4),
            (13, 8),
            (14, 4),
            (15, 8),
            (FRAME, 32),
        ],
    )
}

/// A load in both arms of a branch, after a call writing what an earlier
/// load in the head read.
fn branch_loads(calls: Calls) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let (x, y, c) = (stable(X), stable(Y), stable(C));
    unit(
        vec![
            block(
                0,
                Vec::new(),
                vec![
                    imm(FRAME, 0),
                    load(0, c, 0, 1),
                    load(1, x, 0, 8),
                    store(y, 0, 8, 1),
                    calls.effects(Vec::new(), vec![x]),
                ],
                branch(0, 1, 2),
            ),
            block(
                1,
                Vec::new(),
                vec![load(2, x, 0, 8), store(y, 8, 8, 2)],
                jump(3),
            ),
            block(
                2,
                Vec::new(),
                vec![load(3, x, 0, 8), store(y, 16, 8, 3)],
                jump(3),
            ),
            block(3, Vec::new(), Vec::new(), SIRTerminator::Return),
        ],
        &[(0, 1), (1, 8), (2, 8), (3, 8), (FRAME, 32)],
    )
}

/// An FF-style update staged in WORKING and published by a commit at the
/// join: one arm's call replaces the staged value after its store, and a
/// call at the join reads the old STABLE value before the publication.
fn staged_update(calls: Calls) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let (x, c) = (stable(X), stable(C));
    let staged = working(X);
    unit(
        vec![
            block(
                0,
                Vec::new(),
                vec![imm(FRAME, 0), commit(x, staged, 0, 8), load(0, c, 0, 1)],
                branch(0, 1, 2),
            ),
            block(
                1,
                Vec::new(),
                vec![imm(1, 7), store(staged, 0, 8, 1)],
                jump(3),
            ),
            block(
                2,
                Vec::new(),
                vec![
                    imm(2, 9),
                    store(staged, 0, 8, 2),
                    calls.effects(Vec::new(), vec![staged]),
                ],
                jump(3),
            ),
            block(
                3,
                Vec::new(),
                vec![
                    calls.effects(vec![x, staged], Vec::new()),
                    commit(staged, x, 0, 8),
                ],
                SIRTerminator::Return,
            ),
        ],
        &[(0, 1), (1, 8), (2, 8), (FRAME, 32)],
    )
}

fn fixtures() -> Vec<(&'static str, Calls, ExecutionUnit<RegionedAbsoluteAddr>)> {
    let mut fixtures = Vec::new();
    for calls in [Calls::Writing, Calls::Unrelated] {
        fixtures.push(("straight_line", calls, straight_line(calls)));
        fixtures.push(("branch_loads", calls, branch_loads(calls)));
        fixtures.push(("staged_update", calls, staged_update(calls)));
    }
    fixtures
}

fn call_count(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> usize {
    eu.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, SIRInstruction::Call { .. }))
        .count()
}

/// Run `pass` over every fixture and check that it opts in and keeps what
/// each fixture makes observable.
pub(super) fn check_pass(pass: &dyn ExecutionUnitPass) {
    assert!(
        pass.handles_calls(),
        "{} does not handle calls",
        pass.name()
    );
    for (name, calls, fixture) in fixtures() {
        let mut optimized = fixture.clone();
        pass.run(&mut optimized, &PassOptions::default());
        if let Err(error) = optimized.verify_result() {
            panic!("{} on {name} ({calls:?}): {error}", pass.name());
        }
        assert_eq!(
            call_count(&optimized),
            call_count(&fixture),
            "{} on {name} ({calls:?}) changed the calls",
            pass.name()
        );
        for seed in 0..4 {
            assert_eq!(
                execute(&optimized, seed),
                execute(&fixture, seed),
                "{} on {name} ({calls:?}), seed {seed}:\n{optimized}",
                pass.name()
            );
        }
    }
}

/// A context for the passes built from a program's design facts.
fn with_context<T>(build: impl FnOnce(&OptimizationContext<'_>) -> T) -> T {
    let mut sir = SirProgram {
        eval_comb: Vec::new(),
        eval_apply_ffs: HashMap::default(),
        eval_comb_apply_ffs: HashMap::default(),
        merge_comb_apply: false,
        eval_comb_stateful: None,
        eval_only_ffs: HashMap::default(),
        apply_ffs: HashMap::default(),
        parallel: None,
        processes: Vec::new(),
        functions: Vec::new(),
    };
    let design = celox_design::ElaboratedDesign::default();
    let runtime_schema = celox_design::RuntimeSchema::default();
    let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
    let context = OptimizationContext {
        sir: &mut sir,
        design: &design,
        runtime_schema: &runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    build(&context)
}

macro_rules! pass_tests {
    ($($name:ident => $pass:expr;)*) => {
        $(
            #[test]
            fn $name() {
                check_pass(&$pass);
            }
        )*
    };
}

pass_tests! {
    store_load_forwarding => StoreLoadForwardingPass;
    partial_forward => PartialForwardPass;
    control_flow_simplify => ControlFlowSimplifyPass;
    post_gvn_cfg_cleanup => PostGvnCfgCleanupPass;
    gvn => GvnPass;
    indexed_store_recovery => with_context(IndexedStoreRecoveryPass::for_program);
    concat_folding => ConcatFoldingPass::default();
    xor_chain_folding => XorChainFoldingPass;
    hoist_common_branch_loads => HoistCommonBranchLoadsPass;
    bit_extract_peephole => BitExtractPeepholePass;
    optimize_blocks => OptimizeBlocksPass {
        skip_final_schedule: false,
        element_widths: Arc::default(),
    };
    coalesce_stores => CoalesceStoresPass::default();
    split_wide_commits => SplitWideCommitsPass;
    guarded_region_sinking => GuardedRegionSinkingPass;
    branchify_mux => BranchifyMuxPass;
    loop_idiom => LoopIdiomPass;
    vectorize_concat => VectorizeConcatPass::default();
    masked_array_any => with_context(MaskedArrayAnyPass::for_program);
    circular_priority => with_context(CircularPriorityPass::for_program);
    sparse_case_dispatch => SparseCaseDispatchPass::new(&HashMap::default());
    dead_code_elimination => DeadCodeEliminationPass;
    eliminate_dead_working_stores => EliminateDeadWorkingStoresPass;
    reschedule => ReschedulePass;
    split_coalesced_stores => SplitCoalescedStoresPass { max_store_width: 64 };
    commit_sinking => CommitSinkingPass;
    packed_scatter_store => with_context(PackedScatterStorePass::for_program);
    phi_outcome_compression => PhiOutcomeCompressionPass;
}

/// The instructions of the only block of a straight-line unit.
fn instructions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> &[SIRInstruction<RegionedAbsoluteAddr>] {
    &eu.blocks[&BlockId(0)].instructions
}

fn loads_of(eu: &ExecutionUnit<RegionedAbsoluteAddr>, addr: RegionedAbsoluteAddr) -> usize {
    eu.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, SIRInstruction::Load(_, a, ..) if *a == addr))
        .count()
}

/// The passes still optimize across a call that leaves their object alone:
/// a call writing an unrelated object lets them remove a load the writing
/// call keeps.
#[test]
fn a_call_leaving_the_object_alone_does_not_stop_load_elimination() {
    let passes: [&dyn ExecutionUnitPass; 2] = [
        &OptimizeBlocksPass {
            skip_final_schedule: false,
            element_widths: Arc::default(),
        },
        &GvnPass,
    ];
    for pass in passes {
        let x = stable(X);
        let reloaded = |calls: Calls| {
            let mut eu = unit(
                vec![block(
                    0,
                    Vec::new(),
                    vec![
                        imm(FRAME, 0),
                        load(0, x, 0, 8),
                        store(stable(Y), 0, 8, 0),
                        calls.effects(Vec::new(), vec![x]),
                        load(1, x, 0, 8),
                        store(stable(Y), 8, 8, 1),
                    ],
                    SIRTerminator::Return,
                )],
                &[(0, 8), (1, 8), (FRAME, 32)],
            );
            pass.run(&mut eu, &PassOptions::default());
            loads_of(&eu, x)
        };
        assert_eq!(reloaded(Calls::Writing), 2, "{}", pass.name());
        assert_eq!(reloaded(Calls::Unrelated), 1, "{}", pass.name());
    }
}

/// Store coalescing merges adjacent narrow stores across a call that leaves
/// their object alone, and keeps them apart around one that reads it.
#[test]
fn store_coalescing_stops_only_at_a_call_reading_the_object() {
    let x = stable(X);
    for (calls, merged) in [(Calls::Writing, false), (Calls::Unrelated, true)] {
        let mut eu = unit(
            vec![block(
                0,
                Vec::new(),
                vec![
                    imm(FRAME, 0),
                    imm(0, 1),
                    imm(1, 2),
                    store(x, 0, 4, 0),
                    calls.effects(vec![x], Vec::new()),
                    store(x, 4, 4, 1),
                ],
                SIRTerminator::Return,
            )],
            &[(0, 4), (1, 4), (FRAME, 32)],
        );
        CoalesceStoresPass::default().run(&mut eu, &PassOptions::default());
        let stores = instructions(&eu)
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count();
        assert_eq!(stores == 1, merged, "{calls:?}:\n{eu}");
    }
}

/// Commit sinking publishes a predecessor's staged value directly unless a
/// call after that store may have replaced it.
#[test]
fn commit_sinking_stops_at_a_call_replacing_the_staged_value() {
    for (calls, sunk) in [(Calls::Writing, false), (Calls::Unrelated, true)] {
        let (x, staged) = (stable(X), working(X));
        let mut eu = unit(
            vec![
                block(
                    0,
                    Vec::new(),
                    vec![imm(FRAME, 0), load(0, stable(C), 0, 1)],
                    branch(0, 1, 2),
                ),
                block(
                    1,
                    Vec::new(),
                    vec![imm(1, 7), store(staged, 0, 8, 1)],
                    jump(3),
                ),
                block(
                    2,
                    Vec::new(),
                    vec![
                        imm(2, 9),
                        store(staged, 0, 8, 2),
                        calls.effects(Vec::new(), vec![staged]),
                    ],
                    jump(3),
                ),
                block(
                    3,
                    Vec::new(),
                    vec![commit(staged, x, 0, 8)],
                    SIRTerminator::Return,
                ),
            ],
            &[(0, 1), (1, 8), (2, 8), (FRAME, 32)],
        );
        CommitSinkingPass.run(&mut eu, &PassOptions::default());
        let commits = eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Commit(..)))
            .count();
        assert_eq!(commits == 0, sunk, "{calls:?}:\n{eu}");
    }
}

/// A WORKING store is dead when nothing reads it before the unit ends, but
/// not when a call reads its object.
#[test]
fn a_call_reading_working_state_keeps_its_stores() {
    for (reads, kept) in [(vec![working(X)], true), (vec![working(Y)], false)] {
        let mut eu = unit(
            vec![block(
                0,
                Vec::new(),
                vec![
                    imm(FRAME, 0),
                    imm(0, 1),
                    store(working(X), 0, 8, 0),
                    call(FRAME, reads.clone(), Vec::new()),
                ],
                SIRTerminator::Return,
            )],
            &[(0, 8), (FRAME, 32)],
        );
        EliminateDeadWorkingStoresPass.run(&mut eu, &PassOptions::default());
        let stores = instructions(&eu)
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count();
        assert_eq!(stores == 1, kept, "{reads:?}:\n{eu}");
    }
}

/// A program with calls runs the pipeline, its function units too: a frame
/// slot is an ordinary location there, forwarded across a call, which never
/// touches the caller's frame, while state the callee writes is loaded
/// again.
#[test]
fn function_units_are_optimized_with_frame_slots_as_locations() {
    let slot = object(FRAME_REGION, X);
    let x = stable(X);
    let body = unit(
        vec![block(
            0,
            Vec::new(),
            vec![
                imm(FRAME, 0),
                imm(0, 5),
                store(slot, 0, 8, 0),
                load(1, x, 0, 8),
                SIRInstruction::Call {
                    function: celox_sir::FunctionId(1),
                    frame: r(FRAME),
                    args: Vec::new(),
                    outputs: Vec::new(),
                    dst: None,
                    effects: Default::default(),
                },
                load(2, slot, 0, 8),
                load(3, x, 0, 8),
                SIRInstruction::Binary(r(4), r(2), BinaryOp::Add, r(3)),
                SIRInstruction::Binary(r(5), r(4), BinaryOp::Add, r(1)),
                store(slot, 0, 8, 5),
            ],
            SIRTerminator::Return,
        )],
        &[(0, 8), (1, 8), (2, 8), (3, 8), (4, 8), (5, 8), (FRAME, 32)],
    );
    // A caller in the settle, whose loads of `y` around the call merge.
    let y = stable(Y);
    let caller = unit(
        vec![block(
            0,
            Vec::new(),
            vec![
                imm(FRAME, 0),
                load(0, y, 0, 8),
                SIRInstruction::Call {
                    function: celox_sir::FunctionId(1),
                    frame: r(FRAME),
                    args: Vec::new(),
                    outputs: Vec::new(),
                    dst: None,
                    effects: Default::default(),
                },
                load(1, y, 0, 8),
                SIRInstruction::Binary(r(2), r(0), BinaryOp::Add, r(1)),
                store(stable(Z), 0, 8, 2),
            ],
            SIRTerminator::Return,
        )],
        &[(0, 8), (1, 8), (2, 8), (FRAME, 32)],
    );
    let mut sir = SirProgram {
        eval_comb: vec![caller],
        eval_apply_ffs: HashMap::default(),
        eval_comb_apply_ffs: HashMap::default(),
        merge_comb_apply: false,
        eval_comb_stateful: None,
        eval_only_ffs: HashMap::default(),
        apply_ffs: HashMap::default(),
        parallel: None,
        processes: Vec::new(),
        functions: vec![
            celox_sir::FunctionUnit {
                unit: body,
                frame: celox_sir::FrameLayout {
                    size: 8,
                    slots: vec![celox_sir::FrameSlot {
                        addr: slot,
                        offset: 0,
                        width: 8,
                        four_state: false,
                        handle: false,
                        element_width: None,
                    }],
                },
                effects: Default::default(),
            },
            // The callee writes `x`.
            celox_sir::FunctionUnit {
                unit: unit(
                    vec![block(
                        0,
                        Vec::new(),
                        vec![imm(0, 1), store(x, 0, 8, 0)],
                        SIRTerminator::Return,
                    )],
                    &[(0, 8)],
                ),
                frame: celox_sir::FrameLayout {
                    size: 0,
                    slots: Vec::new(),
                },
                effects: Default::default(),
            },
        ],
    };
    sir.summarize_calls();
    let design = celox_design::ElaboratedDesign::default();
    let runtime_schema = celox_design::RuntimeSchema::default();
    let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
    let mut context = OptimizationContext {
        sir: &mut sir,
        design: &design,
        runtime_schema: &runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    super::pipeline::run(&mut context, &PassOptions::default());
    let body = &context.sir.functions[0].unit;
    assert_eq!(loads_of(body, slot), 0, "{body}");
    assert_eq!(loads_of(body, x), 2, "{body}");
    // A program with calls runs the ordinary pipeline.
    let caller = &context.sir.eval_comb[0];
    assert_eq!(loads_of(caller, y), 1, "{caller}");
    assert!(context.sir.calls_summarized());
}

#[test]
fn a_function_keeps_a_frame_slot_value_it_overwrites_later() {
    // `f` saves its slot, overwrites it, and stores bits of the saved value
    // to `z`: no reload may read the overwritten slot.
    let slot = object(FRAME_REGION, X);
    let body = unit(
        vec![block(
            0,
            Vec::new(),
            vec![
                imm(0, 0xab),
                store(slot, 0, 8, 0),
                load(1, slot, 0, 8),
                imm(2, 0x11),
                store(slot, 0, 8, 2),
                SIRInstruction::Slice(r(3), r(1), 0, 4),
                store(stable(Z), 0, 4, 3),
            ],
            SIRTerminator::Return,
        )],
        &[(0, 8), (1, 8), (2, 8), (3, 4)],
    );
    let caller = unit(
        vec![block(
            0,
            Vec::new(),
            vec![
                imm(FRAME, 0),
                SIRInstruction::Call {
                    function: celox_sir::FunctionId(0),
                    frame: r(FRAME),
                    args: Vec::new(),
                    outputs: Vec::new(),
                    dst: None,
                    effects: Default::default(),
                },
            ],
            SIRTerminator::Return,
        )],
        &[(FRAME, 32)],
    );
    let before = execute(&body, 1);
    let mut sir = SirProgram {
        eval_comb: vec![caller],
        eval_apply_ffs: HashMap::default(),
        eval_comb_apply_ffs: HashMap::default(),
        merge_comb_apply: false,
        eval_comb_stateful: None,
        eval_only_ffs: HashMap::default(),
        apply_ffs: HashMap::default(),
        parallel: None,
        processes: Vec::new(),
        functions: vec![celox_sir::FunctionUnit {
            unit: body,
            frame: celox_sir::FrameLayout {
                size: 1,
                slots: vec![celox_sir::FrameSlot {
                    addr: slot,
                    offset: 0,
                    width: 8,
                    four_state: false,
                    handle: false,
                    element_width: None,
                }],
            },
            effects: Default::default(),
        }],
    };
    sir.summarize_calls();
    let design = celox_design::ElaboratedDesign::default();
    let runtime_schema = celox_design::RuntimeSchema::default();
    let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
    let mut context = OptimizationContext {
        sir: &mut sir,
        design: &design,
        runtime_schema: &runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    super::pipeline::run(&mut context, &PassOptions::default());
    let body = &context.sir.functions[0].unit;
    assert_eq!(execute(body, 1), before, "{body}");
}
