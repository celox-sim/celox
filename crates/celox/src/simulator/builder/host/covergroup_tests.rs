//! What covergroups (IEEE 1800-2023 clause 19) cost a design: nothing while
//! it constructs none, and for sampling in clocked logic the calls of the
//! coverage helper alone, which leave the plan of the design as it is.
use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SirProgram};
use celox_design::ValueKind;
use celox_heap::coverage;
use celox_sir::extern_abi::{COVERAGE_HELPER_ID, HEAP_HELPER_ID};

fn units(sir: &SirProgram) -> impl Iterator<Item = &ExecutionUnit<RegionedAbsoluteAddr>> {
    sir.updates
        .units()
        .chain(sir.processes.iter())
        .chain(sir.functions.iter().map(|function| &function.unit))
}

fn instructions(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = &SIRInstruction<RegionedAbsoluteAddr>> {
    unit.blocks.values().flat_map(|block| &block.instructions)
}

/// The number of calls of extern function `func` in `sir`.
fn extern_calls(sir: &SirProgram, func: u32) -> usize {
    units(sir)
        .flat_map(instructions)
        .filter(
            |instruction| matches!(instruction, SIRInstruction::ExternCall { func: called, .. } if *called == func),
        )
        .count()
}

/// The facts of a laid-out design that a covergroup must not change.
#[derive(Debug, PartialEq)]
struct Facts {
    /// The handle slots of the design state.
    handles: usize,
    state_objects: usize,
    functions: usize,
    processes: usize,
    instructions: usize,
    heap_calls: usize,
    coverage_calls: usize,
}

fn laid_out(
    builder: crate::SimulatorBuilder<'_, crate::Simulation>,
) -> (crate::ir::LaidOutProgram, Facts) {
    laid_out_in(builder, false)
}

/// As [`laid_out`], in four-state simulation if `four_state`.
fn laid_out_in(
    builder: crate::SimulatorBuilder<'_, crate::Simulation>,
    four_state: bool,
) -> (crate::ir::LaidOutProgram, Facts) {
    let (program, ..) = builder
        .four_state(four_state)
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    let design = &program.runtime().design;
    let facts = Facts {
        handles: design
            .state_objects
            .values()
            .filter(|object| object.value_kind != ValueKind::Bits)
            .count(),
        state_objects: design.state_objects.len(),
        functions: program.sir.functions.len(),
        processes: program.sir.processes.len(),
        instructions: units(&program.sir).flat_map(instructions).count(),
        heap_calls: extern_calls(&program.sir, HEAP_HELPER_ID),
        coverage_calls: extern_calls(&program.sir, COVERAGE_HELPER_ID),
    };
    (program, facts)
}

fn timed(source: &str) -> crate::SimulatorBuilder<'_, crate::Simulation> {
    crate::Simulation::from_sv_sources(vec![(source, std::path::Path::new("covergroup.sv"))], "Top")
}

/// The design of the tests: a counter on a clock and a combinational
/// result, with `items` among the module's items and `packages` before it.
fn rtl(packages: &str, items: &str) -> String {
    format!(
        "`timescale 1ns/1ns
{packages}
module Top(output logic [7:0] y);
    logic clk = 0;
    logic [7:0] d = 0, q = 0;
    int edges = 0;
    always #1 clk = ~clk;
    always #2 d = d + 8'd1;
    always_ff @(posedge clk) q <= q + d;
    assign y = q ^ d;
{items}
endmodule
"
    )
}

