//! Calls of the coverage helper (`celox_sir::extern_abi::COVERAGE_HELPER_ID`):
//! every backend passes them to the heap as it passes a heap call, and the
//! scheduler does not take them for calls that may read or change simulation
//! state, except in combinational logic, where it plans them as heap calls.
//! No frontend emits them yet, so the tests inject them into the SIR of
//! ordinary designs with heap operations that exist today.
use super::*;
use crate::ir::AbsoluteAddr;
use celox_heap::ops;
use celox_sir::extern_abi::{COVERAGE_HELPER_ID, HEAP_HELPER_ID};

type Instruction = SIRInstruction<RegionedAbsoluteAddr>;

/// The registers a list of instructions is built over, numbered from the
/// first one free in the unit that gets them.
struct Registers {
    next: usize,
    types: Vec<(RegisterId, RegisterType)>,
}

impl Registers {
    fn after(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        Self {
            next: unit.register_map.keys().map(|r| r.0 + 1).max().unwrap_or(0),
            types: Vec::new(),
        }
    }

    fn alloc(&mut self, width: usize) -> RegisterId {
        let register = RegisterId(self.next);
        self.next += 1;
        self.types.push((register, bits(width)));
        register
    }

    fn word(&mut self, instructions: &mut Vec<Instruction>, value: u64) -> RegisterId {
        let register = self.alloc(64);
        instructions.push(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    /// Heap operation `code` on `operands` through helper `func`.
    fn heap_call(
        &mut self,
        instructions: &mut Vec<Instruction>,
        func: u32,
        code: u64,
        operands: [RegisterId; 3],
    ) -> RegisterId {
        let code = self.word(instructions, code);
        let result = self.alloc(64);
        instructions.push(SIRInstruction::ExternCall {
            dst: Some(result),
            func,
            args: [code].into_iter().chain(operands).collect(),
        });
        result
    }

    fn declare(self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
        unit.register_map.extend(self.types);
    }
}

/// The byte the heap gives for input `a`: byte `a & 7` of the string below.
const LETTERS: u64 = u64::from_be_bytes(*b"ABCDEFGH");

fn letter(a: u16) -> u32 {
    u32::from(b"ABCDEFGH"[usize::from(a & 7)])
}

/// A unit that stores into `b` byte `a & 7` of a string it builds, with four
/// heap operations called through helper `func`: a new buffer, the eight
/// bytes appended to it, the buffer frozen into a string, and the byte read
/// from that. The value passes through each operand position and the
/// result, and the handles from one call to the next.
fn letter_unit(
    func: u32,
    a: RegionedAbsoluteAddr,
    b: RegionedAbsoluteAddr,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut unit = block(Vec::new());
    let mut registers = Registers::after(&unit);
    let mut instructions = Vec::new();
    let input = registers.alloc(INPUT);
    instructions.push(SIRInstruction::Load(input, a, SIROffset::Static(0), INPUT));
    let high = registers.alloc(64 - INPUT);
    instructions.push(SIRInstruction::Imm(high, SIRValue::new(0u8)));
    let wide = registers.alloc(64);
    instructions.push(SIRInstruction::Concat(wide, vec![high, input]));
    let seven = registers.word(&mut instructions, 7);
    let index = registers.alloc(64);
    instructions.push(SIRInstruction::Binary(index, wide, BinaryOp::And, seven));
    let zero = registers.word(&mut instructions, 0);
    let letters = registers.word(&mut instructions, LETTERS);
    let count = registers.word(&mut instructions, 8);
    let buffer = registers.heap_call(&mut instructions, func, ops::NEW, [zero; 3]);
    let buffer = registers.heap_call(
        &mut instructions,
        func,
        ops::APPEND_BYTES,
        [buffer, letters, count],
    );
    let string = registers.heap_call(&mut instructions, func, ops::FREEZE, [buffer, zero, zero]);
    let byte = registers.heap_call(&mut instructions, func, ops::GETC, [string, index, zero]);
    let output = registers.alloc(OUTPUT);
    instructions.push(SIRInstruction::Slice(output, byte, 0, OUTPUT));
    instructions.push(SIRInstruction::Store(
        b,
        SIROffset::Static(0),
        OUTPUT,
        output,
        vec![],
        vec![],
    ));
    unit.blocks.get_mut(&BlockId(0)).unwrap().instructions = instructions;
    registers.declare(&mut unit);
    unit
}

/// Run `unit` first in every settle, the fused ones included.
fn settle_first_unit(sir: &mut SirProgram, unit: &ExecutionUnit<RegionedAbsoluteAddr>) {
    sir.updates.observe.insert(0, unit.clone());
    for (_, units) in sir.updates.groups_mut(celox_sir::UpdateEntry::FromInputs) {
        units.insert(0, unit.clone());
    }
}

/// The untimed design's settle computes `b` from `a` with heap operations
/// called through `func`, before the logic that reads `b`: in a unit of the
/// settle, or in a function the settle calls.
fn settle_letters(sir: &mut SirProgram, func: u32, in_function: bool) {
    let a = object(&sir.updates.observe, INPUT, false);
    let b = object(&sir.updates.observe, OUTPUT, false);
    let unit = letter_unit(func, a, b);
    let first = if in_function {
        caller(push_function(sir, unit))
    } else {
        unit
    };
    settle_first_unit(sir, &first);
    // The lane-partitioned settle, where the design has one.
    if let Some(parallel) = &mut sir.parallel
        && !parallel.observe.is_empty()
    {
        parallel
            .observe
            .insert(0, celox_sir::LaneUnit::new(0, first));
    }
    sir.summarize_calls();
}

fn coverage_letters(sir: &mut SirProgram) {
    settle_letters(sir, COVERAGE_HELPER_ID, false);
}

fn coverage_letters_in_a_function(sir: &mut SirProgram) {
    settle_letters(sir, COVERAGE_HELPER_ID, true);
}

fn heap_letters(sir: &mut SirProgram) {
    settle_letters(sir, HEAP_HELPER_ID, false);
}

fn letters(transform: fn(&mut SirProgram)) -> SimulatorBuilder<'static, Simulator> {
    SimulatorBuilder::<Simulator>::from_sv_sources(
        vec![(UNTIMED, std::path::Path::new("coverage-calls.sv"))],
        "Top",
    )
    .four_state(false)
    .transform_sir(transform)
}

fn check_letters<B: crate::SimBackend>(backend: &str, mut sim: Simulator<B>) {
    let (a, z, q, clk) = (
        sim.signal("a"),
        sim.signal("z"),
        sim.signal("q"),
        sim.event("clk"),
    );
    for n in [5u16, 1000, 8191, 2] {
        sim.modify(|io| io.set(a, n)).unwrap();
        let b = letter(n);
        assert_eq!(sim.get_as::<u32>(z), b + 1, "{backend}: z for a = {n}");
        sim.posedge(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), b, "{backend}: q for a = {n}");
        sim.negedge(clk).unwrap();
    }
}

