use super::{
    BinaryOp, EffectId, EffectKind, EventDomain, EventIr, ProcessId, RegionId, RegionKind, ValueId,
    ValueKind, ValueScope,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventIrInvariant {
    RootRegion,
    RegionParent,
    ProcessRegion,
    ValueRegion,
    ValueOperand,
    ValueScope,
    ValueType,
    CombGraph,
    EffectRegion,
    EffectOrder,
    EffectValue,
    EffectScope,
    StageDomain,
    StageProcess,
    StageType,
    CommitDomain,
    CommitSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventIrError {
    pub invariant: EventIrInvariant,
    pub entity: Option<String>,
    pub message: String,
}

impl EventIrError {
    fn new(
        invariant: EventIrInvariant,
        entity: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            entity,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EventIrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "EIR verification {:?}", self.invariant)?;
        if let Some(entity) = &self.entity {
            write!(formatter, " at {entity}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for EventIrError {}

pub(super) fn verify(ir: &EventIr) -> Result<(), EventIrError> {
    verify_regions(ir)?;
    verify_processes(ir)?;
    verify_values(ir)?;
    verify_comb_definitions(ir)?;
    verify_effects(ir)
}

fn verify_regions(ir: &EventIr) -> Result<(), EventIrError> {
    let Some(root) = ir.regions().first() else {
        return Err(EventIrError::new(
            EventIrInvariant::RootRegion,
            None,
            "graph has no event root",
        ));
    };
    if root.parent.is_some() || root.kind != RegionKind::EventRoot {
        return Err(EventIrError::new(
            EventIrInvariant::RootRegion,
            Some(RegionId(0).to_string()),
            "region 0 must be the parentless EventRoot",
        ));
    }
    for (index, region) in ir.regions().iter().enumerate().skip(1) {
        let id = RegionId(index);
        let Some(parent) = region.parent else {
            return Err(EventIrError::new(
                EventIrInvariant::RegionParent,
                Some(id.to_string()),
                "non-root region has no parent",
            ));
        };
        if parent.0 >= index {
            return Err(EventIrError::new(
                EventIrInvariant::RegionParent,
                Some(id.to_string()),
                format!("parent {parent} is not an earlier region"),
            ));
        }
    }
    Ok(())
}

fn verify_processes(ir: &EventIr) -> Result<(), EventIrError> {
    for (index, process) in ir.processes().iter().enumerate() {
        let id = ProcessId(index);
        let Some(region) = ir.regions().get(process.region.0) else {
            return Err(EventIrError::new(
                EventIrInvariant::ProcessRegion,
                Some(id.to_string()),
                format!("names absent {}", process.region),
            ));
        };
        if region.kind != RegionKind::FfProcess(id) {
            return Err(EventIrError::new(
                EventIrInvariant::ProcessRegion,
                Some(id.to_string()),
                format!("{} is not this process's root region", process.region),
            ));
        }
    }
    Ok(())
}

fn region_process(ir: &EventIr, mut region: RegionId) -> Option<ProcessId> {
    loop {
        let current = ir.regions().get(region.0)?;
        if let RegionKind::FfProcess(process) = current.kind {
            return Some(process);
        }
        region = current.parent?;
    }
}

fn verify_values(ir: &EventIr) -> Result<(), EventIrError> {
    for (index, value) in ir.values().iter().enumerate() {
        let id = ValueId(index);
        if value.ty.width == 0 {
            return Err(EventIrError::new(
                EventIrInvariant::ValueType,
                Some(id.to_string()),
                "value has zero logical width",
            ));
        }
        if ir.regions().get(value.region.0).is_none() {
            return Err(EventIrError::new(
                EventIrInvariant::ValueRegion,
                Some(id.to_string()),
                format!("names absent {}", value.region),
            ));
        }
        let expected_process = region_process(ir, value.region);
        match value.scope {
            ValueScope::Event if expected_process.is_some() => {
                return Err(EventIrError::new(
                    EventIrInvariant::ValueScope,
                    Some(id.to_string()),
                    "event-scoped value is nested in an FF process region",
                ));
            }
            ValueScope::Process(process) if expected_process == Some(process) => {}
            ValueScope::Process(process) => {
                return Err(EventIrError::new(
                    EventIrInvariant::ValueScope,
                    Some(id.to_string()),
                    format!(
                        "scope {process} does not own control region {}",
                        value.region
                    ),
                ));
            }
            ValueScope::Event => {}
        }
        let mut operand_error = None;
        value.kind.visit_operands(|operand| {
            if operand_error.is_none() {
                operand_error = verify_value_operand(ir, id, index, value.scope, operand).err();
            }
        });
        if let Some(error) = operand_error {
            return Err(error);
        }
        verify_value_type(ir, id)?;
    }
    Ok(())
}

fn verify_value_operand(
    ir: &EventIr,
    id: ValueId,
    index: usize,
    scope: ValueScope,
    operand: ValueId,
) -> Result<(), EventIrError> {
    let Some(input) = ir.values().get(operand.0) else {
        return Err(EventIrError::new(
            EventIrInvariant::ValueOperand,
            Some(id.to_string()),
            format!("names absent operand {operand}"),
        ));
    };
    if operand.0 >= index {
        return Err(EventIrError::new(
            EventIrInvariant::ValueOperand,
            Some(id.to_string()),
            format!("operand {operand} is not topologically earlier"),
        ));
    }
    match (scope, input.scope) {
        (ValueScope::Event, ValueScope::Process(process)) => Err(EventIrError::new(
            EventIrInvariant::ValueScope,
            Some(id.to_string()),
            format!("event value depends on process-local {operand} from {process}"),
        )),
        (ValueScope::Process(consumer), ValueScope::Process(producer)) if consumer != producer => {
            Err(EventIrError::new(
                EventIrInvariant::ValueScope,
                Some(id.to_string()),
                format!("{consumer} value depends on {producer} process-local operand {operand}"),
            ))
        }
        _ => Ok(()),
    }
}

fn get_value(ir: &EventIr, id: ValueId) -> &super::Value {
    &ir.values()[id.0]
}

fn verify_value_type(ir: &EventIr, id: ValueId) -> Result<(), EventIrError> {
    let current = get_value(ir, id);
    let fail =
        |message| EventIrError::new(EventIrInvariant::ValueType, Some(id.to_string()), message);
    match &current.kind {
        ValueKind::Constant { value, unknown } => {
            if value.bits() > current.ty.width as u64 || unknown.bits() > current.ty.width as u64 {
                return Err(fail(
                    "constant payload or unknown mask exceeds its type width",
                ));
            }
            if !current.ty.four_state && unknown != &num_bigint::BigUint::from(0u8) {
                return Err(fail("two-state constant has a non-zero unknown mask"));
            }
        }
        ValueKind::ReadClockSnapshot(range) => {
            if range.width() != Some(current.ty.width) {
                return Err(fail("snapshot range width does not match value type"));
            }
            if current.scope != ValueScope::Event {
                return Err(fail("snapshot read is not event-scoped"));
            }
        }
        ValueKind::ReadPersistentMemory { offset, width, .. } => {
            if *width != current.ty.width {
                return Err(fail("memory read width does not match value type"));
            }
            if get_value(ir, *offset).ty.four_state {
                return Err(fail("persistent-memory offset is four-state"));
            }
        }
        ValueKind::DynamicSelect {
            source,
            bit_offset,
            width,
        } => {
            if *width != current.ty.width {
                return Err(fail("selected width does not match value type"));
            }
            if *width > get_value(ir, *source).ty.width {
                return Err(fail("dynamic selection is wider than its source"));
            }
            if get_value(ir, *bit_offset).ty.four_state {
                return Err(fail("dynamic bit offset is four-state"));
            }
        }
        ValueKind::ReadCombDefinition { definition, access } => {
            let width = access
                .msb
                .checked_sub(access.lsb)
                .and_then(|width| width.checked_add(1));
            if width != Some(current.ty.width) {
                return Err(fail("selected range width does not match value type"));
            }
            let Some(definition) = ir.comb_definitions().get(definition.0) else {
                return Err(fail("read names an absent combinational definition"));
            };
            let Some(definition_width) = definition.target.width() else {
                return Err(fail(
                    "read names a combinational definition with an invalid range",
                ));
            };
            if access.msb >= definition_width {
                return Err(fail("read lies outside its combinational definition"));
            }
            if current.scope != ValueScope::Event {
                return Err(fail("combinational-definition read is not event-scoped"));
            }
        }
        ValueKind::Slice { source, access } => {
            let width = access
                .msb
                .checked_sub(access.lsb)
                .and_then(|width| width.checked_add(1));
            if width != Some(current.ty.width) {
                return Err(fail("selected range width does not match value type"));
            }
            if access.msb >= get_value(ir, *source).ty.width {
                return Err(fail("slice lies outside its source type"));
            }
        }
        ValueKind::Unary { op, input } => {
            if op.result_width(get_value(ir, *input).ty.width) != current.ty.width {
                return Err(fail(
                    "unary result width does not match operation semantics",
                ));
            }
            if matches!(op, super::UnaryOp::ToTwoState) {
                if current.ty.four_state {
                    return Err(fail("ToTwoState has a four-state result"));
                }
            } else if current.ty.four_state != get_value(ir, *input).ty.four_state {
                return Err(fail(
                    "unary operation changes state kind without a conversion",
                ));
            }
        }
        ValueKind::Binary { op, lhs, rhs } => {
            let comparison = matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
            );
            let logical = matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr);
            let expected = if comparison || logical {
                1
            } else {
                get_value(ir, *lhs).ty.width
            };
            if current.ty.width != expected {
                return Err(fail(
                    "binary result width does not match operation semantics",
                ));
            }
            if comparison && get_value(ir, *lhs).ty.width != get_value(ir, *rhs).ty.width {
                return Err(fail("comparison operand widths differ"));
            }
            if !matches!(
                op,
                BinaryOp::Shl
                    | BinaryOp::Shr
                    | BinaryOp::Sar
                    | BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
            ) && get_value(ir, *lhs).ty.width != get_value(ir, *rhs).ty.width
            {
                return Err(fail("binary operand widths differ"));
            }
        }
        ValueKind::Mux {
            condition,
            then_value,
            else_value,
        } => {
            if get_value(ir, *condition).ty.width != 1 {
                return Err(fail("Mux condition is not one bit"));
            }
            if get_value(ir, *then_value).ty != current.ty
                || get_value(ir, *else_value).ty != current.ty
            {
                return Err(fail("Mux arms do not match its result type"));
            }
        }
        ValueKind::Concat { parts } => {
            let width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(get_value(ir, *part).ty.width)
            });
            if width != Some(current.ty.width) {
                return Err(fail("Concat part widths do not match its result type"));
            }
        }
        ValueKind::ProcessPhi { inputs } => {
            if inputs.is_empty()
                || inputs
                    .iter()
                    .any(|input| get_value(ir, *input).ty != current.ty)
            {
                return Err(fail("ProcessPhi inputs do not match its result type"));
            }
        }
        ValueKind::LoopValue { initial, update } => {
            if get_value(ir, *initial).ty != current.ty || get_value(ir, *update).ty != current.ty {
                return Err(fail("LoopValue inputs do not match its result type"));
            }
        }
    }
    Ok(())
}

fn verify_comb_definitions(ir: &EventIr) -> Result<(), EventIrError> {
    ir.comb_graph().verify().map_err(|error| {
        EventIrError::new(
            EventIrInvariant::CombGraph,
            error.recipe.map(|recipe| format!("recipe{recipe}")),
            error.to_string(),
        )
    })
}

fn verify_effects(ir: &EventIr) -> Result<(), EventIrError> {
    let mut stages = Vec::new();
    let mut commit = None;
    for (index, effect) in ir.effects().iter().enumerate() {
        let id = EffectId(index);
        if ir.regions().get(effect.region.0).is_none() {
            return Err(EventIrError::new(
                EventIrInvariant::EffectRegion,
                Some(id.to_string()),
                format!("names absent {}", effect.region),
            ));
        }
        for predecessor in &effect.predecessors {
            if predecessor.0 >= index {
                return Err(EventIrError::new(
                    EventIrInvariant::EffectOrder,
                    Some(id.to_string()),
                    format!("predecessor {predecessor} is not topologically earlier"),
                ));
            }
        }
        let effect_process = region_process(ir, effect.region);
        let mut operand_error = None;
        effect.kind.visit_value_operands(|operand| {
            if operand_error.is_some() {
                return;
            }
            let Some(value) = ir.values().get(operand.0) else {
                operand_error = Some(EventIrError::new(
                    EventIrInvariant::EffectValue,
                    Some(id.to_string()),
                    format!("names absent value {operand}"),
                ));
                return;
            };
            if let ValueScope::Process(process) = value.scope
                && effect_process != Some(process)
            {
                operand_error = Some(EventIrError::new(
                    EventIrInvariant::EffectScope,
                    Some(id.to_string()),
                    format!(
                        "effect in {:?} consumes {operand} owned by {process}",
                        effect_process
                    ),
                ));
            }
        });
        if let Some(error) = operand_error {
            return Err(error);
        }
        match &effect.kind {
            EffectKind::StageNextFf {
                process,
                target,
                value,
                guard,
                ..
            } => {
                if !ir.domain().is_clock() {
                    return Err(EventIrError::new(
                        EventIrInvariant::StageDomain,
                        Some(id.to_string()),
                        "combinational event contains FF staging",
                    ));
                }
                let Some(process_region) = ir.process_region(*process) else {
                    return Err(EventIrError::new(
                        EventIrInvariant::StageProcess,
                        Some(id.to_string()),
                        format!("names absent {process}"),
                    ));
                };
                if region_process(ir, effect.region) != Some(*process)
                    || process_region.0 > effect.region.0
                {
                    return Err(EventIrError::new(
                        EventIrInvariant::StageProcess,
                        Some(id.to_string()),
                        "stage is outside its process control region",
                    ));
                }
                if matches!(
                    get_value(ir, *value).scope,
                    ValueScope::Process(owner) if owner != *process
                ) || target.width() != Some(get_value(ir, *value).ty.width)
                {
                    return Err(EventIrError::new(
                        EventIrInvariant::StageType,
                        Some(id.to_string()),
                        "stage value scope or target width is invalid",
                    ));
                }
                if let Some(guard) = guard
                    && (get_value(ir, *guard).ty.width != 1
                        || matches!(
                            get_value(ir, *guard).scope,
                            ValueScope::Process(owner) if owner != *process
                        ))
                {
                    return Err(EventIrError::new(
                        EventIrInvariant::StageType,
                        Some(id.to_string()),
                        "stage guard is not a one-bit event/same-process value",
                    ));
                }
                stages.push(id);
            }
            EffectKind::CommitFfState { stages: committed } => {
                if effect.region != ir.root_region() {
                    return Err(EventIrError::new(
                        EventIrInvariant::CommitDomain,
                        Some(id.to_string()),
                        "CommitFfState is not in the event root region",
                    ));
                }
                if commit.replace((id, committed.as_slice())).is_some() {
                    return Err(EventIrError::new(
                        EventIrInvariant::CommitDomain,
                        Some(id.to_string()),
                        "event has more than one CommitFfState",
                    ));
                }
            }
            _ => {}
        }
    }

    match (&ir.domain, commit) {
        (EventDomain::Combinational, None) if stages.is_empty() => Ok(()),
        (EventDomain::Combinational, Some((id, _))) => Err(EventIrError::new(
            EventIrInvariant::CommitDomain,
            Some(id.to_string()),
            "combinational event contains CommitFfState",
        )),
        (EventDomain::Combinational, None) => Err(EventIrError::new(
            EventIrInvariant::StageDomain,
            None,
            "combinational event contains staged FF writes",
        )),
        (EventDomain::Clock { .. }, None) => Err(EventIrError::new(
            EventIrInvariant::CommitDomain,
            None,
            "clock event has no CommitFfState",
        )),
        (EventDomain::Clock { .. }, Some((commit, committed))) => {
            verify_commit_set(ir, commit, committed, &stages)
        }
    }
}

fn verify_commit_set(
    ir: &EventIr,
    commit: EffectId,
    committed: &[EffectId],
    stages: &[EffectId],
) -> Result<(), EventIrError> {
    let mut named = vec![false; ir.effects().len()];
    for stage in committed {
        if stage.0 >= commit.0
            || !matches!(
                ir.effects().get(stage.0).map(|effect| &effect.kind),
                Some(EffectKind::StageNextFf { .. })
            )
        {
            return Err(EventIrError::new(
                EventIrInvariant::CommitSet,
                Some(commit.to_string()),
                format!("{stage} is not a preceding StageNextFf"),
            ));
        }
        if std::mem::replace(&mut named[stage.0], true) {
            return Err(EventIrError::new(
                EventIrInvariant::CommitSet,
                Some(commit.to_string()),
                format!("{stage} appears more than once in the commit set"),
            ));
        }
    }
    if stages.iter().any(|stage| !named[stage.0]) || committed.len() != stages.len() {
        return Err(EventIrError::new(
            EventIrInvariant::CommitSet,
            Some(commit.to_string()),
            "commit does not name exactly every staged FF write",
        ));
    }

    let mut ordered_before_commit = vec![false; commit.0];
    let mut worklist = ir.effects()[commit.0].predecessors.clone();
    while let Some(effect) = worklist.pop() {
        if ordered_before_commit[effect.0] {
            continue;
        }
        ordered_before_commit[effect.0] = true;
        worklist.extend(ir.effects()[effect.0].predecessors.iter().copied());
    }
    if let Some(stage) = stages.iter().find(|stage| !ordered_before_commit[stage.0]) {
        return Err(EventIrError::new(
            EventIrInvariant::CommitSet,
            Some(commit.to_string()),
            format!("{stage} has no effect-order path to the commit barrier"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;
    use std::sync::Arc;
    use veryl_analyzer::ir::VarId;

    use super::*;
    use crate::event_ir::{
        CombGraph, Effect, EventDomain, ObjectRange, Value, ValueScope, ValueType,
    };
    use crate::ir::{AbsoluteAddr, InstanceId};

    fn object(var: usize, lsb: usize, msb: usize) -> ObjectRange {
        ObjectRange::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(var as u32),
            },
            crate::event_ir::BitAccess::new(lsb, msb),
        )
    }

    #[test]
    fn verifies_snapshot_process_stage_and_commit_phase_separation() {
        let clock = object(0, 0, 0).object;
        let mut ir = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let process = ir.add_process(0);
        let region = ir.process_region(process).unwrap();
        let snapshot = ir.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: ir.root_region(),
            kind: ValueKind::ReadClockSnapshot(object(1, 0, 7)),
        });
        let one = ir.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: ir.root_region(),
            kind: ValueKind::Constant {
                value: BigUint::from(1u8),
                unknown: BigUint::from(0u8),
            },
        });
        let next = ir.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Binary {
                op: crate::event_ir::BinaryOp::Add,
                lhs: snapshot,
                rhs: one,
            },
        });
        let stage = ir.add_effect(Effect {
            region,
            predecessors: Vec::new(),
            kind: EffectKind::StageNextFf {
                process,
                target: object(1, 0, 7),
                value: next,
                guard: None,
                priority: 0,
            },
        });
        ir.add_effect(Effect {
            region: ir.root_region(),
            predecessors: vec![stage],
            kind: EffectKind::CommitFfState {
                stages: vec![stage],
            },
        });

        assert_eq!(ir.verify(), Ok(()));
    }

    #[test]
    fn rejects_process_local_values_crossing_between_ff_processes() {
        let clock = object(0, 0, 0).object;
        let mut ir = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let producer = ir.add_process(0);
        let consumer = ir.add_process(1);
        let produced = ir.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Process(producer),
            region: ir.process_region(producer).unwrap(),
            kind: ValueKind::Constant {
                value: BigUint::from(0u8),
                unknown: BigUint::from(0u8),
            },
        });
        ir.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Process(consumer),
            region: ir.process_region(consumer).unwrap(),
            kind: ValueKind::Unary {
                op: crate::event_ir::UnaryOp::Ident,
                input: produced,
            },
        });

        assert!(matches!(
            ir.verify(),
            Err(EventIrError {
                invariant: EventIrInvariant::ValueScope,
                ..
            })
        ));
    }

    #[test]
    fn rejects_commit_which_omits_a_staged_write() {
        let clock = object(0, 0, 0).object;
        let mut ir = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let process = ir.add_process(0);
        let region = ir.process_region(process).unwrap();
        let next = ir.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Constant {
                value: BigUint::from(0u8),
                unknown: BigUint::from(0u8),
            },
        });
        ir.add_effect(Effect {
            region,
            predecessors: Vec::new(),
            kind: EffectKind::StageNextFf {
                process,
                target: object(1, 0, 0),
                value: next,
                guard: None,
                priority: 0,
            },
        });
        ir.add_effect(Effect {
            region: ir.root_region(),
            predecessors: Vec::new(),
            kind: EffectKind::CommitFfState { stages: Vec::new() },
        });

        assert!(matches!(
            ir.verify(),
            Err(EventIrError {
                invariant: EventIrInvariant::CommitSet,
                ..
            })
        ));
    }
}
