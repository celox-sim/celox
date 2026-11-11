use std::path::Path;

use celox_runtime::{BackendExecutor, DerivedTickScratch, Edge, EdgeOutcome, run_edge, set_level};

use super::TestEdgeUnits;
use crate::{
    Simulator,
    backend::{EventHandle, SimBackend},
};

const GATED: &str = "module Top(input logic clk, input logic en, output logic [7:0] q);
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] g = 0;
                logic [7:0] w;
                assign gclk = clk & en;
                // A value the host's writes of `en` change through logic.
                assign w = en ? 8'd3 : 8'd1;
                always_ff @(posedge clk) r <= r + w;
                always_ff @(posedge gclk) g <= g + r;
                assign q = g;
            endmodule";

const INVERTED: &str = "module Top(input logic clk, input logic en, output logic [7:0] q);
                logic nclk;
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] n = 0;
                logic [7:0] g = 0;
                assign nclk = ~clk;
                assign gclk = clk & en;
                always_ff @(posedge clk) r <= r + 8'd1;
                always_ff @(posedge nclk) n <= n + r;
                always_ff @(negedge gclk) g <= g ^ n;
                assign q = n + g;
            endmodule";

const DIVIDERS: &str = "module Top(input logic clk, output logic [7:0] q);
                logic half = 0;
                logic quarter = 0;
                logic [7:0] h = 0;
                logic [7:0] w = 0;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) quarter <= ~quarter;
                always_ff @(posedge half) h <= h + 8'd1;
                always_ff @(posedge quarter) w <= w + h;
                assign q = h + w;
            endmodule";

const RAISED_ENABLE: &str = "module Top(input logic clk, output logic [7:0] q);
                logic en = 0;
                logic gclk;
                logic [7:0] g = 0;
                assign gclk = clk & en;
                always_ff @(posedge clk) en <= ~en;
                always_ff @(posedge gclk) g <= g + 8'd1;
                assign q = g;
            endmodule";

const INDEPENDENT: &str =
    "module Top(input logic clk, input logic en, input logic [7:0] d, output logic [7:0] q);
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] g = 0;
                assign gclk = clk & en;
                always_ff @(posedge clk) r <= r + d;
                always_ff @(posedge gclk) g <= g + 8'd3;
                assign q = r ^ g;
            endmodule";

/// The design state: the stable region after its header.
fn state<B: SimBackend>(backend: &B) -> Vec<u8> {
    let (ptr, len) = backend.memory_as_ptr();
    let size = backend.layout().total_size;
    assert!(size <= len);
    // Safety: the backend exposes `len` readable bytes at `ptr`.
    let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
    memory[celox_state_layout::STATE_HEADER_SIZE..size].to_vec()
}

/// Whether combinational logic is stale after an edge with `outcome`,
/// as `run_tick` keeps it.
fn stale(dirty: bool, outcome: EdgeOutcome) -> bool {
    match outcome {
        EdgeOutcome::Unchanged | EdgeOutcome::Stored => dirty,
        EdgeOutcome::Moved => true,
        EdgeOutcome::Settled => false,
    }
}

/// Run edges of `clk` and host writes of `en`, when the design has it,
/// through the Rust plan walk and through the generated units, and
/// compare the state after each.
fn agrees(source: &str) {
    let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
    // The Cranelift backend runs extra units without an image to check.
    let mut walked = build().build_cranelift().unwrap();
    let sink = TestEdgeUnits::default();
    let mut generated = build()
        .derived_edge_units(sink.clone())
        .build_cranelift()
        .unwrap();
    let units = sink.lock().unwrap().clone();
    assert!(!units.is_empty(), "no edge unit was generated");
    let clk = walked.event("clk");
    let address = walked.backend.id_to_addr_slice()[clk.id()];
    let plans = walked.derived_tick_plans();
    let plan = plans.get(clk.id()).and_then(Option::as_ref);
    assert!(plan.is_some(), "clk has no derived plan");
    let gated = plan.is_some_and(|plan| plan.lever_signals().next().is_some());
    let info = walked.event_edge(clk);
    let en = walked.try_signal("en").ok();
    let mut scratch = DerivedTickScratch::default();
    assert!(
        state(&generated.backend) == state(&walked.backend),
        "the simulators start in other states"
    );
    let (mut walked_dirty, mut generated_dirty) = (false, false);
    let edges = [
        Edge::Rising,
        Edge::Falling,
        Edge::Falling,
        Edge::Rising,
        Edge::Falling,
    ];
    for step in 0..60usize {
        if let Some(en) = en
            && step % 7 < 2
        {
            let value = u8::from(step % 3 != 0);
            walked.backend.set(en, value);
            generated.backend.set(en, value);
            walked_dirty = true;
            generated_dirty = true;
        }
        let edge = edges[step % edges.len()];
        let outcome = run_edge(
            &mut BackendExecutor(&mut walked.backend),
            plan,
            info,
            &mut scratch,
            clk,
            edge,
            walked_dirty,
            set_level,
        )
        .unwrap();
        walked_dirty = stale(walked_dirty, outcome);

        let from = u8::from(generated.backend.get_as::<u8>(info.signal) & 1 != 0);
        let outcome = if from == edge.level() {
            EdgeOutcome::Unchanged
        } else {
            generated.backend.set(info.signal, edge.level());
            let fires = info.fires(Some(from != 0), edge);
            let settle = generated_dirty || info.comb_read;
            let unit = units
                .iter()
                .find(|unit| {
                    unit.event == address
                        && unit.rising == (edge == Edge::Rising)
                        && unit.settle == settle
                        && unit.fires == fires
                })
                .expect("a unit for every case");
            generated.backend.run_process(unit.index).unwrap();
            if gated || fires {
                EdgeOutcome::Settled
            } else if info.comb_read {
                EdgeOutcome::Moved
            } else {
                EdgeOutcome::Stored
            }
        };
        generated_dirty = stale(generated_dirty, outcome);
        assert_eq!(generated_dirty, walked_dirty, "step {step}");
        let (left, right) = (state(&generated.backend), state(&walked.backend));
        // The settle's partition flags hold only what is left to
        // recompute.
        let flags = walked
            .backend
            .layout()
            .settle
            .as_ref()
            .map(|settle| settle.flags());
        if let Some(offset) = left
            .iter()
            .zip(&right)
            .enumerate()
            .map(|(offset, pair)| (offset + celox_state_layout::STATE_HEADER_SIZE, pair))
            .find(|(offset, (a, b))| {
                a != b && !flags.as_ref().is_some_and(|flags| flags.contains(offset))
            })
            .map(|(offset, _)| offset)
        {
            let name = walked
                .named_signals()
                .into_iter()
                .find(|named| {
                    let start = named.signal.offset;
                    (start..start + named.signal.width.div_ceil(8)).contains(&offset)
                })
                .map_or_else(|| "no named signal".to_string(), |named| named.name);
            panic!(
                "step {step} ({edge:?}): the generated edge left state byte {offset} ({name}) otherwise"
            );
        }
    }
}

