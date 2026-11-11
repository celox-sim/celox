use super::*;
use crate::ir::{
    BasicBlock, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset, SIRTerminator,
    SirProgram,
};
use celox_sir::{
    BinaryOp, BlockId, FrameLayout, FrameSlot, FunctionId, FunctionUnit, RegisterId, RegisterType,
    SIRValue,
};

/// The width of the object a callee reads, `a`.
const INPUT: usize = 13;
/// The width of the object it writes, `b`.
const OUTPUT: usize = 17;

fn bits(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn block(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    }
}

/// The one object `units` load (or store, when `stored`) with
/// `width` bits.
fn object<'u>(
    units: impl IntoIterator<Item = &'u ExecutionUnit<RegionedAbsoluteAddr>>,
    width: usize,
    stored: bool,
) -> RegionedAbsoluteAddr {
    let mut found: Vec<RegionedAbsoluteAddr> = units
        .into_iter()
        .flat_map(|unit| unit.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Load(_, addr, _, w) if !stored && *w == width => Some(*addr),
            SIRInstruction::Store(addr, _, w, ..) if stored && *w == width => Some(*addr),
            _ => None,
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    assert_eq!(found.len(), 1, "one {width}-bit object: {found:?}");
    found[0]
}

/// Append a function that stores `a + 100` into `b`, then runs
/// `extra`, with a frame of one slot it leaves unused.
fn push_adder(
    sir: &mut SirProgram,
    a: RegionedAbsoluteAddr,
    b: RegionedAbsoluteAddr,
    extra: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> FunctionId {
    let reg = RegisterId;
    let mut instructions = vec![
        SIRInstruction::Load(reg(0), a, SIROffset::Static(0), INPUT),
        SIRInstruction::Imm(reg(1), SIRValue::new(0u8)),
        SIRInstruction::Concat(reg(2), vec![reg(1), reg(0)]),
        SIRInstruction::Imm(reg(3), SIRValue::new(100u8)),
        SIRInstruction::Binary(reg(4), reg(2), BinaryOp::Add, reg(3)),
        SIRInstruction::Store(b, SIROffset::Static(0), OUTPUT, reg(4), vec![], vec![]),
    ];
    instructions.extend(extra);
    let mut unit = block(instructions);
    for (register, width) in [
        (0, INPUT),
        (1, OUTPUT - INPUT),
        (2, OUTPUT),
        (3, OUTPUT),
        (4, OUTPUT),
    ] {
        unit.register_map.insert(reg(register), bits(width));
    }
    push_function(sir, unit)
}

/// Append a function that stores zero into the one-bit `target`.
fn push_clearer(sir: &mut SirProgram, target: RegionedAbsoluteAddr) -> FunctionId {
    let mut unit = block(vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
        SIRInstruction::Store(
            target,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            vec![],
            vec![],
        ),
    ]);
    unit.register_map.insert(RegisterId(0), bits(1));
    push_function(sir, unit)
}

fn push_function(sir: &mut SirProgram, unit: ExecutionUnit<RegionedAbsoluteAddr>) -> FunctionId {
    let id = FunctionId(sir.functions.len() as u32);
    sir.functions.push(FunctionUnit {
        unit,
        frame: FrameLayout {
            size: 4,
            slots: vec![FrameSlot {
                addr: RegionedAbsoluteAddr {
                    region: celox_design::FRAME_REGION,
                    instance_id: celox_design::InstanceId(0),
                    var_id: celox_design::StateObjectId(id.0),
                },
                offset: 0,
                width: 32,
                four_state: false,
                handle: false,
                element_width: None,
            }],
        },
        effects: Default::default(),
    });
    id
}

/// The instructions of a call of `function` at frame offset zero:
/// its effects are given by `SirProgram::summarize_calls`.
fn call(function: FunctionId, frame: usize) -> [SIRInstruction<RegionedAbsoluteAddr>; 2] {
    [
        SIRInstruction::Imm(RegisterId(frame), SIRValue::new(0u8)),
        SIRInstruction::Call {
            function,
            frame: RegisterId(frame),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: Default::default(),
        },
    ]
}

/// A unit that calls `function`.
fn caller(function: FunctionId) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut unit = block(call(function, 0).into());
    unit.register_map.insert(RegisterId(0), bits(32));
    unit
}