/// A covergroup that nothing constructs is free, wherever it is declared: in
/// the module, in a package the module imports, in the compilation unit and
/// in a class of the package, which the module does not construct. The
/// laid-out design has the state, the units and the plan of the design
/// without the covergroups, no handle of a covergroup and no call of the
/// heap.
#[test]
fn unused_covergroup_declaration_is_free() {
    // The package and its class are in both designs: a class brings the
    // variables of its methods to the modules that import it.
    let packages = |covergroups: [&str; 3]| {
        format!(
            "{}
package p;
    {}
    class K;
        int v;
        {}
        function new(); v = 1; endfunction
    endclass
endpackage",
            covergroups[0], covergroups[1], covergroups[2]
        )
    };
    let import = "    import p::*;";
    let (base_program, base) = laid_out(timed(&rtl(&packages([""; 3]), import)));
    assert_eq!((base.heap_calls, base.coverage_calls), (0, 0));
    // With coverpoints, bins and options, which are compiled only where
    // the subroutines of a type are made, and reach a module only with the
    // subroutines it calls.
    let declared = packages([
        "covergroup ucg(int a); option.weight = a; coverpoint a { bins b = {[0:a]}; } endgroup",
        "covergroup pcg with function sample(bit [3:0] s); coverpoint s; endgroup",
        "covergroup kcg(string s = \"k\"); option.name = s; coverpoint v iff (v > 0); \
         coverpoint s.len(); endgroup",
    ]);
    let items = format!(
        "{import}
    covergroup cg(int lo, string name = \"n\") @(posedge clk); coverpoint d; endgroup
    covergroup plain(int hi); coverpoint d {{ bins low = {{[0:hi]}}; }} coverpoint q; endgroup"
    );
    let (program, facts) = laid_out(timed(&rtl(&declared, &items)));
    assert_eq!(facts, base);
    let plan = |program| crate::scheduler::plan_for_test(program).unwrap();
    let (base_plan, plan) = (plan(&base_program), plan(&program));
    assert!(base_plan.settle_reads.is_some());
    assert_eq!(plan.settle_reads, base_plan.settle_reads);
    assert_eq!(plan.fallback, base_plan.fallback);
    assert_eq!(plan.lazy, base_plan.lazy);
    assert_eq!(plan.incremental, base_plan.incremental);
    // Without a class the design has no handle at all.
    let (_, plain) = laid_out(timed(&rtl(
        "covergroup ucg(int a); coverpoint a; endgroup
package q;
    covergroup pcg(string s); option.comment = s; coverpoint s.len(); endgroup
endpackage",
        "    import q::*;
    covergroup plain(string s); coverpoint d + s.len(); endgroup",
    )));
    let (_, none) = laid_out(timed(&rtl("", "")));
    assert_eq!(plain, none);
    assert_eq!(none.handles, 0);
    // Nor when the package and the compilation unit have variables that
    // the module uses: the modules that hold their state take the
    // subroutines of a covergroup, with the handles among their locals,
    // only when their own code calls them.
    let variables = |covergroups: [&str; 2]| {
        format!(
            "{}
int ucount = 0;
package q;
    {}
    int qcount = 0;
endpackage",
            covergroups[0], covergroups[1]
        )
    };
    let counting = "    import q::*;
    always @(posedge clk) begin qcount++; ucount++; end";
    let (_, declared) = laid_out(timed(&rtl(
        &variables([
            "covergroup ucg(string s = \"u\"); option.name = s; coverpoint s.len(); endgroup",
            "covergroup pcg(string s = \"p\"); coverpoint s.len() + 1; endgroup",
        ]),
        counting,
    )));
    let (_, none) = laid_out(timed(&rtl(&variables([""; 2]), counting)));
    assert_eq!(declared, none);
    assert_eq!(none.handles, 0);
}

/// `$get_coverage` of a design that constructs no covergroup is the constant
/// 100 (IEEE 1800-2023 19.11): no call of the heap, also with covergroups
/// that are declared and never constructed. With a construction anywhere in
/// the design, here in another module, it asks the heap.
#[test]
fn get_coverage_without_covergroups_makes_no_heap_call() {
    let read = "    real r;
    always @(posedge clk) r = $get_coverage();";
    let (_, facts) = laid_out(timed(&rtl("", read)));
    assert_eq!((facts.heap_calls, facts.coverage_calls), (0, 0));
    let declared = format!("{read}\n    covergroup cg; endgroup");
    let (_, facts) = laid_out(timed(&rtl(
        "package p; covergroup pcg; endgroup endpackage",
        &declared,
    )));
    assert_eq!((facts.heap_calls, facts.coverage_calls), (0, 0));
    // Nor does the state of a package whose variables construct none.
    let counted = format!(
        "{read}
    import p::*;
    always @(posedge clk) count++;"
    );
    let (_, facts) = laid_out(timed(&rtl(
        "package p; int count = 0; covergroup pcg; endgroup endpackage",
        &counted,
    )));
    assert_eq!((facts.heap_calls, facts.coverage_calls), (0, 0));
    // A variable of a package that its declaration constructs is an
    // instance, though no module has the constructor.
    let sampled = format!("{read}\n    initial p::made.sample();");
    let (program, _) = laid_out(timed(&rtl(
        "package p; covergroup pcg; endgroup pcg made = new; endpackage",
        &sampled,
    )));
    let overall = coverage::code(coverage::op::OVERALL, 0, 0);
    assert!(coverage_operations(&program.sir, overall) > 0);
    let constructing = "module Sub;
    covergroup cg; endgroup
    cg c;
    initial c = new;
endmodule";
    let instantiating = format!("{read}\n    Sub sub();");
    let (program, facts) = laid_out(timed(&rtl(constructing, &instantiating)));
    assert!(facts.coverage_calls > 0);
    assert!(coverage_operations(&program.sir, overall) > 0);
}

/// The constant is chosen from the whole design, not from the source file
/// that calls `$get_coverage`: with the covergroup in another file than the
/// call, the call asks the heap when that file constructs the covergroup,
/// in a module or by the declaration of a variable of a package, and is the
/// constant when the file only declares it.
#[test]
fn get_coverage_folds_over_every_source_file() {
    let reading = rtl(
        "",
        "    real r;
    always @(posedge clk) r = $get_coverage();
    Sub sub();",
    );
    let other = |package: &str, items: &str| {
        format!(
            "`timescale 1ns/1ns
{package}
module Sub;
    covergroup cg; endgroup
    cg c;
{items}
endmodule
"
        )
    };
    let overall = coverage::code(coverage::op::OVERALL, 0, 0);
    let built = |other: &str, reading_first: bool| {
        let mut sources = vec![
            (reading.as_str(), std::path::Path::new("reading.sv")),
            (other, std::path::Path::new("other.sv")),
        ];
        if !reading_first {
            sources.reverse();
        }
        let (program, facts) = laid_out(crate::Simulation::from_sv_sources(sources, "Top"));
        (coverage_operations(&program.sir, overall), facts)
    };
    for reading_first in [true, false] {
        let (asked, facts) = built(&other("", ""), reading_first);
        assert_eq!(
            (asked, facts.coverage_calls, facts.heap_calls),
            (0, 0, 0),
            "declared only, reading first: {reading_first}"
        );
        let (asked, _) = built(&other("", "    initial c = new;"), reading_first);
        assert!(asked > 0, "constructed, reading first: {reading_first}");
        let (asked, _) = built(
            &other(
                "package p; covergroup pcg; endgroup pcg made = new; endpackage",
                "    initial p::made.sample();",
            ),
            reading_first,
        );
        assert!(asked > 0, "package state, reading first: {reading_first}");
    }
}

/// The number of calls of the coverage helper with opcode word `code` in
/// `sir`.
fn coverage_operations(sir: &SirProgram, code: u64) -> usize {
    let code = celox_sir::SIRValue::new(num_bigint::BigUint::from(code));
    units(sir)
        .map(|unit| {
            let constants: crate::HashMap<_, _> = instructions(unit)
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Imm(register, value) => Some((*register, value)),
                    _ => None,
                })
                .collect();
            instructions(unit)
                .filter(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::ExternCall { func, args, .. }
                            if *func == COVERAGE_HELPER_ID
                                && args.first().and_then(|code| constants.get(code).copied())
                                    == Some(&code)
                    )
                })
                .count()
        })
        .sum()
}

