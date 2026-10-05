//! Typed compile-only export written by `lydite-celox-export` and read by the
//! lifter and the Python conformance tools. Celox's own serde types carry the
//! design and SIR; only the envelope around them is defined here.
use celox_design::{
    ElaboratedDesign, InitialStateValue, RegionedStateAddr, RuntimeEventSite, StateAddr,
    VariableMetadata,
};
use celox_frontend_core::VariableKind;
use celox_sir::{ExecutionUnit, SirProgram};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type Unit = ExecutionUnit<RegionedStateAddr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compiled {
    pub status: String,
    pub four_state: bool,
    pub allow_always_ff_function_effects: bool,
    pub allowed_diagnostics: Vec<String>,
    pub signals: Vec<Signal>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub design: Design,
    pub sir: Sir,
    /// `Debug` rendering for humans; not a machine-readable lookup.
    pub frontend_lookup: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub instances: Vec<(String, usize)>,
    pub path: Vec<String>,
    pub kind: VariableKind,
    pub signed: bool,
    pub metadata: VariableMetadata,
    pub address: StateAddr,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateObject {
    pub address: StateAddr,
    pub metadata: VariableMetadata,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Design {
    pub state_objects: Vec<StateObject>,
    pub initial_state: Vec<InitialStateValue<StateAddr>>,
    pub ordered_events: Vec<StateAddr>,
    pub cascaded_events: BTreeSet<StateAddr>,
    pub event_aliases: Vec<(StateAddr, StateAddr)>,
    pub reset_clocks: Vec<(StateAddr, StateAddr)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventUnits {
    pub event: StateAddr,
    pub units: Vec<Unit>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sir {
    pub eval_comb: Vec<Unit>,
    pub eval_apply_ffs: Vec<EventUnits>,
    pub eval_comb_apply_ffs: Vec<EventUnits>,
    pub eval_only_ffs: Vec<EventUnits>,
    pub apply_ffs: Vec<EventUnits>,
}
impl Compiled {
    /// The wire format: object keys sorted, as `serde_json::Value` orders them.
    /// Celox's SIR maps are hash maps, so serializing directly is not stable.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&serde_json::to_value(self)?)
    }
}
impl From<ElaboratedDesign<StateAddr>> for Design {
    fn from(design: ElaboratedDesign<StateAddr>) -> Self {
        let events = design.events;
        Self {
            state_objects: design
                .state_objects
                .into_iter()
                .map(|(address, metadata)| StateObject { address, metadata })
                .collect(),
            initial_state: design.initial_state,
            ordered_events: events.ordered_events,
            cascaded_events: events.cascaded_events,
            event_aliases: events.aliases.into_iter().collect(),
            reset_clocks: events.reset_clocks.into_iter().collect(),
        }
    }
}
fn groups(groups: impl IntoIterator<Item = (StateAddr, Vec<Unit>)>) -> Vec<EventUnits> {
    groups
        .into_iter()
        .map(|(event, units)| EventUnits { event, units })
        .collect()
}
impl From<SirProgram<StateAddr, RegionedStateAddr>> for Sir {
    fn from(sir: SirProgram<StateAddr, RegionedStateAddr>) -> Self {
        Self {
            eval_comb: sir.eval_comb,
            eval_apply_ffs: groups(sir.eval_apply_ffs),
            eval_comb_apply_ffs: groups(sir.eval_comb_apply_ffs),
            eval_only_ffs: groups(sir.eval_only_ffs),
            apply_ffs: groups(sir.apply_ffs),
        }
    }
}