/// Prepend a call of `function` to `unit`.
fn prepend_call(unit: &mut ExecutionUnit<RegionedAbsoluteAddr>, function: FunctionId) {
    let frame = unit.register_map.keys().map(|r| r.0 + 1).max().unwrap_or(0);
    unit.register_map.insert(RegisterId(frame), bits(32));
    let entry = unit.entry_block_id;
    let instructions = &mut unit.blocks.get_mut(&entry).unwrap().instructions;
    instructions.splice(0..0, call(function, frame));
}

/// Run `function` first in every settle, the fused ones included.
fn settle_first(sir: &mut SirProgram, function: FunctionId) {
    sir.eval_comb.insert(0, caller(function));
    for units in sir.eval_comb_apply_ffs.values_mut() {
        units.insert(0, caller(function));
    }
}

/// The settle computes `b = a + 100` in a function, before the
/// logic that reads `b`.
fn comb_adder(sir: &mut SirProgram) {
    let a = object(&sir.eval_comb, INPUT, false);
    let b = object(&sir.eval_comb, OUTPUT, false);
    let adder = push_adder(sir, a, b, Vec::new());
    settle_first(sir, adder);
    sir.summarize_calls();
}

const UNTIMED: &str = r#"
module Top(input logic clk, input logic [12:0] a, input logic [16:0] b,
           output logic [12:0] w, output logic [16:0] z, output logic [16:0] q);
    assign w = a ^ 13'd1;
    assign z = b + 17'd1;
    always_ff @(posedge clk) q <= b;
endmodule
"#;

fn untimed() -> SimulatorBuilder<'static, Simulator> {
    SimulatorBuilder::<Simulator>::from_sv_sources(
        vec![(UNTIMED, std::path::Path::new("calls.sv"))],
        "Top",
    )
    .four_state(false)
    .transform_sir(comb_adder)
}

fn check_untimed<B: crate::SimBackend>(backend: &str, mut sim: Simulator<B>) {
    let (a, z, q, clk) = (
        sim.signal("a"),
        sim.signal("z"),
        sim.signal("q"),
        sim.event("clk"),
    );
    for n in [5u16, 1000, 8191] {
        sim.modify(|io| io.set(a, n)).unwrap();
        let b = u32::from(n) + 100;
        assert_eq!(sim.get_as::<u32>(z), b + 1, "{backend}: z for a = {n}");
        sim.posedge(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), b, "{backend}: q for a = {n}");
        sim.negedge(clk).unwrap();
    }
}