/// A call of the coverage helper reaches the heap's call entry on every
/// backend, with its opcode word and operands in place and its result
/// returned, from a unit and from a function unit: it computes what the
/// same operations compute through the heap helper.
#[test]
fn coverage_helper_calls_reach_the_heap_on_every_backend() {
    let forms: [(&str, fn(&mut SirProgram)); 3] = [
        ("coverage helper", coverage_letters),
        (
            "coverage helper in a function",
            coverage_letters_in_a_function,
        ),
        ("heap helper", heap_letters),
    ];
    for (form, transform) in forms {
        check_letters(
            &format!("interpreter, {form}"),
            letters(transform).build_interpreter().unwrap(),
        );
        #[cfg(feature = "cranelift")]
        check_letters(
            &format!("cranelift, {form}"),
            letters(transform).build_cranelift().unwrap(),
        );
        #[cfg(feature = "wasm-runtime")]
        check_letters(
            &format!("wasm, {form}"),
            letters(transform).build_wasm().unwrap(),
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            check_letters(
                &format!("native, {form}"),
                letters(transform).build_native().unwrap(),
            );
            let lanes = || {
                letters(transform)
                    .threads(4)
                    .parallel_partition(crate::ParallelPartition::Always)
            };
            // The lane-partitioned settle makes the calls too.
            let (program, ..) = lanes()
                .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
                .unwrap();
            let parallel = program.sir.parallel.as_ref().expect("a lane partition");
            assert!(
                parallel
                    .observe
                    .iter()
                    .flat_map(|lane| lane.unit.blocks.values())
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| instruction.host_effects().extern_calls),
                "{form}: the partitioned settle calls the helper"
            );
            check_letters(
                &format!("native with lanes, {form}"),
                lanes().build_native().unwrap(),
            );
        }
    }
}