/// A package class that constructs its embedded covergroup: the analysis of
/// the package lowers the class, and the analysis of the package for a
/// specialization that a module asks for (`p::P#(bit)`) takes that lowering
/// over without binding the constructor again (the analyzer's
/// `classes_taken_over_keep_their_covergroup_constructors` pins that `K` is
/// taken over here). `$get_coverage` still sees that the design constructs a
/// covergroup, with one module using the classes and with two, and with the
/// specialization asked for by another module than the one that constructs
/// `K`.
#[test]
fn get_coverage_sees_instances_of_a_replayed_package_class() {
    let package = "`timescale 1ns/1ns
package p;
    class K;
        covergroup cg; endgroup
        function new(); cg = new; endfunction
    endclass
    class P #(type T = int);
        T v;
        function T get(); return v; endfunction
    endclass
endpackage";
    let overall = coverage::code(coverage::op::OVERALL, 0, 0);
    let one = format!(
        "{package}
module Top(output longint unsigned all);
    p::P#(bit) x = new;
    p::K k = new;
    initial all = $realtobits($get_coverage());
endmodule
"
    );
    let (program, facts) = laid_out(timed(&one));
    assert!(facts.coverage_calls > 0);
    assert!(coverage_operations(&program.sir, overall) > 0);
    for (in_a, in_b) in [
        ("    p::P#(bit) x = new;", ""),
        ("", "    p::P#(bit) x = new;"),
        ("    p::P#(bit) x = new;", "    p::P#(logic [3:0]) y = new;"),
    ] {
        let two = format!(
            "{package}
module A;
{in_a}
    p::K k = new;
endmodule
module B(output longint unsigned all);
{in_b}
    initial all = $realtobits($get_coverage());
endmodule
module Top(output longint unsigned seen);
    A a();
    B b(.all(seen));
endmodule
"
        );
        let (program, facts) = laid_out(timed(&two));
        assert!(facts.coverage_calls > 0, "{in_a} / {in_b}");
        assert!(
            coverage_operations(&program.sir, overall) > 0,
            "{in_a} / {in_b}"
        );
    }
}

/// Sampling a covergroup from clocked logic leaves the plan of the design
/// as it is: the settle stays selected from the storage it reads and
/// deferred until something reads it, the kernels stay compiled. A call of
/// the heap helper there would take the deferred settles from the design
/// (the scheduler's `coverage_helper_calls_keep_the_plan_of_the_design`).
/// The plan is that of the design that counts the edges instead, with the
/// covergroup constructed in the clocked procedure, and with the
/// subroutines of the type forced out of line, where a function unit makes
/// the calls. Constructed by its declaration, the covergroup adds the
/// kernel of the initializer, which runs once, and changes nothing else.
#[test]
fn coverage_calls_keep_the_plan_of_the_design() {
    let plan = |source: &str, forced: bool| {
        let builder = timed(source);
        let builder = if forced {
            builder.force_out_of_line_calls()
        } else {
            builder
        };
        let (program, facts) = laid_out(builder);
        (
            crate::scheduler::plan_for_test(&program).unwrap(),
            facts,
            program,
        )
    };
    let counting = rtl("", "    always @(posedge clk) edges++;");
    let lazy = rtl(
        "",
        "    covergroup cg; endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end",
    );
    let declared = rtl(
        "",
        "    covergroup cg; endgroup
    cg c = new;
    always @(posedge clk) c.sample();",
    );
    for forced in [false, true] {
        let (base, base_facts, _) = plan(&counting, forced);
        assert!(
            base.settle_reads.is_some() && base.lazy.is_some(),
            "the fixture's settle is selected from what it reads and deferred"
        );
        assert_eq!(base_facts.coverage_calls, 0);
        let (sampling, facts, program) = plan(&lazy, forced);
        assert!(facts.coverage_calls > 0);
        // Constructing and sampling make no call of the heap helper.
        assert_eq!(facts.heap_calls, 0, "forced: {forced}");
        assert_eq!(sampling.settle_reads.is_some(), base.settle_reads.is_some());
        assert_eq!(sampling.fallback, base.fallback, "forced: {forced}");
        assert_eq!(sampling.lazy, base.lazy, "forced: {forced}");
        assert_eq!(sampling.incremental, base.incremental);
        assert_eq!(sampling.active, base.active);
        // The registration of the type is in a function unit of its own
        // whatever is forced, and with everything out of line a function
        // unit samples.
        let in_functions = program
            .sir
            .functions
            .iter()
            .flat_map(|function| instructions(&function.unit))
            .filter(|instruction| {
                matches!(instruction, SIRInstruction::ExternCall { func, .. } if *func == COVERAGE_HELPER_ID)
            })
            .count();
        assert!(in_functions > 0, "forced: {forced}");
        if forced {
            assert_eq!(in_functions, facts.coverage_calls);
        }
        let (sampling, facts, _) = plan(&declared, forced);
        assert!(facts.coverage_calls > 0);
        assert_eq!(facts.heap_calls, 0, "forced: {forced}");
        assert_eq!(sampling.settle_reads.is_some(), base.settle_reads.is_some());
        let (initializer, kernels) = sampling.fallback.split_first().unwrap();
        assert_eq!(initializer.as_deref(), Some("a declaration initializer"));
        assert_eq!(kernels, base.fallback, "forced: {forced}");
        assert_eq!(
            sampling.lazy.as_ref().map(|kernels| &kernels[1..]),
            base.lazy.as_deref(),
            "forced: {forced}"
        );
        assert_eq!(sampling.incremental, base.incremental);
        assert_eq!(sampling.active, base.active);
    }
}

/// Sampling from combinational logic is not free: each evaluation of the
/// settle makes the call again, so the settle is the complete one, as with
/// any call into the host there. Pinned so that a change is noticed.
#[test]
fn sample_in_always_comb_drops_settle_slicing() {
    let (base_program, _) = laid_out(timed(&rtl("", "")));
    assert!(
        crate::scheduler::plan_for_test(&base_program)
            .unwrap()
            .settle_reads
            .is_some()
    );
    let sampling = rtl(
        "",
        "    covergroup cg; endgroup
    cg c;
    logic [7:0] z;
    always_comb begin
        z = q + 8'd1;
        c.sample();
    end",
    );
    let (program, facts) = laid_out(timed(&sampling));
    assert!(facts.coverage_calls > 0);
    assert_eq!(
        crate::scheduler::plan_for_test(&program)
            .unwrap()
            .settle_reads,
        None
    );
}

