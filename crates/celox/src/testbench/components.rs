//! Clocks, resets and writes of the components a testbench drives.

use super::*;

/// Run up to `count` periods of a testbench clock generator's signal, each a
/// rise followed by a fall, and return how many ran. A run that the event's
/// own domain alone reacts to fires that domain once per period with the
/// signal left low, which is all a period does then.
pub(super) fn clock_periods<B: SimBackend>(
    sim: &mut Simulator<B>,
    event: B::Event,
    count: u64,
) -> (u64, Result<(), crate::RuntimeErrorCode>) {
    if sim.period_runs_only_active_edge(event) {
        return sim.tick_deferred_comb_many(event, count);
    }
    let result = sim
        .edge(event, celox_runtime::Edge::Rising)
        .and_then(|()| sim.edge(event, celox_runtime::Edge::Falling));
    (1, result)
}

pub(super) fn tick_component_reset<B: SimBackend>(
    sim: &mut Simulator<B>,
    reset_event: Option<B::Event>,
    clock_event: B::Event,
    time: u64,
) -> Result<(), String> {
    sim.components
        .begin_reset_cycles(reset_event.map(|event| event.id()));
    let result = tick_component_clock(sim, clock_event, time.saturating_sub(1));
    sim.components.end_reset_cycles();
    result
}

pub(super) fn tick_component_clock<B: SimBackend>(
    sim: &mut Simulator<B>,
    event: B::Event,
    completed_cycles: u64,
) -> Result<(), String> {
    let mut state = sim
        .component_simulation
        .take()
        .ok_or_else(|| "component event scheduler is not initialized".to_string())?;
    if sim.dirty {
        if let Err(error) = settle_testbench(sim) {
            sim.component_simulation = Some(state);
            return Err(error.to_string());
        }
    }
    state.synchronize_event_values(&sim.backend);
    let signal = sim.backend.resolve_signal(&event.addr());
    let high_time = completed_cycles.saturating_mul(2);
    state.schedule(event, signal, high_time, 1);
    let result = state
        .step(sim)
        .and_then(|_| {
            state.schedule(event, signal, high_time.saturating_add(1), 0);
            state.step(sim)
        })
        .map(|_| ())
        .map_err(|error| error.to_string());
    sim.component_simulation = Some(state);
    result
}

pub(super) fn apply_component_writes<B: SimBackend>(
    sim: &mut Simulator<B>,
    writes: Vec<crate::component::ComponentWrite>,
) {
    if writes.is_empty() {
        return;
    }
    for write in writes {
        write.apply(&mut sim.backend);
    }
    sim.dirty = true;
}

pub(super) fn root_testbench_name<B: SimBackend>(sim: &Simulator<B>) -> String {
    sim.program
        .design
        .root_instance()
        .map(|instance| instance.module_name.clone())
        .unwrap_or_else(|| "testbench".to_string())
}

pub(super) fn resize_component_return(
    value: BigUint,
    source_width: usize,
    signed: bool,
    destination_width: usize,
) -> BigUint {
    if destination_width == 0 {
        return BigUint::default();
    }
    let source_mask = if source_width == 0 {
        BigUint::default()
    } else {
        (BigUint::from(1u8) << source_width) - BigUint::from(1u8)
    };
    let mut value = value & source_mask;
    if signed
        && source_width != 0
        && source_width < destination_width
        && value.bit((source_width - 1) as u64)
    {
        let destination_mask = (BigUint::from(1u8) << destination_width) - BigUint::from(1u8);
        let extension = destination_mask ^ ((BigUint::from(1u8) << source_width) - 1u8);
        value |= extension;
    }
    let destination_mask = (BigUint::from(1u8) << destination_width) - BigUint::from(1u8);
    value & destination_mask
}