/// A function's write reaches the combinational and the flip-flop
/// logic that read it, on every backend.
#[test]
fn function_writes_reach_their_readers_on_every_backend() {
    check_untimed("interpreter", untimed().build_interpreter().unwrap());
    #[cfg(feature = "cranelift")]
    check_untimed("cranelift", untimed().build_cranelift().unwrap());
    #[cfg(feature = "wasm-runtime")]
    check_untimed("wasm", untimed().build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check_untimed("native", untimed().build_native().unwrap());
}

/// What only a callee writes is a combinational write of the
/// design, of the whole object; what only a callee reads and writes
/// is ranked and allocated as an access of the unit's own would be.
#[test]
fn inventories_count_what_calls_touch() {
    let (program, _, _, _, _) = untimed()
        .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
        .unwrap();
    let a = object([&program.sir.functions[0].unit], INPUT, false);
    let b = object([&program.sir.functions[0].unit], OUTPUT, true);
    let schema = &program.runtime().runtime_schema;
    assert!(schema.comb_writes.contains(&b.absolute_addr()));
    assert!(schema.rtl_writes.contains(&celox_design::VarAtomBase {
        id: b.absolute_addr(),
        access: celox_design::BitAccess::new(0, OUTPUT - 1),
    }));
    // The FF units, ranked and lane-partitioned, with the call alone
    // touching `a` and `b`.
    let LaidOutProgram {
        mut sir, runtime, ..
    } = program;
    let adder = FunctionId(0);
    let touches = |instruction: &SIRInstruction<RegionedAbsoluteAddr>| {
        [instruction.memory_read(), instruction.memory_write()]
            .into_iter()
            .flatten()
            .any(|access| {
                access.addr.absolute_addr() == a.absolute_addr()
                    || access.addr.absolute_addr() == b.absolute_addr()
            })
    };
    for unit in sir
        .eval_apply_ffs
        .values_mut()
        .chain(sir.eval_only_ffs.values_mut())
        .chain(sir.apply_ffs.values_mut())
        .flatten()
    {
        for block in unit.blocks.values_mut() {
            block
                .instructions
                .retain(|instruction| !touches(instruction));
        }
    }
    for unit in sir.eval_apply_ffs.values_mut().flatten() {
        prepend_call(unit, adder);
    }
    sir.eval_comb.clear();
    sir.eval_comb_apply_ffs.clear();
    sir.parallel = Some(celox_sir::ParallelSirProgram {
        lanes: 2,
        eval_comb: vec![celox_sir::LaneUnit {
            lane: 1,
            unit: caller(adder),
        }],
        eval_apply_ffs: HashMap::default(),
    });
    sir.summarize_calls();
    let program = crate::ir::OptimizedSir {
        sir,
        runtime,
        layout_requirements: Default::default(),
    };
    let input = celox_state_layout::LayoutSource::layout_input(
        &program,
        crate::MemoryLayoutMode::ElementStrided,
    );
    assert!(input.ff_referenced_addresses.contains(&a.absolute_addr()));
    assert!(input.ff_referenced_addresses.contains(&b.absolute_addr()));
    assert!(input.access_rank.contains_key(&a.absolute_addr()));
    assert!(input.access_rank.contains_key(&b.absolute_addr()));
    assert_eq!(
        input.lane_writers.stable.get(&b.absolute_addr()),
        Some(&0b10)
    );
    assert!(!input.lane_writers.stable.contains_key(&a.absolute_addr()));
}

const ARRAY: &str = r#"
module Top (
    clk: input clock, write: input logic, index: input logic<2>, probe: input logic<2>,
    d: input logic<7>, q: output logic<7>,
) {
    var lanes: logic<7> [4];
    always_ff (clk) {
        if write {
            lanes[index] = d;
        }
    }
    assign q = lanes[probe];
}
"#;

/// The layout keeps an array element-strided only when every access
/// allows it, a callee's included.
#[test]
fn strided_layouts_follow_the_accesses_of_callees() {
    let (program, _, _, _, _) = Simulator::builder(ARRAY, "Top")
        .four_state(false)
        .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
        .unwrap();
    let mem = program.get_addr(&[], &["lanes"]).unwrap();
    let LaidOutProgram {
        mut sir, runtime, ..
    } = program;
    let layout = |sir: &SirProgram| {
        let program = crate::ir::OptimizedSir {
            sir: sir.clone(),
            runtime: runtime.clone(),
            layout_requirements: Default::default(),
        };
        celox_state_layout::LayoutSource::layout_input(
            &program,
            crate::MemoryLayoutMode::ElementStrided,
        )
        .unpacked_arrays
    };
    assert!(
        layout(&sir).contains_key(&mem),
        "the fixture's array is strided"
    );
    // A callee that reads two elements at once, across the padding.
    let mut unit = block(vec![SIRInstruction::Load(
        RegisterId(0),
        RegionedAbsoluteAddr::from_absolute_addr(celox_design::STABLE_REGION, mem),
        SIROffset::Static(0),
        14,
    )]);
    unit.register_map.insert(RegisterId(0), bits(14));
    let function = push_function(&mut sir, unit);
    sir.eval_comb.push(caller(function));
    sir.summarize_calls();
    assert!(!layout(&sir).contains_key(&mem));
}

const TIMED: &str = r#"
`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [12:0] a = 0;
    logic [16:0] b = 0;
    logic [16:0] z, q = 0;
    logic [10:0] n = 0;
    assign z = b + 17'd1;
    always_ff @(posedge clk) q <= z;
    always #1 n = n + 11'd1;
    initial begin
        #1 a = 13'd5;
        #1 if (z != 17'd106) $fatal(1, "stale z %0d", z);
        clk = 1;
        #1 if (q != 17'd106) $fatal(1, "stale q %0d", q);
        a = 13'd7;
        #1 if (z != 17'd108) $fatal(1, "stale z %0d", z);
        clk = 0;
        $finish;
    end
endmodule
"#;

/// The timed design's settle computes `b = a + 100` in a function;
/// the only other access of `a` is the initial process's write.
fn timed_adder(sir: &mut SirProgram) {
    let a = object(&sir.processes, INPUT, true);
    let b = object(&sir.eval_comb, OUTPUT, false);
    let adder = push_adder(sir, a, b, Vec::new());
    settle_first(sir, adder);
    sir.summarize_calls();
}

/// ...and also stores the clock in another function.
fn timed_adder_clearing_the_clock(sir: &mut SirProgram) {
    let clk = clock(sir);
    let clearer = push_clearer(sir, clk);
    sir.eval_comb.push(caller(clearer));
    timed_adder(sir);
}

fn clock(sir: &SirProgram) -> RegionedAbsoluteAddr {
    let mut events: Vec<_> = sir.eval_apply_ffs.keys().copied().collect();
    events.sort_unstable();
    assert_eq!(events.len(), 1);
    RegionedAbsoluteAddr::from_absolute_addr(celox_design::STABLE_REGION, events[0])
}

/// The process that stores `target` other than as an initializer:
/// the last one.
fn writer_of(sir: &SirProgram, target: RegionedAbsoluteAddr) -> usize {
    (0..sir.processes.len())
                .rev()
                .find(|&index| {
                    sir.processes[index]
                        .blocks
                        .values()
                        .flat_map(|block| &block.instructions)
                        .any(|instruction| matches!(instruction, SIRInstruction::Store(addr, ..) if *addr == target))
                })
                .unwrap()
}

fn timed(transform: fn(&mut SirProgram)) -> SimulatorBuilder<'static, crate::Simulation> {
    crate::Simulation::from_sv_sources(vec![(TIMED, std::path::Path::new("timed-calls.sv"))], "Top")
        .four_state(false)
        .transform_sir(transform)
}

fn plan(
    transform: fn(&mut SirProgram),
) -> (
    LaidOutProgram,
    Result<crate::scheduler::PlanSummary, String>,
) {
    let (program, _, _, _, _) = timed(transform)
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    let plan = crate::scheduler::plan_for_test(&program);
    (program, plan)
}

/// The compiled scheduler settles after a process writes what only
/// a callee of the settle reads, and the flip-flop that reads the
/// callee's result sees it.
#[test]
fn the_scheduler_settles_for_what_callees_read() {
    let (program, summary) = plan(timed_adder);
    let a = object([&program.sir.functions[0].unit], INPUT, false);
    let shared = program.shared_storage();
    let a = shared
        .get(&a.absolute_addr())
        .copied()
        .unwrap_or(a.absolute_addr());
    assert!(summary.unwrap().settle_reads.unwrap().contains(&a));
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut simulation = timed(timed_adder).build_native().unwrap();
        simulation.run_until(10).unwrap();
        assert!(simulation.is_finished());
        assert!(simulation.scheduler_stats().compiled_slots > 0);
    }
    let mut simulation = timed(timed_adder).build_interpreter().unwrap();
    simulation.run_until(10).unwrap();
    assert!(simulation.is_finished());
    #[cfg(feature = "cranelift")]
    {
        let mut simulation = timed(timed_adder).build_cranelift().unwrap();
        simulation.run_until(10).unwrap();
        assert!(simulation.is_finished());
    }
}

/// A clock a function of the settle stores is driven by RTL logic,
/// which the scheduler does not take for one only its process
/// moves.
#[test]
fn a_clock_a_callee_stores_is_driven_by_rtl() {
    let (program, summary) = plan(timed_adder);
    let clk = clock(&program.sir).absolute_addr();
    assert!(!summary.unwrap().derived.contains(&clk));
    let (_, summary) = plan(timed_adder_clearing_the_clock);
    let error = summary.err().unwrap();
    assert!(
        error.contains("RTL logic drives an event signal"),
        "{error}"
    );
}

/// The width of the counter the free-running process increments.
const COUNTER: usize = 11;

/// Append a function that fails.
fn push_failing(sir: &mut SirProgram) -> FunctionId {
    let mut unit = block(Vec::new());
    unit.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Error(1);
    push_function(sir, unit)
}

/// The process that increments the counter: the one that loads it.
fn incrementer(sir: &SirProgram) -> usize {
    let n = object(&sir.processes, COUNTER, false);
    (0..sir.processes.len())
                .find(|&index| {
                    sir.processes[index]
                        .blocks
                        .values()
                        .flat_map(|block| &block.instructions)
                        .any(|instruction| matches!(instruction, SIRInstruction::Load(_, addr, ..) if *addr == n))
                })
                .unwrap()
}

fn no_calls(_: &mut SirProgram) {}

/// The counter's process calls a function that cannot fail.
fn process_calls_a_quiet_function(sir: &mut SirProgram) {
    let a = object(&sir.processes, INPUT, true);
    let b = object(&sir.eval_comb, OUTPUT, false);
    let adder = push_adder(sir, a, b, Vec::new());
    let process = incrementer(sir);
    prepend_call(&mut sir.processes[process], adder);
    sir.summarize_calls();
}

/// The counter's process calls a function that may fail.
fn process_calls_a_failing_function(sir: &mut SirProgram) {
    let failing = push_failing(sir);
    let process = incrementer(sir);
    prepend_call(&mut sir.processes[process], failing);
    sir.summarize_calls();
}

/// The settle calls a function that may fail.
fn settle_calls_a_failing_function(sir: &mut SirProgram) {
    let failing = push_failing(sir);
    sir.eval_comb.push(caller(failing));
    sir.summarize_calls();
}

/// A call that cannot fail costs no settle its laziness; one that may
/// fail costs it only the unit that makes it: a process that makes
/// it observes the settled state, and only a settle that makes it
/// runs where the reference's does.
#[test]
fn only_units_with_failing_calls_lose_lazy_settles() {
    let (program, base) = plan(no_calls);
    let base = base.unwrap();
    let kernels = base.lazy.clone().expect("the fixture defers its settles");
    let counter = incrementer(&program.sir);
    assert!(!kernels[counter], "the counter does not observe the settle");
    for transform in [timed_adder, process_calls_a_quiet_function] {
        let summary = plan(transform).1.unwrap();
        assert_eq!(summary.lazy, base.lazy);
        assert_eq!(summary.incremental, base.incremental);
    }
    let summary = plan(process_calls_a_failing_function).1.unwrap();
    assert_eq!(summary.incremental, base.incremental);
    let lazy = summary.lazy.unwrap();
    for (index, (&observes, &before)) in lazy.iter().zip(&kernels).enumerate() {
        assert_eq!(observes, before || index == counter, "kernel {index}");
    }
    let summary = plan(settle_calls_a_failing_function).1.unwrap();
    assert!(summary.lazy.is_none() && !summary.incremental);
}

/// The settle's function records a runtime event, and the process
/// that writes `a` calls one that records one too.
fn recording_calls(sir: &mut SirProgram) {
    let a = object(&sir.processes, INPUT, true);
    let b = object(&sir.eval_comb, OUTPUT, false);
    let event = || SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: Vec::new(),
    };
    let recorder = push_adder(sir, a, b, vec![event()]);
    let process = writer_of(sir, a);
    prepend_call(&mut sir.processes[process], recorder);
    let adder = push_adder(sir, a, b, vec![event()]);
    settle_first(sir, adder);
    sir.summarize_calls();
}

/// Callees that record runtime events keep their callers compiled:
/// no kernel falls back for them and the plan stands.
#[test]
fn records_of_callees_stay_in_the_scheduler() {
    let base = plan(no_calls).1.unwrap();
    let summary = plan(recording_calls).1.unwrap();
    assert_eq!(summary.fallback, base.fallback);
}

const FINISHING: &str = r#"
`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [10:0] n = 0;
    logic [3:0] count = 0;
    always #1 clk = ~clk;
    always #1 n = n + 11'd1;
    always_ff @(posedge clk) begin
        count <= count + 4'd1;
        if (count == 4'd2) $finish;
    end
endmodule
"#;

/// The flip-flop's `$finish` record moves into a function it calls
/// there instead, which gets the record's arguments in frame slots.
fn ff_finish_in_a_function(sir: &mut SirProgram) {
    let mut recorder = None;
    let units = sir
        .eval_apply_ffs
        .values_mut()
        .chain(sir.eval_comb_apply_ffs.values_mut())
        .chain(sir.eval_only_ffs.values_mut())
        .chain(sir.apply_ffs.values_mut())
        .flatten();
    let mut functions = Vec::new();
    for unit in units {
        let mut frame = unit.register_map.keys().map(|r| r.0 + 1).max().unwrap_or(0);
        let mut new_registers = Vec::new();
        for block in unit.blocks.values_mut() {
            let old = std::mem::take(&mut block.instructions);
            for instruction in old {
                let SIRInstruction::RuntimeEvent { site_id, args } = &instruction else {
                    block.instructions.push(instruction);
                    continue;
                };
                let function = *recorder.get_or_insert_with(|| {
                    let id = FunctionId(functions.len() as u32);
                    functions.push(recording_function(*site_id, args, &unit.register_map));
                    id
                });
                new_registers.push(RegisterId(frame));
                let [
                    imm,
                    SIRInstruction::Call {
                        function,
                        frame: frame_register,
                        outputs,
                        dst,
                        effects,
                        ..
                    },
                ] = call(function, frame)
                else {
                    unreachable!()
                };
                frame += 1;
                block.instructions.push(imm);
                block.instructions.push(SIRInstruction::Call {
                    function,
                    frame: frame_register,
                    args: args.iter().copied().enumerate().collect(),
                    outputs,
                    dst,
                    effects,
                });
            }
        }
        for register in new_registers {
            unit.register_map.insert(register, bits(32));
        }
    }
    assert!(recorder.is_some(), "the `$finish` record");
    assert!(sir.functions.is_empty());
    sir.functions = functions;
    sir.summarize_calls();
}

/// A function that records site `site_id` with `args` of the types
/// `types` gives them, passed in its frame slots.
fn recording_function(
    site_id: u32,
    args: &[RegisterId],
    types: &HashMap<RegisterId, RegisterType>,
) -> FunctionUnit<RegionedAbsoluteAddr> {
    let mut slots = Vec::new();
    let mut body = Vec::new();
    let mut registers = Vec::new();
    let mut offset = 0;
    for (index, arg) in args.iter().enumerate() {
        let ty = types[arg].clone();
        let addr = RegionedAbsoluteAddr {
            region: celox_design::FRAME_REGION,
            instance_id: celox_design::InstanceId(0),
            var_id: celox_design::StateObjectId(index as u32),
        };
        let slot = FrameSlot {
            addr,
            offset,
            width: ty.width(),
            four_state: matches!(ty, RegisterType::Logic { .. }),
            handle: false,
            element_width: None,
        };
        offset += slot.size().next_multiple_of(8);
        slots.push(slot);
        body.push(SIRInstruction::Load(
            RegisterId(index),
            addr,
            SIROffset::Static(0),
            ty.width(),
        ));
        registers.push((RegisterId(index), ty));
    }
    body.push(SIRInstruction::RuntimeEvent {
        site_id,
        args: (0..args.len()).map(RegisterId).collect(),
    });
    let mut unit = block(body);
    unit.register_map.extend(registers);
    FunctionUnit {
        unit,
        frame: FrameLayout {
            size: offset.max(8),
            slots,
        },
        effects: Default::default(),
    }
}

/// A `$finish` a callee of a flip-flop records ends a compiled run
/// where it ends the reference's: the scheduler checks for it after
/// the domain, as for a record of the flip-flop's own.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn a_callee_finish_ends_the_compiled_run() {
    let run = |mode| {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(FINISHING, std::path::Path::new("finishing.sv"))],
            "Top",
        )
        .four_state(false)
        .transform_sir(ff_finish_in_a_function)
        .scheduler_mode(mode)
        .build_native()
        .unwrap();
        simulation.run_until(40).unwrap();
        assert!(simulation.is_finished(), "{mode:?}");
        let n = simulation.signal("n");
        let compiled = simulation.scheduler_stats().compiled_slots;
        (simulation.time(), simulation.get(n), compiled)
    };
    let (time, n, _) = run(crate::SchedulerMode::Reference);
    let (compiled_time, compiled_n, slots) = run(crate::SchedulerMode::Compiled);
    assert!(slots > 0);
    assert_eq!((compiled_time, compiled_n), (time, n));
    assert!(time < 40, "{time}");
}

