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
}

/// Build a map of signal name -> layout info from named signals.
pub fn build_signal_layout(signals: &[NamedSignal]) -> HashMap<String, SignalLayout> {
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
            PortTypeKind::Reset => "reset",
            PortTypeKind::Logic => "logic",
            PortTypeKind::Bit => "bit",
            PortTypeKind::Other => "other",
        };
        map.insert(
            ns.name.clone(),
            SignalLayout {
                offset: ns.signal.offset,
                width: ns.signal.width,
                byte_size: get_byte_size(ns.signal.width),
                is_4state: ns.signal.is_4state,
                direction,
                type_kind,
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