/// A heap operation without operands or effect, the length of no string,
/// called through helper `func` over registers of `unit`.
fn idle_heap_call(unit: &ExecutionUnit<RegionedAbsoluteAddr>, func: u32) -> Registered {
    let mut registers = Registers::after(unit);
    let mut instructions = Vec::new();
    let zero = registers.word(&mut instructions, 0);
    registers.heap_call(&mut instructions, func, ops::LEN, [zero; 3]);
    Registered {
        instructions,
        registers,
    }
}

/// Instructions and the registers they are over.
struct Registered {
    instructions: Vec<Instruction>,
    registers: Registers,
}

impl Registered {
    fn prepend_to(self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
        let entry = unit.entry_block_id;
        let instructions = &mut unit.blocks.get_mut(&entry).unwrap().instructions;
        instructions.splice(0..0, self.instructions);
        self.registers.declare(unit);
    }
}

/// Every flip-flop unit that evaluates its domain makes a heap call
/// through `func` first, of its own or in a function it calls.
fn ff_heap_call(sir: &mut SirProgram, func: u32, in_function: bool) {
    let function = in_function.then(|| {
        let mut unit = block(Vec::new());
        idle_heap_call(&unit, func).prepend_to(&mut unit);
        push_function(sir, unit)
    });
    use celox_sir::UpdateEntry::{FromInputs, Sample, Transition};
    let mut found = false;
    for event in sir.updates.events.values_mut() {
        for entry in [Transition, FromInputs, Sample] {
            for unit in event.entry_mut(entry).into_iter().flatten() {
                found = true;
                match function {
                    Some(function) => prepend_call(unit, function),
                    None => idle_heap_call(unit, func).prepend_to(unit),
                }
            }
        }
    }
    assert!(found, "the fixture has a flip-flop");
}

fn ff_samples(sir: &mut SirProgram) {
    ff_heap_call(sir, COVERAGE_HELPER_ID, false);
    timed_adder(sir);
}

fn ff_calls_a_sampling_function(sir: &mut SirProgram) {
    ff_heap_call(sir, COVERAGE_HELPER_ID, true);
    timed_adder(sir);
}

fn ff_makes_a_heap_call(sir: &mut SirProgram) {
    ff_heap_call(sir, HEAP_HELPER_ID, false);
    timed_adder(sir);
}

fn ff_calls_a_heap_function(sir: &mut SirProgram) {
    ff_heap_call(sir, HEAP_HELPER_ID, true);
    timed_adder(sir);
}

/// The counter's process makes a heap call through `func` first.
fn process_heap_call(sir: &mut SirProgram, func: u32) {
    let process = incrementer(sir);
    let unit = &mut sir.processes[process];
    idle_heap_call(unit, func).prepend_to(unit);
    timed_adder(sir);
}

fn process_samples(sir: &mut SirProgram) {
    process_heap_call(sir, COVERAGE_HELPER_ID);
}

fn process_makes_a_heap_call(sir: &mut SirProgram) {
    process_heap_call(sir, HEAP_HELPER_ID);
}

/// Every decision of a plan that its summary shows, in an order that does
/// not depend on how the plan's tables were filled.
#[derive(Debug, PartialEq)]
struct Decisions {
    settle_reads: Option<Vec<AbsoluteAddr>>,
    fallback: Vec<Option<String>>,
    derived: Vec<AbsoluteAddr>,
    clocks: Vec<AbsoluteAddr>,
    lazy: Option<Vec<bool>>,
    incremental: bool,
    changed_outputs: Vec<AbsoluteAddr>,
    nba_groups: Vec<Vec<AbsoluteAddr>>,
    publications: Vec<(AbsoluteAddr, u64)>,
    event_groups: Vec<Vec<AbsoluteAddr>>,
    active: Vec<AbsoluteAddr>,
}