/// The calls of the coverage helper in `sir`: the operation of each (see
/// `celox_heap::coverage::op`) with its two immediate operands. A clocked
/// procedure that takes the body of a subroutine in has its calls in each
/// of its units, so the calls of one execution are these divided by the
/// number of times the body is there.
fn coverage_subs(sir: &SirProgram) -> Vec<(u64, u64, u64)> {
    units(sir)
        .flat_map(|unit| {
            let constants: crate::HashMap<_, _> = instructions(unit)
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Imm(register, value) => Some((*register, value)),
                    _ => None,
                })
                .collect();
            instructions(unit)
                .filter_map(|instruction| match instruction {
                    SIRInstruction::ExternCall { func, args, .. }
                        if *func == COVERAGE_HELPER_ID =>
                    {
                        let word = constants
                            .get(args.first()?)?
                            .payload
                            .iter_u64_digits()
                            .next()
                            .unwrap_or(0);
                        Some((
                            word >> 8 & 0xff,
                            word >> 16 & 0xff_ffff,
                            word >> 40 & 0xff_ffff,
                        ))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The number of calls of coverage operation `sub` among `calls`.
fn count(calls: &[(u64, u64, u64)], sub: u64) -> usize {
    calls.iter().filter(|(called, ..)| *called == sub).count()
}

/// A sample hands the heap the values of its coverpoints in one call for
/// each 64 bits of them, and one more for a group of them that has a guard:
/// 96 coverpoints of one bit are two calls, three with the guards of the
/// first 64, between the call that asks whether the instance samples and
/// the one that counts. A coverpoint of more than 64 bits takes a call for
/// each word of its own.
#[test]
fn packed_points_take_a_call_for_each_64_bits() {
    use coverage::op;
    let points = |wide: bool| {
        (0..96)
            .map(|index| {
                let guard = if index < 64 && index % 3 == 0 {
                    " iff (en)"
                } else {
                    ""
                };
                let value = if wide {
                    format!("{{64'd0, data[{index}]}}")
                } else {
                    format!("data[{index}]")
                };
                format!("        p{index}: coverpoint {value}{guard};\n")
            })
            .collect::<String>()
    };
    let design = |wide: bool| {
        rtl(
            "",
            &format!(
                "    bit [95:0] data;
    bit en;
    covergroup cg;
{}    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end",
                points(wide)
            ),
        )
    };
    let (program, facts) = laid_out(timed(&design(false)));
    assert_eq!(facts.heap_calls, 0);
    let calls = coverage_subs(&program.sir);
    // The body that samples is in the design this many times.
    let bodies = count(&calls, op::SAMPLING);
    assert!(bodies > 0);
    let mut sampling: Vec<_> = calls
        .iter()
        .filter(|(sub, ..)| {
            [
                op::SAMPLING,
                op::PRESENT,
                op::POINTS,
                op::POINT,
                op::SAMPLE_END,
            ]
            .contains(sub)
        })
        .copied()
        .collect();
    // The blocks of a unit are in no order.
    sampling.sort_unstable();
    let mut expected: Vec<_> = [
        (op::SAMPLING, 0, 0),
        (op::PRESENT, 1, 64),
        (op::POINTS, 1, 64),
        (op::POINTS, 65, 32),
        (op::SAMPLE_END, 0, 0),
    ]
    .into_iter()
    .flat_map(|call| std::iter::repeat_n(call, bodies))
    .collect();
    expected.sort_unstable();
    assert_eq!(sampling, expected);
    // The same values as coverpoints of 65 bits: two words each, and
    // still no call of the heap helper.
    let (program, facts) = laid_out(timed(&design(true)));
    assert_eq!(facts.heap_calls, 0);
    let calls = coverage_subs(&program.sir);
    assert_eq!(
        count(&calls, op::POINT),
        2 * 96 * count(&calls, op::SAMPLING)
    );
    assert_eq!(count(&calls, op::POINTS) + count(&calls, op::PRESENT), 0);
}

/// What the bins of 19.5.1 to 19.5.6 add to a sample and to a construction.
/// A bin array, a `default` bin, `ignore_bins` and a wildcard bin add
/// nothing: the heap makes them of the descriptor. The guards of the bins
/// of a coverpoint are one more call of a sample for each 64 of them, and a
/// `with` filter is two calls of a construction for each value it is asked
/// about, the call that goes to the next value and the one that reads it,
/// in a loop of the constructor. None is a call of the heap helper, so a
/// design that samples such a covergroup in clocked logic keeps its plan.
/// A type with an illegal bin has the report of a hit in its sample: the
/// call that reads the text, which only a hit makes, and a string handle
/// for it in the state of the procedure that samples.
#[test]
fn bin_kinds_add_the_calls_of_their_predicates_and_filters() {
    use coverage::op;
    let design = |bins: &str| {
        rtl(
            "",
            &format!(
                "    bit [3:0] v;
    bit en;
    covergroup cg;
        coverpoint v {{
{bins}
        }}
        coverpoint q;
    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end"
            ),
        )
    };
    // What the plan decides for the design, as far as a call of the heap
    // helper in its clocked logic would change it.
    let plan = |source: &str| {
        let (program, facts) = laid_out(timed(source));
        let calls = coverage_subs(&program.sir);
        let plan = crate::scheduler::plan_for_test(&program).unwrap();
        (
            (
                plan.settle_reads.is_some(),
                plan.fallback,
                plan.lazy,
                plan.incremental,
                plan.active,
            ),
            facts,
            calls,
        )
    };
    // The calls of one execution of the body that samples.
    let of_a_sample = |calls: &[(u64, u64, u64)]| {
        let bodies = count(calls, op::SAMPLING);
        assert!(bodies > 0);
        [
            op::SAMPLING,
            op::PRESENT,
            op::POINTS,
            op::POINT,
            op::PREDICATES,
            op::SAMPLE_END,
            op::MESSAGE,
        ]
        .map(|sub| {
            // `MESSAGE` is also the call of a constructor that warns.
            let calls = count(calls, sub);
            assert_eq!(calls % bodies, 0);
            calls / bodies
        })
    };
    let (base, base_facts, base_calls) = plan(&design("            bins low = {[0:3]};"));
    assert!(base.0 && base.2.is_some());
    assert_eq!(base_facts.heap_calls, 0);
    assert_eq!(of_a_sample(&base_calls), [1, 0, 1, 0, 0, 1, 0]);
    // Arrays, a `default` bin, an ignore bin and a wildcard bin: the calls
    // of the design with one bin, in the sample and in the constructor.
    let (kinds, facts, calls) = plan(&design(
        "            bins low[] = {[0:3]};
            bins some[2] = {[4:11]};
            wildcard bins w = {4'b11??};
            ignore_bins ig = {5};
            bins rest[] = default;",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 0, 1, 0]);
    let constructing = |calls: &[(u64, u64, u64)]| {
        [
            op::NEW,
            op::INPUT,
            op::SET_PUSH,
            op::FILTER_NEXT,
            op::FILTER_ITEM,
            op::BUILD,
        ]
        .map(|sub| count(calls, sub))
    };
    assert_eq!(constructing(&calls), constructing(&base_calls));
    assert_eq!(constructing(&calls)[2..5], [0, 0, 0]);
    assert_eq!(kinds, base);
    // Guards of bins: one call for the predicate bits of the coverpoint.
    let (guarded, facts, calls) = plan(&design(
        "            bins low[] = {[0:3]} iff (en);
            bins high = {[4:15]} iff (!en);",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 1, 1, 0]);
    assert!(calls.contains(&(op::PREDICATES, 1, 0)));
    assert_eq!(guarded, base);
    // A filter: the constructor has the call before its loop and the two
    // of the loop, which run once for each of the sixteen values.
    let (filtered, facts, calls) = plan(&design(
        "            bins even[] = {[0:15]} with (item % 2 == 0);",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 0, 1, 0]);
    let news = count(&calls, op::NEW);
    assert_eq!(constructing(&calls)[2..5], [0, 2 * news, news], "{calls:?}");
    assert_eq!(filtered, base);
    // An illegal bin: the sample reads the text of a hit and reports it.
    let (illegal, facts, calls) = plan(&design(
        "            bins low = {[0:3]};
            illegal_bins bad = {15};",
    ));
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 0, 1, 1]);
    // The report is no call of the heap helper, and the plan is that of
    // the design without the bin. The text is a string the procedure
    // holds: one more handle in the state of the design.
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(illegal, base);
    assert_eq!(facts.handles, base_facts.handles + 1);
    // Declared and never constructed, such a covergroup is free as any.
    let array = "    localparam int S [2] = '{1, 2};";
    let (_, declared) = laid_out(timed(&rtl(
        "",
        &format!(
            "{array}
    covergroup cg(int n);
        coverpoint d {{
            bins a[] = {{[0:7]}} with (item < n);
            bins s[n] = S;
            wildcard ignore_bins w = {{8'b1???_???1}};
            illegal_bins bad = {{[9 +/- 1]}} iff (q[0]);
            bins rest = default;
        }}
    endgroup"
        ),
    )));
    let (_, none) = laid_out(timed(&rtl("", array)));
    assert_eq!(declared, none);
}

/// What crosses add to a sample (19.6): a cross is counted by the heap from
/// the values the sample hands over for its coverpoints, so it adds no call
/// of its own, also with bins and `intersect` lists, which the heap makes of
/// the descriptor. A variable it crosses is one more coverpoint, whose value
/// joins those of the others in their call while they fit 64 bits. The
/// guards of a cross and of its bins are one call for the cross. None is a
/// call of the heap helper, so the plan is that of the design without the
/// cross; a cross with an illegal bin has the report of a hit, as a
/// coverpoint with one has.
#[test]
fn crosses_add_a_call_for_their_guards_only() {
    use coverage::op;
    let design = |crosses: &str| {
        rtl(
            "",
            &format!(
                "    bit [3:0] v;
    bit en;
    covergroup cg;
        cv: coverpoint v {{ bins low[] = {{[0:3]}}; bins high = {{[4:15]}}; }}
        cq: coverpoint q;
{crosses}
    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end"
            ),
        )
    };
    let plan = |source: &str| {
        let (program, facts) = laid_out(timed(source));
        let calls = coverage_subs(&program.sir);
        let plan = crate::scheduler::plan_for_test(&program).unwrap();
        (
            (
                plan.settle_reads.is_some(),
                plan.fallback,
                plan.lazy,
                plan.incremental,
                plan.active,
            ),
            facts,
            calls,
        )
    };
    let of_a_sample = |calls: &[(u64, u64, u64)]| {
        let bodies = count(calls, op::SAMPLING);
        assert!(bodies > 0);
        [
            op::SAMPLING,
            op::PRESENT,
            op::POINTS,
            op::POINT,
            op::PREDICATES,
            op::SAMPLE_END,
            op::MESSAGE,
        ]
        .map(|sub| {
            let calls = count(calls, sub);
            assert_eq!(calls % bodies, 0);
            calls / bodies
        })
    };
    let constructing =
        |calls: &[(u64, u64, u64)]| [op::NEW, op::INPUT, op::BUILD].map(|sub| count(calls, sub));
    let (base, base_facts, base_calls) = plan(&design(""));
    assert!(base.0 && base.2.is_some());
    assert_eq!(base_facts.heap_calls, 0);
    assert_eq!(of_a_sample(&base_calls), [1, 0, 1, 0, 0, 1, 0]);
    // Crosses of the coverpoints, with bins and constant `intersect`
    // lists, and of a variable: the calls of the design without them.
    let (crossed, facts, calls) = plan(&design(
        "        x: cross cv, cq { bins b = binsof(cv.low) && binsof(cq) intersect {[0:3]}; \
         ignore_bins i = !binsof(cq) intersect {7}; }
        y: cross cv, en;",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 0, 1, 0]);
    // `v`, `q` and `en` in one call of three items.
    assert!(calls.contains(&(op::POINTS, 1, 3)), "{calls:?}");
    assert_eq!(constructing(&calls), constructing(&base_calls));
    assert_eq!(crossed, base);
    // The guards of a cross and of its bins: one call for the cross, the
    // fourth item, after the coverpoints.
    let (guarded, facts, calls) = plan(&design(
        "        x: cross cv, cq iff (en) { bins b = binsof(cv.low) iff (!en); }
        y: cross cv, cq;",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 1, 1, 0]);
    assert!(calls.contains(&(op::PREDICATES, 3, 0)), "{calls:?}");
    assert_eq!(guarded, base);
    // An illegal cross product: the sample reads the text of a hit.
    let (illegal, facts, calls) = plan(&design(
        "        x: cross cv, cq { illegal_bins bad = binsof(cv.high) && binsof(cq) intersect {0}; }",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), [1, 0, 1, 0, 0, 1, 1]);
    assert_eq!(illegal, base);
    assert_eq!(facts.handles, base_facts.handles + 1);
    // Declared and never constructed, a covergroup with crosses is free.
    let (_, declared) = laid_out(timed(&rtl(
        "",
        "    bit [1:0] a, b;
    covergroup cg(int n);
        ca: coverpoint a;
        x: cross ca, b iff (q[0]) { bins s = binsof(b) intersect {n}; illegal_bins i = binsof(ca) intersect {3}; }
    endgroup",
    )));
    let (_, none) = laid_out(timed(&rtl("", "    bit [1:0] a, b;")));
    assert_eq!(declared, none);
}

/// A coverpoint, a bin operand and an argument of more than 64 bits are
/// handed to the heap in words, with coverage operations alone: nothing of
/// a sample or of a construction is put in a collection of the heap, which
/// a call of the heap helper would make, in two-state and in four-state
/// simulation. The covergroup is that of `wide_coverpoint_planes` of the
/// SystemVerilog tests, with an argument that a coverpoint reads beside
/// the one that a bin reads.
#[test]
fn wide_points_and_operands_make_no_heap_call() {
    use coverage::op;
    let design = rtl(
        "",
        "    bit [64:0] w65;
    logic [127:0] w128;
    bit signed [99:0] s100;
    bit [1:0] narrow;
    covergroup cg(bit [127:0] limit, logic [127:0] offset);
        coverpoint w65 {
            bins top = {65'h1_0000_0000_0000_0000};
            bins low = {[0:65'hffff_ffff_ffff_ffff]};
        }
        coverpoint narrow;
        coverpoint w128 iff (narrow != 2) {
            bins big = {[128'h1_0000_0000_0000_0000_0000_0000:$]};
            bins bound = {[0:limit]};
            bins exact = {128'hdead_beef_0000_0000_0000_0000_0000_0001};
        }
        coverpoint s100 {
            bins negative = {[$:-1]};
            bins positive = {[1:$]};
        }
        coverpoint w128 ^ offset;
    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new(128'h1234_0000_0000_0000_0000, 128'd1);
        w65 = w65 + 65'd1;
        w128 = {w128[126:0], 1'b1};
        s100 = s100 - 100'sd3;
        narrow = narrow + 2'd1;
        c.sample();
    end",
    );
    for four_state in [false, true] {
        let (program, facts) = laid_out_in(timed(&design), four_state);
        assert_eq!(facts.heap_calls, 0, "four_state: {four_state}");
        let calls = coverage_subs(&program.sir);
        // The bodies that construct and that sample are in the design
        // this many times.
        let (constructing, sampling) = (count(&calls, op::NEW), count(&calls, op::SAMPLING));
        assert!(constructing > 0 && sampling > 0);
        // Two words for each of the four coverpoints of more than 64
        // bits and one call for the one of two bits; the argument a bin
        // reads is an input of two words, and the one a coverpoint reads
        // is stored in two words and read in two, with two more for its
        // x and z bits in four-state simulation.
        assert_eq!(
            (
                count(&calls, op::POINT),
                count(&calls, op::POINTS),
                count(&calls, op::PRESENT),
                count(&calls, op::INPUT),
                count(&calls, op::FORMAL_SET),
                count(&calls, op::FORMAL),
            ),
            (
                8 * sampling,
                sampling,
                0,
                2 * constructing,
                2 * constructing,
                if four_state { 4 } else { 2 } * sampling,
            ),
            "four_state: {four_state}, {constructing} constructing, {sampling} sampling"
        );
    }
}

/// An integral argument of a covergroup that its coverpoints read is kept
/// in the record of the instance: a construction stores it and a sample
/// reads it with coverage operations, so that a covergroup of a module
/// with such arguments makes no call of the heap helper and leaves the plan
/// of a design that samples it in clocked logic as it is. A string argument
/// is a property of the object of the instance, read with the heap helper.
#[test]
fn integral_formals_are_read_without_object_loads() {
    use coverage::op;
    let design = |formals: &str, points: &str, actuals: &str| {
        rtl(
            "",
            &format!(
                "    covergroup cg({formals});
{points}
    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) c = new({actuals});
        c.sample();
    end"
            ),
        )
    };
    let integral = design(
        "int lo, logic [70:0] wide, bit [3:0] unread",
        "        coverpoint d + lo[7:0];
        coverpoint wide[70:60] { bins some = {[1:$]}; }
        coverpoint q { bins low = {[0:unread]}; }",
        "3, 71'd5, 4'd9",
    );
    let (program, facts) = laid_out(timed(&integral));
    assert_eq!(facts.heap_calls, 0);
    let calls = coverage_subs(&program.sir);
    // The bodies that construct and that sample are in the design this
    // many times.
    let (constructing, sampling) = (count(&calls, op::NEW), count(&calls, op::SAMPLING));
    assert!(constructing > 0 && sampling > 0);
    // The two arguments the coverpoints read: one word, and two words.
    assert_eq!(count(&calls, op::FORMAL_SET), 3 * constructing);
    assert_eq!(count(&calls, op::FORMAL), 3 * sampling);
    // The argument only a bin reads is an input of the construction.
    assert_eq!(count(&calls, op::INPUT), constructing);
    let plan = |program| crate::scheduler::plan_for_test(program).unwrap();
    let (base_program, _) = laid_out(timed(&rtl("", "    always @(posedge clk) edges++;")));
    let (base, sampled) = (plan(&base_program), plan(&program));
    assert!(base.settle_reads.is_some());
    assert_eq!(sampled.settle_reads.is_some(), base.settle_reads.is_some());
    assert_eq!(sampled.fallback, base.fallback);
    assert_eq!(sampled.lazy, base.lazy);
    assert_eq!(sampled.incremental, base.incremental);
    let string = design("string s", "        coverpoint d + s.len();", "\"ab\"");
    let (program, facts) = laid_out(timed(&string));
    assert!(facts.heap_calls > 0);
    assert_eq!(count(&coverage_subs(&program.sir), op::FORMAL), 0);
}

/// What the descriptor of a covergroup type costs a design: the function
/// that registers the type hands the heap its text in calls of eight bytes,
/// once for the type, whatever constructs and samples it. A bin of a value
/// is about 25 calls and 27 instructions there.
#[test]
fn descriptor_of_a_thousand_bins_is_registered_once() {
    use coverage::op;
    let design = |bins: usize| {
        let bins = (0..bins)
            .map(|index| format!("            bins b{index} = {{{index}}};\n"))
            .collect::<String>();
        rtl(
            "",
            &format!(
                "    bit [15:0] v;
    covergroup cg;
        coverpoint v {{
{bins}        }}
    endgroup
    cg c, e;
    always @(posedge clk) begin
        if (c == null) begin c = new; e = new; end
        c.sample();
        e.sample();
    end"
            ),
        )
    };
    let (small_program, small) = laid_out(timed(&design(1)));
    let (program, facts) = laid_out(timed(&design(1000)));
    let bytes = |program: &crate::ir::LaidOutProgram| {
        count(&coverage_subs(&program.sir), op::REGISTER_BYTES)
    };
    assert_eq!(count(&coverage_subs(&program.sir), op::REGISTER), 1);
    // About 203 bytes of text for a bin of one value: 25 calls. A call is
    // lowered with the words of its operation, of its eight bytes and of
    // the operand it has not; the optimizer keeps one of each value, and
    // of the words of the text about one in ten differs from all others.
    let calls = bytes(&program) - bytes(&small_program);
    let instructions = facts.instructions - small.instructions;
    assert!(
        (25_200..25_450).contains(&calls) && (calls..calls + calls / 8).contains(&instructions),
        "{instructions} instructions and {calls} calls for 999 bins ({} and {} for the design)",
        facts.instructions,
        bytes(&program)
    );
    assert_eq!(facts.functions, small.functions);
}

/// Options written in a procedure, `start()`, `stop()` and the queries of
/// items are coverage operations where they are written (IEEE 1800-2023
/// 19.7, 19.8): a sample makes the calls it makes without them, and none is
/// a call of the heap helper, so the plan of a design that uses them in its
/// clocked logic is that of the design without them. A covergroup that does
/// not call `get_coverage` or `get_inst_coverage` with two arguments has no
/// subroutine for it. (A string member takes a string, which is made as
/// any string is.)
#[test]
fn options_and_methods_add_no_call_to_a_sample() {
    use coverage::op;
    let design = |setup: &str, after: &str| {
        rtl(
            "",
            &format!(
                "    bit [3:0] v;
    int covered, total;
    real r;
    covergroup cg;
        cv: coverpoint v {{ bins low[] = {{[0:3]}}; bins high = {{[4:15]}}; }}
        cq: coverpoint q;
        x: cross cv, cq;
    endgroup
    cg c;
    always @(posedge clk) begin
        if (c == null) begin
            c = new;
{setup}
        end
        c.sample();
{after}
    end"
            ),
        )
    };
    let plan = |source: &str| {
        let (program, facts) = laid_out(timed(source));
        let calls = coverage_subs(&program.sir);
        let plan = crate::scheduler::plan_for_test(&program).unwrap();
        (
            (
                plan.settle_reads.is_some(),
                plan.fallback,
                plan.lazy,
                plan.incremental,
                plan.active,
            ),
            facts,
            calls,
            program.sir.functions.len(),
        )
    };
    let of_a_sample = |calls: &[(u64, u64, u64)]| {
        let bodies = count(calls, op::SAMPLING);
        assert!(bodies > 0);
        [
            op::SAMPLING,
            op::PRESENT,
            op::POINTS,
            op::POINT,
            op::PREDICATES,
            op::SAMPLE_END,
            op::MESSAGE,
        ]
        .map(|sub| {
            let calls = count(calls, sub);
            assert_eq!(calls % bodies, 0);
            calls / bodies
        })
    };
    let (base, base_facts, base_calls, base_functions) = plan(&design("", ""));
    assert!(base.0 && base.2.is_some());
    assert_eq!(base_facts.heap_calls, 0);
    let per_sample = of_a_sample(&base_calls);
    assert_eq!(
        [
            op::COLLECT,
            op::SET_OPTION,
            op::GET_OPTION,
            op::SET_TYPE_OPTION,
            op::GET_TYPE_OPTION
        ]
        .map(|sub| count(&base_calls, sub)),
        [0; 5]
    );
    let (used, facts, calls, functions) = plan(&design(
        "            c.option.weight = 2;
            c.cv.option.at_least = 2;
            c.x.option.cross_num_print_missing = 1;
            cg::type_option.weight = 3;
            cg::cq::type_option.goal = 50;
            cg::type_option.merge_instances = 1;",
        "        if (q == 8'd9) c.stop();
        if (q == 8'd20) c.cv.start();
        if (q == 8'd30) c.start();
        r = c.cv.get_inst_coverage() + cg::x::get_coverage();
        covered = c.option.at_least + cg::type_option.weight;",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), per_sample);
    assert_eq!(used, base);
    // No subroutine of the two-argument queries reaches the design.
    assert_eq!(functions, base_functions);
    assert!(count(&calls, op::COLLECT) >= 3, "{calls:?}");
    assert!(count(&calls, op::SET_OPTION) >= 3, "{calls:?}");
    assert!(count(&calls, op::SET_TYPE_OPTION) >= 3, "{calls:?}");
    assert!(count(&calls, op::GET_OPTION) >= 1, "{calls:?}");
    assert!(count(&calls, op::GET_TYPE_OPTION) >= 1, "{calls:?}");
    // With the two arguments, the queries are coverage operations too.
    let (counted, facts, calls, _) = plan(&design(
        "",
        "        r = c.get_inst_coverage(covered, total) + cg::x::get_coverage(covered, total);",
    ));
    assert_eq!(facts.heap_calls, 0);
    assert_eq!(of_a_sample(&calls), per_sample);
    assert_eq!(counted, base);
    assert!(count(&calls, op::INST_COVERAGE) >= 3, "{calls:?}");
    assert!(count(&calls, op::TYPE_COVERAGE) >= 3, "{calls:?}");
}

/// A covergroup instance sampled at the clocking event of its type (IEEE
/// 1800-2023 19.3) costs its declaration one procedure and one handle, the
/// hidden variable that holds the instance. At edges of variables the
/// procedure is flip-flop shaped: the plan of the design is that of the
/// design that writes `always @(posedge clk) c.sample();` itself. At any
/// other event it is a process, `initial forever begin @(event);
/// c.sample(); end`, a compiled kernel, planned as that process written in
/// the source. A clocking event costs nothing where no declaration
/// constructs an instance of the type.
#[test]
fn covergroup_sampler_kernel_is_compiled() {
    let plan = |items: &str| {
        let (program, facts) = laid_out(timed(&rtl("", items)));
        (crate::scheduler::plan_for_test(&program).unwrap(), facts)
    };
    for (written, clocked) in [
        (
            "    covergroup cg; coverpoint d; endgroup
    cg c = new;
    always @(posedge clk) c.sample();",
            "    covergroup cg @(posedge clk); coverpoint d; endgroup
    cg c = new;",
        ),
        (
            "    covergroup cg; coverpoint d; endgroup
    cg c = new;
    initial forever begin @(clk); c.sample(); end",
            "    covergroup cg @(clk); coverpoint d; endgroup
    cg c = new;",
        ),
    ] {
        let (written, written_facts) = plan(written);
        let (sampled, sampled_facts) = plan(clocked);
        assert!(sampled_facts.coverage_calls > 0);
        assert_eq!(sampled_facts.heap_calls, 0);
        assert_eq!(
            sampled_facts.processes, written_facts.processes,
            "{clocked}"
        );
        assert_eq!(
            sampled_facts.functions, written_facts.functions,
            "{clocked}"
        );
        assert_eq!(
            sampled_facts.handles,
            written_facts.handles + 1,
            "{clocked}"
        );
        // The initializer, then the kernels of the fixture (and the
        // sampler of a change event, compiled).
        let (initializer, kernels) = sampled.fallback.split_first().unwrap();
        assert_eq!(initializer.as_deref(), Some("a declaration initializer"));
        assert!(
            kernels.iter().all(Option::is_none),
            "{:?}",
            sampled.fallback
        );
        assert_eq!(sampled.fallback, written.fallback, "{clocked}");
        assert_eq!(sampled.settle_reads, written.settle_reads);
        assert!(sampled.settle_reads.is_some());
        assert_eq!(sampled.lazy, written.lazy, "{clocked}");
        assert_eq!(sampled.incremental, written.incremental);
        assert_eq!(sampled.active, written.active);
        assert_eq!(sampled.clocks, written.clocks);
        assert_eq!(sampled.derived, written.derived);
    }
    // The edge sampler adds no kernel: the design has the processes of the
    // design without the covergroup and its initializer.
    let (_, none) = plan("");
    let (_, edges) = plan(
        "    covergroup cg @(posedge clk); coverpoint d; endgroup
    cg c = new;",
    );
    assert_eq!(edges.processes, none.processes + 1);
    // A sampler that waits for a named event has dynamic activation frames,
    // as the process that triggers it does, and one of a program runs in
    // the reactive region set: both run in the runtime's loop, as the same
    // process written in the source does.
    let (named, _) = plan(
        "    event e;
    initial forever #4 -> e;
    covergroup cg @e; coverpoint d; endgroup
    cg c = new;",
    );
    assert_eq!(
        named.fallback.last().unwrap().as_deref(),
        Some("the process has dynamic activation frames"),
        "{:?}",
        named.fallback
    );
    let (named_written, _) = plan(
        "    event e;
    initial forever #4 -> e;
    covergroup cg; coverpoint d; endgroup
    cg c = new;
    initial forever begin @e; c.sample(); end",
    );
    assert_eq!(named.fallback, named_written.fallback);
    let (program, _) = laid_out(timed(&rtl(
        "program prog(input logic clk, input logic [7:0] d);
    covergroup pc @(posedge clk); coverpoint d; endgroup
    pc c = new;
endprogram",
        "    prog p(.clk(clk), .d(d));",
    )));
    let program = crate::scheduler::plan_for_test(&program).unwrap();
    assert!(
        program
            .fallback
            .iter()
            .any(|reason| reason.as_deref()
                == Some("a program process runs in the reactive region set")),
        "{:?}",
        program.fallback
    );
    // A type with a clocking event that only a variable names, or that a
    // design merely declares, is laid out as one without.
    for (clocked, plain) in [
        (
            "    covergroup cg @(posedge clk); coverpoint d; endgroup
    cg c;",
            "    covergroup cg; coverpoint d; endgroup
    cg c;",
        ),
        (
            "    covergroup cg @(posedge clk); coverpoint d; endgroup",
            "",
        ),
    ] {
        let (clocked_plan, clocked_facts) = plan(clocked);
        let (plain_plan, plain_facts) = plan(plain);
        assert_eq!(clocked_facts, plain_facts, "{clocked}");
        assert_eq!(clocked_plan.fallback, plain_plan.fallback, "{clocked}");
        assert_eq!(clocked_plan.settle_reads, plain_plan.settle_reads);
        assert_eq!(clocked_plan.lazy, plain_plan.lazy);
        assert_eq!(clocked_plan.incremental, plain_plan.incremental);
    }
}
