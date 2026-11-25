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
    let (program, ..) = builder
        .four_state(false)
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
    let declared = packages([
        "covergroup ucg(int a); endgroup",
        "covergroup pcg; endgroup",
        "covergroup kcg(string s = \"k\"); endgroup",
    ]);
    let items = format!(
        "{import}
    covergroup cg(int lo, string name = \"n\") @(posedge clk); coverpoint d; endgroup
    covergroup plain; endgroup"
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
        "covergroup ucg(int a); endgroup
package q;
    covergroup pcg; endgroup
endpackage",
        "    import q::*;
    covergroup plain(string s); endgroup",
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
            "covergroup ucg(string s = \"u\"); endgroup",
            "covergroup pcg(string s = \"p\"); endgroup",
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