const ALIASED: &str = r#"
module Child (clk: input clock, d: input bit<8>, q: output bit<8>) {
    always_ff (clk) { q = d; }
}
module Top (clk: input clock, d: input bit<8>, q: output bit<8>) {
    inst child: Child (clk, d, q);
}
"#;

/// A clock's wiring copies are no wiring once a call of the settle
/// may store an alias or read the clock.
#[test]
fn clock_aliases_written_by_calls_are_not_wiring() {
    let (program, _, _, _, _) = Simulator::builder(ALIASED, "Top")
        .four_state(false)
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    let clock = crate::edge_plans::event_addresses(&program)[0];
    let copies = crate::clock_aliases::extract(&program, clock).expect("wiring copies");
    let alias = copies[0]
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            SIRInstruction::Store(addr, ..) => Some(*addr),
            _ => None,
        })
        .unwrap();
    let d = object(program.sir.eval_apply_ffs.values().flatten(), 8, false);
    let with_call = |program: &LaidOutProgram,
                     effects: celox_sir::CallEffects<RegionedAbsoluteAddr>| {
        let mut program = program.clone();
        let mut unit = caller(FunctionId(0));
        for instruction in unit
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
        {
            if let SIRInstruction::Call { effects: call, .. } = instruction {
                *call = std::sync::Arc::new(effects.clone());
            }
        }
        program.sir.eval_comb.push(unit);
        crate::clock_aliases::extract(&program, clock).is_some()
    };
    assert!(with_call(
        &program,
        celox_sir::CallEffects {
            reads: vec![d],
            ..Default::default()
        }
    ));
    assert!(!with_call(
        &program,
        celox_sir::CallEffects {
            writes: vec![alias],
            ..Default::default()
        }
    ));
    assert!(!with_call(
        &program,
        celox_sir::CallEffects {
            reads: vec![RegionedAbsoluteAddr::from_absolute_addr(
                celox_design::STABLE_REGION,
                clock
            )],
            ..Default::default()
        }
    ));
}
