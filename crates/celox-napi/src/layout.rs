use serde::Serialize;
use std::collections::HashMap;
use celox::{NamedEvent, NamedSignal, PortTypeKind, get_byte_size};

/// Layout information for a single signal, serialized to JS.
#[derive(Debug, Clone, Serialize)]
pub struct SignalLayout {
    pub offset: usize,
    pub width: usize,
    pub byte_size: usize,
    pub is_4state: bool,
    pub direction: &'static str,
    pub type_kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub array_dims: Vec<usize>,
}

/// Build a map of signal name -> layout info from named signals.
///
/// `four_state_mode`: whether the simulator is running in 4-state mode.
/// When false, `is_4state` is always reported as false (no mask space exists).
pub fn build_signal_layout(signals: &[NamedSignal], four_state_mode: bool) -> HashMap<String, SignalLayout> {
    let mut map = HashMap::new();
    for ns in signals {
        let direction = match ns.info.var_kind {
            veryl_analyzer::ir::VarKind::Input => "input",
            veryl_analyzer::ir::VarKind::Output => "output",
            veryl_analyzer::ir::VarKind::Inout => "inout",
            _ => "internal",
        };
        let type_kind = match ns.info.type_kind {
            PortTypeKind::Clock => "clock",
            PortTypeKind::Reset | PortTypeKind::ResetAsyncHigh => "reset_async_high",
            PortTypeKind::ResetAsyncLow => "reset_async_low",
            PortTypeKind::ResetSyncHigh => "reset_sync_high",
            PortTypeKind::ResetSyncLow => "reset_sync_low",
            PortTypeKind::Logic => "logic",
            PortTypeKind::Bit => "bit",
            PortTypeKind::Other => "other",
        };
        let (width, array_dims) = if ns.info.array_dims.is_empty() {
            (ns.signal.width, vec![])
        } else {
            let element_width = ns.signal.width
                / ns.info.array_dims.iter().product::<usize>();
            (element_width, ns.info.array_dims.clone())
        };

        map.insert(
            ns.name.clone(),
            SignalLayout {
                offset: ns.signal.offset,
                width,
                byte_size: get_byte_size(width),
                is_4state: four_state_mode && ns.signal.is_4state,
                direction,
                type_kind,
                array_dims,
            },
        );
    }
    map
}

/// Build a map of event name -> event ID from named events.
pub fn build_event_map(events: &[NamedEvent]) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for ne in events {
        map.insert(ne.name.clone(), ne.id as u32);
    }
    map
}
