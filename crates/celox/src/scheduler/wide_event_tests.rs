use num_bigint::BigUint;

const CLOCKS: usize = 70;
const PERIODS: usize = 7;
const END: u64 = 100;

/// 70 clocks, each clocking a counter. One process per period toggles
/// its clocks, so the events exceed a word while the kernels stay few.
fn more_than_64_events() -> String {
    let mut source = String::from("`timescale 1ns/1ns\nmodule Top;\n");
    for index in 0..CLOCKS {
        source += &format!(
            "  logic c{index} = 0;\n  logic [7:0] n{index} = 0;\n  \
                 always_ff @(posedge c{index}) n{index} <= n{index} + 8'd1;\n"
        );
    }
    for period in 1..=PERIODS {
        let toggles: String = (0..CLOCKS)
            .filter(|index| index % PERIODS + 1 == period)
            .map(|index| format!(" c{index} = ~c{index};"))
            .collect();
        source += &format!("  always #{period} begin{toggles} end\n");
    }
    source + "endmodule\n"
}

fn builder(source: &str) -> crate::SimulatorBuilder<'_, crate::Simulation> {
    crate::Simulation::from_sv_sources(vec![(source, std::path::Path::new("events.sv"))], "Top")
}

/// Run to `END` and check every counter against its clock's edges;
/// returns the slots the compiled scheduler ran.
fn counts_every_edge<B: crate::SimBackend>(backend: &str, mut sim: crate::Simulation<B>) -> u64 {
    sim.run_until(END).unwrap();
    for index in 0..CLOCKS {
        // A clock with half period `p` rises at p, 3p, 5p, ...
        let period = (index % PERIODS + 1) as u64;
        let rises = (END / period).div_ceil(2);
        let counter = sim.signal(&format!("n{index}"));
        assert_eq!(
            sim.get(counter),
            BigUint::from(rises),
            "{backend}: n{index}"
        );
    }
    sim.scheduler_stats().compiled_slots
}

/// A design with more events than one word of a mask holds runs in
/// the compiled scheduler, and every domain sees each of its edges.
#[test]
fn more_than_64_events_run_in_the_compiled_scheduler() {
    let source = more_than_64_events();
    let mut sim = builder(&source).build().unwrap();
    let scheduler = sim
        .simulator
        .program
        .runtime_schema
        .scheduler
        .as_ref()
        .unwrap();
    assert!(scheduler.unit.is_some(), "{:?}", scheduler.fallback);
    assert!(scheduler.events.len() > 64);
    assert!(scheduler.fallback.iter().all(Option::is_none));
    sim.run_until(1).unwrap();
    assert!(sim.scheduler_stats().compiled_slots > 0);
    counts_every_edge("native", sim);
    // Every step of the compiled scheduler agrees with the reference
    // loop.
    let mut checked = builder(&source)
        .scheduler_mode(crate::SchedulerMode::Differential)
        .build()
        .unwrap();
    checked.run_until(END).unwrap();
    assert!(checked.scheduler_stats().checked_steps > 0);
}

/// 70 instances on one clock: the frontend's estimate counts the clock
/// once per instance and widens the scheduler's slots, while the design
/// has few events. A `$finish` ends a compiled slot early.
#[test]
fn wide_slots_with_few_events_finish_on_every_backend() {
    let mut source = String::from(
        "`timescale 1ns/1ns\nmodule Counter(input logic clk, output logic [7:0] n);\n  \
             initial n = 0;\n  always_ff @(posedge clk) n <= n + 8'd1;\nendmodule\n\
             module Top;\n  logic clk = 0;\n  always #1 clk = ~clk;\n  initial #20 $finish;\n",
    );
    for index in 0..CLOCKS {
        source +=
            &format!("  logic [7:0] n{index};\n  Counter u{index}(.clk(clk), .n(n{index}));\n");
    }
    source += "endmodule\n";
    fn finishes<B: crate::SimBackend>(backend: &str, mut sim: crate::Simulation<B>) {
        sim.run_until(100).unwrap();
        assert!(sim.is_finished(), "{backend}");
        for index in 0..CLOCKS {
            let counter = sim.signal(&format!("n{index}"));
            assert_eq!(sim.get(counter), BigUint::from(10u8), "{backend}: n{index}");
        }
    }
    let wide = builder(&source).build().unwrap();
    let scheduler = wide
        .simulator
        .program
        .runtime_schema
        .scheduler
        .as_ref()
        .unwrap();
    assert!(scheduler.events.len() <= 64);
    assert!(wide.simulator.layout().widths[&scheduler.slots.levels] > 64);
    finishes("native", wide);
    finishes("interpreter", builder(&source).build_interpreter().unwrap());
    finishes("tiered", builder(&source).build_tiered().unwrap());
    #[cfg(feature = "cranelift")]
    finishes("cranelift", builder(&source).build_cranelift().unwrap());
    #[cfg(feature = "wasm-runtime")]
    finishes("wasm", builder(&source).build_wasm().unwrap());
}

/// The runtime reads the wide scheduler slots on every backend.
#[test]
fn more_than_64_events_run_on_every_backend() {
    let source = more_than_64_events();
    counts_every_edge("interpreter", builder(&source).build_interpreter().unwrap());
    counts_every_edge("tiered", builder(&source).build_tiered().unwrap());
    // Cranelift's wide comparisons once ran every domain (#2102).
    #[cfg(feature = "cranelift")]
    assert!(counts_every_edge("cranelift", builder(&source).build_cranelift().unwrap()) > 0);
    #[cfg(feature = "wasm-runtime")]
    counts_every_edge("wasm", builder(&source).build_wasm().unwrap());
}