fn decisions(summary: &crate::scheduler::PlanSummary) -> Decisions {
    fn sorted<T: Ord + Clone>(items: impl IntoIterator<Item = T>) -> Vec<T> {
        let mut items: Vec<T> = items.into_iter().collect();
        items.sort_unstable();
        items
    }
    Decisions {
        settle_reads: summary
            .settle_reads
            .as_ref()
            .map(|reads| sorted(reads.iter().copied())),
        fallback: summary.fallback.clone(),
        derived: summary.derived.clone(),
        clocks: summary.clocks.clone(),
        lazy: summary.lazy.clone(),
        incremental: summary.incremental,
        changed_outputs: sorted(summary.changed_outputs.iter().copied()),
        nba_groups: summary
            .nba_groups
            .iter()
            .map(|group| sorted(group.iter().copied()))
            .collect(),
        publications: sorted(summary.publications.iter().map(|(&at, &bits)| (at, bits))),
        event_groups: summary.event_groups.clone(),
        active: sorted(summary.active.iter().copied()),
    }
}

/// A coverage helper call in a flip-flop or in a compiled kernel, or in a
/// function either calls, costs the design nothing a heap call there costs
/// it: the plan is the one without the call, in every decision its summary
/// shows. A heap call in the same place is an opaque dependency, with which
/// no settle waits for its readers.
#[test]
fn coverage_helper_calls_keep_the_plan_of_the_design() {
    let (program, base) = plan(timed_adder);
    let base = base.unwrap();
    assert!(
        base.settle_reads.is_some() && base.lazy.is_some(),
        "the fixture's settle is selected from what it reads and deferred"
    );
    let counter = incrementer(&program.sir);
    assert_eq!(base.fallback[counter], None, "the counter is compiled");
    let sampling: [fn(&mut SirProgram); 3] =
        [ff_samples, ff_calls_a_sampling_function, process_samples];
    for transform in sampling {
        let summary = plan(transform).1.unwrap();
        assert_eq!(decisions(&summary), decisions(&base));
    }
    let heap: [fn(&mut SirProgram); 3] = [
        ff_makes_a_heap_call,
        ff_calls_a_heap_function,
        process_makes_a_heap_call,
    ];
    for transform in heap {
        let summary = plan(transform).1.unwrap();
        assert_eq!(summary.lazy, None);
        assert_eq!(summary.fallback, base.fallback);
    }
}

/// The timed design runs to its `$finish` with coverage helper calls in its
/// flip-flop: the sliced settle of the compiled scheduler gives the values
/// the design checks, as the other backends' settles do.
#[test]
fn a_design_with_coverage_helper_calls_in_a_flip_flop_runs() {
    let sampling: [fn(&mut SirProgram); 2] = [ff_samples, ff_calls_a_sampling_function];
    for transform in sampling {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let mut simulation = timed(transform).build_native().unwrap();
            simulation.run_until(10).unwrap();
            assert!(simulation.is_finished());
            assert!(simulation.scheduler_stats().compiled_slots > 0);
        }
        let mut simulation = timed(transform).build_interpreter().unwrap();
        simulation.run_until(10).unwrap();
        assert!(simulation.is_finished());
        #[cfg(feature = "cranelift")]
        {
            let mut simulation = timed(transform).build_cranelift().unwrap();
            simulation.run_until(10).unwrap();
            assert!(simulation.is_finished());
        }
    }
}