/// Drive `source` through the `Simulator` API, with derived domains run
/// by the generated edge units and by the plan walk, and compare every
/// named signal after each operation: ticks of `clk`, single and
/// repeated edges, and host writes of `en` when the design has it.
fn ticks_agree(source: &str) {
    let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
    ticks_agree_on(
        build().build().unwrap(),
        build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build()
            .unwrap(),
    );
}

/// [`ticks_agree`] for two simulators of one backend.
fn ticks_agree_on<B: SimBackend>(mut generated: Simulator<B>, mut walked: Simulator<B>) {
    assert!(
        !generated.program.runtime_schema.derived_edges.is_empty(),
        "no edge unit was generated"
    );
    assert!(walked.program.runtime_schema.derived_edges.is_empty());
    let clk = generated.event("clk");
    let en = generated.try_signal("en").ok();
    let names: Vec<_> = walked.named_signals();
    for step in 0..80usize {
        for sim in [&mut generated, &mut walked] {
            if let Some(en) = en
                && step % 7 < 2
            {
                sim.modify(|io| io.set(en, u8::from(step % 3 != 0)))
                    .unwrap();
            }
            match step % 5 {
                0 | 1 => sim.tick(clk).unwrap(),
                2 => sim.edge(clk, Edge::Rising).unwrap(),
                3 => sim.edge(clk, Edge::Rising).unwrap(),
                _ => sim.edge(clk, Edge::Falling).unwrap(),
            }
        }
        for named in &names {
            assert_eq!(
                generated.get(named.signal),
                walked.get(named.signal),
                "step {step}: {}",
                named.name
            );
        }
    }
}

#[test]
fn simulator_ticks_run_generated_edges_as_the_plan_walk() {
    for source in [GATED, INVERTED, DIVIDERS, RAISED_ENABLE, INDEPENDENT] {
        ticks_agree(source);
    }
}

/// One `clk` gating `gated_clocks` clocks, each clocking a counter.
fn gated_domains_source(gated_clocks: usize) -> String {
    let mut source = String::from(
        "module Top(input logic clk, input logic en, output logic [7:0] q);\n  \
             logic [6:0] count = 0;\n  logic [7:0] sum;\n  \
             always_ff @(posedge clk) count <= count + 7'd1;\n",
    );
    let mut terms = Vec::new();
    for index in 0..gated_clocks {
        source += &format!(
            "  logic g{index};\n  logic [7:0] n{index} = 0;\n  \
                 assign g{index} = clk & (en ^ (count[2:0] == 3'd{bits}) ^ (count[6:3] == 4'd{high}));\n  \
                 always_ff @(posedge g{index}) n{index} <= n{index} + 8'd1;\n",
            bits = index % 8,
            high = index / 8,
        );
        terms.push(format!("n{index}"));
    }
    source += &format!("  assign q = {};\nendmodule\n", terms.join(" ^ "));
    source
}

/// An edge reaching 60 gated domains.
#[test]
fn an_edge_reaching_60_gated_domains_runs_as_the_plan_walk() {
    ticks_agree(&gated_domains_source(60));
}

/// An edge reaching more domains than one word of a fire mask holds.
#[test]
fn an_edge_reaching_more_than_64_domains_runs_as_the_plan_walk() {
    ticks_agree(&gated_domains_source(70));
}

/// The same on Cranelift, whose wide comparisons once ran every domain.
#[test]
fn an_edge_reaching_more_than_64_domains_runs_on_cranelift_as_the_plan_walk() {
    let source = gated_domains_source(70);
    let build = || Simulator::from_sv_sources(vec![(source.as_str(), Path::new("top.sv"))], "Top");
    ticks_agree_on(
        build().build_cranelift().unwrap(),
        build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build_cranelift()
            .unwrap(),
    );
}

#[test]
fn gated_clocks_run_as_the_plan_walk() {
    agrees(GATED);
}

#[test]
fn inverted_and_falling_gated_clocks_run_as_the_plan_walk() {
    agrees(INVERTED);
}

#[test]
fn dividers_run_as_the_plan_walk() {
    agrees(DIVIDERS);
}

#[test]
fn enables_the_commit_raises_run_as_the_plan_walk() {
    agrees(RAISED_ENABLE);
}

#[test]
fn independent_gated_domains_run_as_the_plan_walk() {
    agrees(INDEPENDENT);
}
