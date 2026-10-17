//! Derived-domain plans for hosts that run WebAssembly kernels themselves.
//!
//! The browser and WASI runtime of the TypeScript package drives one
//! WebAssembly module per event. To tick and move clocks exactly as
//! `Simulator` does, it needs the same build-time plans: for each event, its
//! signal and polarity, the clocks computed combinationally from it and the
//! domains clocked from state it writes. This module describes them by event
//! name and memory offset, as JSON.

use std::collections::BTreeMap;

use celox_design::DomainKind;
use serde::Serialize;

use crate::ir::{AbsoluteAddr, LaidOutProgram};

/// A one-bit event signal in the packed memory layout.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct EdgePlanSignal {
    /// Byte offset of the value; a four-state signal's unknown mask follows
    /// it in the next byte.
    pub offset: usize,
    pub width: usize,
}

/// A derived event domain.
#[derive(Clone, Debug, Serialize)]
pub struct EdgePlanDomain {
    /// The event name its kernel is compiled under.
    pub event: String,
    pub signal: EdgePlanSignal,
    /// `"posedge"`, `"negedge"`, or `"any"` for a domain of unknown kind.
    pub edge: &'static str,
}

/// What an edge of one event runs besides the event's own domain.
#[derive(Clone, Debug, Serialize)]
pub struct EdgePlanEvent {
    pub signal: EdgePlanSignal,
    /// `"posedge"`, `"negedge"`, or `"any"`.
    pub edge: &'static str,
    /// Combinational logic reads the event's signal.
    #[serde(rename = "combRead")]
    pub comb_read: bool,
    /// Domains clocked from state the event's domain writes, transitively.
    pub domains: Vec<EdgePlanDomain>,
    /// Clocks computed combinationally from the event's signal.
    pub gated: Vec<EdgePlanDomain>,
}

/// Per event name, its edge plan.
#[derive(Clone, Debug, Serialize)]
pub struct EdgePlans {
    #[serde(rename = "fourState")]
    pub four_state: bool,
    pub events: BTreeMap<String, EdgePlanEvent>,
}

fn edge_name(kind: Option<DomainKind>) -> &'static str {
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => "posedge",
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => "negedge",
        _ => "any",
    }
}

/// The canonical event domains of `program` in the order every execution
/// engine numbers them: the semantic event order, then any kernel the event
/// topology does not list. An event need not have a kernel of its own: a
/// clock that only drives a gated clock is one.
pub fn event_addresses(program: &LaidOutProgram) -> Vec<AbsoluteAddr> {
    let mut addresses = program.design.events.ordered_events.clone();
    let mut seen: crate::HashSet<_> = addresses.iter().copied().collect();
    // Preserve legacy/directly constructed programs whose event topology does
    // not list every kernel, after the semantic IDs used by trigger code.
    let mut extra: Vec<_> = program
        .sir
        .eval_apply_ffs
        .keys()
        .chain(program.sir.eval_only_ffs.keys())
        .chain(program.sir.apply_ffs.keys())
        .map(|address| program.design.events.canonical(*address))
        .filter(|address| seen.insert(*address))
        .collect();
    extra.sort_unstable();
    addresses.extend(extra);
    addresses
}

/// The edge plans of `program`'s events, named as [`event_addresses`] and
/// `program.get_path` name them.
pub fn edge_plans(program: &LaidOutProgram) -> EdgePlans {
    let layout = program.layout();
    let events = &program.design.events;
    let kinds = program.design.event_kinds();
    let signal = |address: &AbsoluteAddr| EdgePlanSignal {
        offset: layout.offsets.get(address).copied().unwrap_or_default(),
        width: layout.widths.get(address).copied().unwrap_or(1),
    };
    let domain = |address: &AbsoluteAddr| EdgePlanDomain {
        event: program.get_path(address),
        signal: signal(address),
        edge: edge_name(kinds.get(address).copied()),
    };
    let plans = event_addresses(program)
        .iter()
        .map(|address| {
            let canonical = events.canonical(*address);
            let plan = EdgePlanEvent {
                signal: signal(&canonical),
                edge: edge_name(kinds.get(&canonical).copied()),
                comb_read: events.combinationally_read.contains(&canonical),
                domains: events
                    .derived_events
                    .get(&canonical)
                    .into_iter()
                    .flatten()
                    .filter(|target| program.sir.eval_apply_ffs.contains_key(target))
                    .map(domain)
                    .collect(),
                gated: events
                    .combinational_derived
                    .get(&canonical)
                    .into_iter()
                    .flatten()
                    .filter(|derived| program.sir.eval_apply_ffs.contains_key(&derived.clock))
                    .map(|derived| domain(&derived.clock))
                    .collect(),
            };
            (program.get_path(address), plan)
        })
        .collect();
    EdgePlans {
        four_state: layout.four_state,
        events: plans,
    }
}
