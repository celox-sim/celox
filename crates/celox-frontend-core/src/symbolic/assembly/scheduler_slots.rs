//! The hidden objects a compiled scheduler exchanges with the runtime.

use super::*;

/// Declare the hidden objects a compiled scheduler exchanges with the
/// runtime (`RuntimeSchema::scheduler`) as variables of the top module, for
/// a design with `process_count` process kernels and about
/// `expected_events` events, whose edge and level masks hold one bit per
/// event in whole 64-bit words. A design without kernels has them too: host
/// clocks may run in a compiled scheduler.
pub(super) fn declare_scheduler_slots(
    modules: &mut HashMap<ModuleId, SimModule>,
    root: ModuleId,
    process_count: usize,
    expected_events: usize,
) -> Option<celox_design::SchedulerSlots<SourceVarId>> {
    let variables = &mut modules.get_mut(&root)?.variables;
    let mut next = variables.keys().map(|id| id.0 + 1).max().unwrap_or(0);
    let mut declare = |name: String, width: usize| {
        let id = SourceVarId(next);
        next += 1;
        variables.insert(
            id,
            crate::symbolic::artifact::SymbolicVariable {
                path: vec!["$scheduler".to_string(), name],
                kind: crate::VariableKind::Variable,
                signed: false,
                metadata: VariableMetadata {
                    width,
                    is_4state: false,
                    kind: DomainKind::Other,
                    type_kind: celox_design::PortTypeKind::Bit,
                    array_dims: Vec::new(),
                    value_kind: Default::default(),
                },
                packed_dims: vec![width],
                unpacked_ranges: Vec::new(),
                source: None,
                module_affiliated: false,
            },
        );
        id
    };
    use celox_design::{
        SCHEDULER_CLOCKS as CLOCKS, SCHEDULER_FLAG_WIDTH as FLAG, SCHEDULER_TIME_WIDTH as TIME,
    };
    let events = expected_events.next_multiple_of(TIME).max(TIME);
    let clocks = events.max(CLOCKS);
    Some(celox_design::SchedulerSlots {
        limit: declare("limit".into(), TIME),
        now: declare("now".into(), TIME),
        next: declare("next".into(), TIME),
        exit: declare("exit".into(), FLAG),
        exit_arg: declare("exit_arg".into(), events * 2),
        finish: declare("finish".into(), FLAG),
        cont: declare("cont".into(), TIME),
        dirty: declare("dirty".into(), FLAG),
        defer_limit_settle: declare(
            "defer_limit_settle".into(),
            celox_design::SCHEDULER_CONTROL_WIDTH,
        ),
        count: declare("count".into(), TIME),
        budget: declare("budget".into(), TIME),
        events: declare("events".into(), TIME),
        current: declare("current".into(), TIME),
        levels: declare("levels".into(), events),
        unknown_levels: declare("unknown_levels".into(), events),
        ready: declare(
            "ready".into(),
            process_count.next_multiple_of(TIME).max(TIME),
        ),
        timed: declare(
            "timed".into(),
            process_count.next_multiple_of(TIME).max(TIME),
        ),
        wake: (0..process_count)
            .map(|index| declare(format!("wake[{index}]"), TIME))
            .collect(),
        waiting: (0..process_count)
            .map(|index| declare(format!("waiting[{index}]"), FLAG))
            .collect(),
        baseline: (0..process_count)
            .map(|index| declare(format!("baseline[{index}]"), FLAG))
            .collect(),
        clock_next: declare("clock_next".into(), TIME * clocks),
        clock_period: declare("clock_period".into(), TIME * clocks),
        clock_value: declare("clock_value".into(), clocks),
        clock_armed: declare("clock_armed".into(), clocks),
    })
}