/// A design in which a kernel waits for an edge of what combinational logic
/// computes, and in which one domain reads what another assigns with a
/// blocking assignment.
const OBSERVED: &str = r#"
`timescale 1ns/1ns
module Top;
    logic clk = 0, clk2 = 0;
    logic [7:0] d = 0, x = 0, y = 0;
    logic g;
    int count = 0;
    assign g = d[0];
    always #1 clk = ~clk;
    always #3 clk2 = ~clk2;
    always #2 d = d + 8'd1;
    always @(posedge clk) x = d;
    always_ff @(posedge clk2) y <= x;
    initial forever begin @(posedge g); count = count + 1; end
endmodule
"#;

/// The settle makes a heap call through `func` first, of its own or in a
/// function it calls.
fn settle_heap_call(sir: &mut SirProgram, func: u32, in_function: bool) {
    let mut unit = block(Vec::new());
    idle_heap_call(&unit, func).prepend_to(&mut unit);
    let first = if in_function {
        caller(push_function(sir, unit))
    } else {
        unit
    };
    settle_first_unit(sir, &first);
    sir.summarize_calls();
}

fn settle_samples(sir: &mut SirProgram) {
    settle_heap_call(sir, COVERAGE_HELPER_ID, false);
}

fn settle_calls_a_sampling_function(sir: &mut SirProgram) {
    settle_heap_call(sir, COVERAGE_HELPER_ID, true);
}

fn settle_makes_a_heap_call(sir: &mut SirProgram) {
    settle_heap_call(sir, HEAP_HELPER_ID, false);
}

fn observed(transform: fn(&mut SirProgram)) -> SimulatorBuilder<'static, crate::Simulation> {
    crate::Simulation::from_sv_sources(
        vec![(OBSERVED, std::path::Path::new("observed-calls.sv"))],
        "Top",
    )
    .four_state(false)
    .transform_sir(transform)
}

fn observed_plan(transform: fn(&mut SirProgram)) -> crate::scheduler::PlanSummary {
    let (program, ..) = observed(transform)
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    crate::scheduler::plan_for_test(&program).unwrap()
}

/// What `count` and `y` are after `until` time units.
fn observed_values<B: crate::SimBackend>(
    mut simulation: crate::Simulation<B>,
    until: u64,
) -> [num_bigint::BigUint; 2] {
    simulation.run_until(until).unwrap();
    ["count", "y"].map(|name| {
        let signal = simulation.signal(name);
        simulation.get(signal)
    })
}

fn observed_ff_samples(sir: &mut SirProgram) {
    ff_heap_call(sir, COVERAGE_HELPER_ID, false);
    sir.summarize_calls();
}

fn observed_ff_calls_a_sampling_function(sir: &mut SirProgram) {
    ff_heap_call(sir, COVERAGE_HELPER_ID, true);
    sir.summarize_calls();
}

fn observed_ff_makes_a_heap_call(sir: &mut SirProgram) {
    ff_heap_call(sir, HEAP_HELPER_ID, false);
    sir.summarize_calls();
}

/// A domain that assigns with a blocking assignment commits it in the
/// Active region when it makes a heap call, whatever reads the result, and
/// not for a coverage helper call, of its own or in a function it calls:
/// the plan is the one of the design without the call.
#[test]
fn coverage_helper_calls_make_no_active_commit() {
    let base = observed_plan(no_calls);
    assert!(base.active.is_empty());
    let heap = observed_plan(observed_ff_makes_a_heap_call);
    assert!(!heap.active.is_empty());
    let sampling: [fn(&mut SirProgram); 2] =
        [observed_ff_samples, observed_ff_calls_a_sampling_function];
    for transform in sampling {
        assert_eq!(decisions(&observed_plan(transform)), decisions(&base));
    }
}

/// A coverage helper call in combinational logic is exempt from nothing:
/// every settle makes the call again, so the plan is the one of the design
/// with a heap call there. The settle stays complete, the domain whose
/// blocking result another one reads commits it in the Active region, and
/// the kernel that waits for a combinational result is not compiled; none
/// of that holds for the design without the call.
#[test]
fn coverage_helper_calls_in_the_settle_are_planned_as_heap_calls() {
    let base = observed_plan(no_calls);
    assert!(
        base.settle_reads.is_some(),
        "the fixture's settle is sliced"
    );
    assert!(base.active.is_empty());
    assert!(base.fallback.iter().all(Option::is_none));
    let heap = observed_plan(settle_makes_a_heap_call);
    assert_eq!(heap.settle_reads, None);
    assert_eq!(heap.active.len(), 1);
    let fallen: Vec<&str> = heap.fallback.iter().flatten().map(String::as_str).collect();
    assert_eq!(fallen, ["a reactive wait has opaque dependencies"]);
    let sampling: [fn(&mut SirProgram); 2] = [settle_samples, settle_calls_a_sampling_function];
    for transform in sampling {
        let summary = observed_plan(transform);
        assert_eq!(summary.settle_reads, None);
        assert_eq!(summary.active, heap.active);
        assert_eq!(summary.fallback, heap.fallback);
        assert_eq!(decisions(&summary), decisions(&heap));
        assert_ne!(decisions(&summary), decisions(&base));
    }
    // The design computes what it computes without the calls.
    let until = 24;
    let expected = observed_values(observed(no_calls).build_interpreter().unwrap(), until);
    assert!(expected[0] > 0u8.into() && expected[1] > 0u8.into());
    for transform in sampling {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert_eq!(
            observed_values(observed(transform).build_native().unwrap(), until),
            expected
        );
        assert_eq!(
            observed_values(observed(transform).build_interpreter().unwrap(), until),
            expected
        );
        #[cfg(feature = "cranelift")]
        assert_eq!(
            observed_values(observed(transform).build_cranelift().unwrap(), until),
            expected
        );
    }
}
