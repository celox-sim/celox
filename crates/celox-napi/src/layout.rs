use serde::Serialize;
use std::collections::HashMap;
use celox::{InstanceHierarchy, NamedEvent, NamedSignal, PortTypeKind, get_byte_size};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub associated_clock: Option<String>,
}

/// Hierarchical node with signals and children, serialized to JS.
#[derive(Debug, Clone, Serialize)]
pub struct HierarchyNode {
    pub module_name: String,
    pub signals: HashMap<String, SignalLayout>,
    pub children: HashMap<String, Vec<HierarchyNode>>,
}

fn direction_str(var_kind: veryl_analyzer::ir::VarKind) -> &'static str {
    match var_kind {
        veryl_analyzer::ir::VarKind::Input => "input",
        veryl_analyzer::ir::VarKind::Output => "output",
        veryl_analyzer::ir::VarKind::Inout => "inout",
        _ => "internal",
    }
}

fn type_kind_str(type_kind: PortTypeKind) -> &'static str {
    match type_kind {
        PortTypeKind::Clock => "clock",
        PortTypeKind::ResetAsyncHigh => "reset_async_high",
        PortTypeKind::ResetAsyncLow => "reset_async_low",
        PortTypeKind::ResetSyncHigh => "reset_sync_high",
        PortTypeKind::ResetSyncLow => "reset_sync_low",
        PortTypeKind::Logic => "logic",
        PortTypeKind::Bit => "bit",
        PortTypeKind::Other => "other",
    }
}

fn build_signal_layout_entry(ns: &NamedSignal, four_state_mode: bool) -> SignalLayout {
    let direction = direction_str(ns.info.var_kind);
    let type_kind = type_kind_str(ns.info.type_kind);
    let (width, array_dims) = if ns.info.array_dims.is_empty() {
        (ns.signal.width, vec![])
    } else {
        let element_width = ns.signal.width
            / ns.info.array_dims.iter().product::<usize>();
        (element_width, ns.info.array_dims.clone())
    };

    SignalLayout {
        offset: ns.signal.offset,
        width,
        byte_size: get_byte_size(width),
        is_4state: four_state_mode && ns.signal.is_4state,
        direction,
        type_kind,
        array_dims,
        associated_clock: ns.associated_clock.clone(),
    }
}

/// Build a map of signal name -> layout info from named signals.
///
/// `four_state_mode`: whether the simulator is running in 4-state mode.
/// When false, `is_4state` is always reported as false (no mask space exists).
pub fn build_signal_layout(signals: &[NamedSignal], four_state_mode: bool) -> HashMap<String, SignalLayout> {
    let mut map = HashMap::new();
    for ns in signals {
        map.insert(ns.name.clone(), build_signal_layout_entry(ns, four_state_mode));
    }
    map
}

/// Build a hierarchy node from an InstanceHierarchy.
pub fn build_hierarchy_node(h: &InstanceHierarchy, four_state: bool) -> HierarchyNode {
    let mut signals = HashMap::new();
    for ns in &h.signals {
        signals.insert(ns.name.clone(), build_signal_layout_entry(ns, four_state));
    }

    let mut children = HashMap::new();
    for (name, instances) in &h.children {
        let nodes: Vec<HierarchyNode> = instances
            .iter()
            .map(|inst| build_hierarchy_node(inst, four_state))
            .collect();
        children.insert(name.clone(), nodes);
    }

    HierarchyNode {
        module_name: h.module_name.clone(),
        signals,
        children,
    }
}

/// Build a map of event name -> event ID from named events.
pub fn build_event_map(events: &[NamedEvent]) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for ne in events {
        map.insert(ne.name.clone(), ne.id as u32);
    }
    map
}
