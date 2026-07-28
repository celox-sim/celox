use std::collections::{BTreeMap, VecDeque};

use celox_analysis::cfg::ControlFlowGraph;
use num_bigint::BigUint;
use num_traits::One;
use thiserror::Error;
use veryl_analyzer::ir::{Module, VarId};

use crate::{
    HashMap, HashSet,
    event_ir::{
        BitAccess, ControlBlockId, ControlTerminator, Effect, EffectId, EffectKind, EventIr,
        FfStageKind, ObjectAccess, ObjectRange, ProcessId, Value, ValueId, ValueKind, ValueOffset,
        ValueScope, ValueType,
    },
    ir::{AbsoluteAddr, BlockId, InstanceId, RegisterId, RegisterType, SIROffset, UnaryOp},
};

use super::builder::{FfBuildOp, FfBuilder, FfReadSource, FfTerminator, FfWriteTarget};

#[derive(Debug, Error)]
pub enum FfEirBuildError {
    #[error("FF EIR builder block {0:?} is absent")]
    MissingBlock(BlockId),
    #[error("FF EIR builder block {0:?} is not terminated")]
    UnterminatedBlock(BlockId),
    #[error("FF EIR builder register {0} has no value")]
    MissingValue(RegisterId),
    #[error("FF EIR builder register {0} is absent")]
    MissingRegister(RegisterId),
    #[error("FF EIR builder local {0:?} has no reaching value in block {1:?}")]
    MissingLocalValue(LocalSlot, BlockId),
    #[error("FF EIR builder object {0} is absent from the AIR module")]
    MissingObject(VarId),
    #[error("FF EIR builder cannot resolve width of {0}: {1}")]
    ObjectWidth(VarId, String),
    #[error("FF EIR builder range overflows for {0}")]
    RangeOverflow(VarId),
    #[error("FF EIR builder combinational binding failed: {0}")]
    Comb(#[from] crate::event_ir::CombImportError),
    #[error("FF EIR builder runtime event site {0} has no relocation")]
    MissingRuntimeEvent(u32),
    #[error("FF EIR builder runtime error code {0} has no relocation")]
    MissingRuntimeError(i64),
}

#[derive(Debug, Clone)]
enum RecordedTerminator {
    Air(FfTerminator),
    Return,
}

#[derive(Debug, Clone)]
struct RecordedBlock {
    parameters: Vec<RegisterId>,
    operations: Vec<FfBuildOp>,
    terminator: Option<RecordedTerminator>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LocalSlot {
    Object(VarId),
    FinalRange(usize),
}

#[derive(Debug, Clone, Copy)]
struct FinalSinkPlan {
    object: VarId,
    access: BitAccess,
    sink: BlockId,
}

impl FinalSinkPlan {
    fn width(self) -> usize {
        self.access.msb - self.access.lsb + 1
    }

    fn contains(self, object: VarId, offset: usize, width: usize) -> bool {
        let Some(msb) = offset.checked_add(width.saturating_sub(1)) else {
            return false;
        };
        self.object == object && self.access.lsb <= offset && msb <= self.access.msb
    }
}
#[derive(Default)]
struct FfEffectOrder {
    stage_frontier: HashMap<AbsoluteAddr, BTreeMap<usize, (usize, EffectId)>>,
    last_observation: Option<EffectId>,
}

impl FfEffectOrder {
    fn stage_predecessors_and_update(
        &mut self,
        target: &ObjectAccess,
        effect: EffectId,
    ) -> Vec<EffectId> {
        let frontier = self.stage_frontier.entry(target.object).or_default();
        let mut overlapping = Vec::new();
        if let Some((&start, &(end, previous))) = frontier.range(..=target.alias.lsb).next_back()
            && end >= target.alias.lsb
        {
            overlapping.push((start, end, previous));
        }
        if target.alias.lsb < target.alias.msb {
            overlapping.extend(
                frontier
                    .range(target.alias.lsb + 1..=target.alias.msb)
                    .map(|(&start, &(end, previous))| (start, end, previous)),
            );
        }

        let mut predecessors = overlapping
            .iter()
            .map(|(_, _, previous)| *previous)
            .collect::<Vec<_>>();
        predecessors.sort_unstable();
        predecessors.dedup();

        for (start, end, previous) in overlapping {
            frontier.remove(&start);
            if start < target.alias.lsb {
                frontier.insert(start, (target.alias.lsb - 1, previous));
            }
            if end > target.alias.msb {
                frontier.insert(target.alias.msb + 1, (end, previous));
            }
        }
        frontier.insert(target.alias.lsb, (target.alias.msb, effect));
        predecessors
    }
}

impl RecordedBlock {
    fn new(parameters: Vec<RegisterId>) -> Self {
        Self {
            parameters,
            operations: Vec::new(),
            terminator: None,
        }
    }
}

/// Records one AIR process through the semantic FF builder interface and then
/// constructs process-local EIR SSA from the complete control graph.
#[derive(Debug, Clone)]
pub(crate) struct FfEirBuilder {
    registers: Vec<RegisterType>,
    blocks: Vec<RecordedBlock>,
    current_block: Option<BlockId>,
}

impl FfEirBuilder {
    pub fn new() -> Self {
        Self {
            registers: Vec::new(),
            blocks: vec![RecordedBlock::new(Vec::new())],
            current_block: Some(BlockId(0)),
        }
    }

    pub(crate) fn format_air(&self) -> String {
        use std::fmt::Write as _;

        let mut output = String::new();
        writeln!(&mut output, "registers:").unwrap();
        for (index, register) in self.registers.iter().enumerate() {
            writeln!(&mut output, "  r{index}: {register:?}").unwrap();
        }
        for (index, block) in self.blocks.iter().enumerate() {
            writeln!(&mut output, "b{index}({:?}):", block.parameters).unwrap();
            for (operation, item) in block.operations.iter().enumerate() {
                writeln!(&mut output, "  i{operation}: {item:?}").unwrap();
            }
            writeln!(&mut output, "  {:?}", block.terminator).unwrap();
        }
        output
    }

    fn block(&self, id: BlockId) -> Result<&RecordedBlock, FfEirBuildError> {
        self.blocks
            .get(id.0)
            .ok_or(FfEirBuildError::MissingBlock(id))
    }

    fn block_mut(&mut self, id: BlockId) -> Result<&mut RecordedBlock, FfEirBuildError> {
        self.blocks
            .get_mut(id.0)
            .ok_or(FfEirBuildError::MissingBlock(id))
    }

    fn finish_recording(&mut self) -> Result<(), FfEirBuildError> {
        if let Some(block) = self.current_block.take() {
            self.block_mut(block)?.terminator = Some(RecordedTerminator::Return);
        }
        for (index, block) in self.blocks.iter().enumerate() {
            if block.terminator.is_none() {
                return Err(FfEirBuildError::UnterminatedBlock(BlockId(index)));
            }
        }
        Ok(())
    }

    pub fn lower_into(
        mut self,
        ir: &mut EventIr,
        module: &Module,
        instance_id: InstanceId,
        source_order: usize,
        resets: Vec<crate::ir::AbsoluteAddr>,
    ) -> Result<Vec<EffectId>, FfEirBuildError> {
        self.finish_recording()?;
        self.mark_write_only_publications();
        let final_sinks = self.final_sink_plan(module)?;
        let process = ir.add_process_with_resets(source_order, resets);
        let process_blocks = self.create_control_blocks(ir, process);
        let local_dataflow = self.analyze_local_liveness(module, &final_sinks)?;
        let mut register_values = vec![None; self.registers.len()];
        let mut local_parameters = vec![HashMap::default(); self.blocks.len()];

        self.create_parameters(
            ir,
            process,
            &process_blocks,
            &local_dataflow,
            &mut register_values,
            &mut local_parameters,
            module,
        )?;

        let block_order = crate::cfg_order::dominance_order(
            BlockId(0),
            (0..self.blocks.len()).map(BlockId),
            |block| self.successors(block),
        );
        let mut outgoing_locals = vec![HashMap::default(); self.blocks.len()];
        let mut stages = Vec::new();
        let mut block_effect_order = (0..self.blocks.len())
            .map(|_| FfEffectOrder::default())
            .collect::<Vec<_>>();

        for block in block_order.iter().copied() {
            let eir_block = process_blocks[block.0];
            let region = ir.blocks()[eir_block.0].region;
            let mut locals = if block == BlockId(0) {
                self.create_entry_locals(
                    ir,
                    module,
                    instance_id,
                    &local_dataflow.live_in[block.0],
                    &final_sinks,
                )?
            } else {
                local_parameters[block.0].clone()
            };

            for operation in &self.block(block)?.operations {
                self.lower_operation(
                    operation,
                    ir,
                    module,
                    instance_id,
                    process,
                    region,
                    block,
                    &mut register_values,
                    &mut locals,
                    &mut block_effect_order[block.0],
                    &mut stages,
                    &final_sinks,
                )?;
            }

            let mut sink_ranges = final_sinks
                .iter()
                .enumerate()
                .filter_map(|(index, plan)| (plan.sink == block).then_some((index, *plan)))
                .collect::<Vec<_>>();
            sink_ranges.sort_unstable_by_key(|(_, plan)| (plan.object, plan.access));
            for (index, plan) in sink_ranges {
                let stage = ir.add_effect(Effect {
                    region,
                    predecessors: Vec::new(),
                    kind: EffectKind::StageNextFf {
                        process,
                        target: ObjectAccess {
                            object: AbsoluteAddr {
                                instance_id,
                                var_id: plan.object,
                            },
                            offset: ValueOffset::Static(plan.access.lsb),
                            width: plan.width(),
                            alias: plan.access,
                        },
                        value: locals[&LocalSlot::FinalRange(index)],
                        guard: None,
                        priority: stages.len(),
                        stage_kind: FfStageKind::FinalProcessSink,
                    },
                });
                stages.push(stage);
            }
            outgoing_locals[block.0] = locals;
        }

        for block in block_order {
            let eir_block = process_blocks[block.0];
            let region = ir.blocks()[eir_block.0].region;
            let terminator = self.lower_terminator(
                block,
                ir,
                process,
                region,
                &process_blocks,
                &local_dataflow.parameter_order,
                &outgoing_locals[block.0],
                &register_values,
            )?;
            ir.set_terminator(eir_block, terminator);
        }

        Ok(stages)
    }

    pub fn remap_runtime_ids(
        &mut self,
        event_sites: &HashMap<u32, u32>,
        error_codes: &HashMap<i64, i64>,
    ) -> Result<(), FfEirBuildError> {
        for block in &mut self.blocks {
            for operation in &mut block.operations {
                if let FfBuildOp::RuntimeEvent { site_id, .. } = operation {
                    *site_id = *event_sites
                        .get(site_id)
                        .ok_or(FfEirBuildError::MissingRuntimeEvent(*site_id))?;
                }
            }
            if let Some(RecordedTerminator::Air(FfTerminator::Error(code))) = &mut block.terminator
            {
                *code = *error_codes
                    .get(code)
                    .ok_or(FfEirBuildError::MissingRuntimeError(*code))?;
            }
        }
        Ok(())
    }

    fn create_control_blocks(&self, ir: &mut EventIr, process: ProcessId) -> Vec<ControlBlockId> {
        let mut blocks = Vec::with_capacity(self.blocks.len());
        blocks.push(ir.processes()[process.0].entry);
        for _ in 1..self.blocks.len() {
            blocks.push(ir.add_control_block(process));
        }
        blocks
    }

    fn create_parameters(
        &self,
        ir: &mut EventIr,
        _process: ProcessId,
        process_blocks: &[ControlBlockId],
        local_dataflow: &LocalDataflow,
        register_values: &mut [Option<ValueId>],
        local_parameters: &mut [HashMap<LocalSlot, ValueId>],
        module: &Module,
    ) -> Result<(), FfEirBuildError> {
        for (block_index, block) in self.blocks.iter().enumerate() {
            let eir_block = process_blocks[block_index];
            for register in &block.parameters {
                let ty = self.register_type(*register)?;
                let value = ir.add_block_parameter(eir_block, value_type(ty));
                register_values[register.0] = Some(value);
            }
            if block_index == 0 {
                continue;
            }
            for slot in &local_dataflow.parameter_order[block_index] {
                let ty = local_slot_type(module, &local_dataflow.final_sinks, *slot)?;
                let value = ir.add_block_parameter(eir_block, ty);
                local_parameters[block_index].insert(*slot, value);
            }
        }
        Ok(())
    }

    fn create_entry_locals(
        &self,
        ir: &mut EventIr,
        module: &Module,
        instance_id: InstanceId,
        live_in: &HashSet<LocalSlot>,
        final_sinks: &[FinalSinkPlan],
    ) -> Result<HashMap<LocalSlot, ValueId>, FfEirBuildError> {
        let mut slots = live_in.iter().copied().collect::<Vec<_>>();
        slots.sort_unstable();
        let mut values = HashMap::default();
        for slot in slots {
            let (object, offset, ty) = match slot {
                LocalSlot::Object(object) => {
                    let ty = object_type(module, object)?;
                    (object, 0, ty)
                }
                LocalSlot::FinalRange(index) => {
                    let plan = final_sinks[index];
                    let object_ty = object_type(module, plan.object)?;
                    (
                        plan.object,
                        plan.access.lsb,
                        part_type(object_ty, plan.width()),
                    )
                }
            };
            let range = object_range(instance_id, object, offset, ty.width)?;
            let value = ir.add_value(Value {
                ty,
                scope: ValueScope::Event,
                region: ir.root_region(),
                kind: ValueKind::ReadClockSnapshot(range),
            });
            values.insert(slot, value);
        }
        Ok(values)
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_operation(
        &self,
        operation: &FfBuildOp,
        ir: &mut EventIr,
        module: &Module,
        instance_id: InstanceId,
        process: ProcessId,
        region: crate::event_ir::RegionId,
        block: BlockId,
        register_values: &mut [Option<ValueId>],
        locals: &mut HashMap<LocalSlot, ValueId>,
        effect_order: &mut FfEffectOrder,
        stages: &mut Vec<EffectId>,
        final_sinks: &[FinalSinkPlan],
    ) -> Result<(), FfEirBuildError> {
        match operation {
            FfBuildOp::Imm(destination, immediate) => {
                let ty = value_type(self.register_type(*destination)?);
                let mask = width_mask(ty.width);
                let value = ir.add_value(Value {
                    ty,
                    scope: ValueScope::Process(process),
                    region,
                    kind: ValueKind::Constant {
                        value: &immediate.payload & &mask,
                        unknown: &immediate.mask & mask,
                    },
                });
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Binary(destination, lhs, operation, rhs) => {
                let value = ir.add_value(Value {
                    ty: value_type(self.register_type(*destination)?),
                    scope: ValueScope::Process(process),
                    region,
                    kind: ValueKind::Binary {
                        op: *operation,
                        lhs: register_value(register_values, *lhs)?,
                        rhs: register_value(register_values, *rhs)?,
                    },
                });
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Unary(destination, operation, input) => {
                let mut input = register_value(register_values, *input)?;
                let ty = value_type(self.register_type(*destination)?);
                let mut input_ty = ir.values()[input.0].ty;
                if !matches!(operation, UnaryOp::Ident | UnaryOp::ToTwoState)
                    && ty.four_state
                    && !input_ty.four_state
                {
                    input = resize_if_needed(
                        ir,
                        ValueScope::Process(process),
                        region,
                        input,
                        ValueType {
                            four_state: true,
                            ..input_ty
                        },
                    );
                    input_ty = ir.values()[input.0].ty;
                }
                let value = if *operation == UnaryOp::Ident && ty != input_ty {
                    resize_if_needed(ir, ValueScope::Process(process), region, input, ty)
                } else {
                    ir.add_value(Value {
                        ty,
                        scope: ValueScope::Process(process),
                        region,
                        kind: ValueKind::Unary {
                            op: *operation,
                            input,
                        },
                    })
                };
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Read {
                destination,
                object,
                source,
                offset,
                width,
            } => {
                let ty = value_type(self.register_type(*destination)?);
                let value = match source {
                    FfReadSource::ClockSnapshot => self.lower_snapshot_read(
                        ir,
                        module,
                        instance_id,
                        process,
                        region,
                        *object,
                        offset,
                        *width,
                        ty,
                        register_values,
                    )?,
                    FfReadSource::ProcessLocal => {
                        let base = *locals.get(&LocalSlot::Object(*object)).ok_or(
                            FfEirBuildError::MissingLocalValue(LocalSlot::Object(*object), block),
                        )?;
                        self.lower_select(
                            ir,
                            process,
                            region,
                            base,
                            offset,
                            *width,
                            ty,
                            register_values,
                        )?
                    }
                };
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Write {
                object,
                target,
                offset,
                width,
                value,
            } => {
                let value = register_value(register_values, *value)?;
                let final_sink = match offset {
                    SIROffset::Static(offset) if *target == FfWriteTarget::StagedState => {
                        final_sinks
                            .iter()
                            .enumerate()
                            .find(|(_, plan)| plan.contains(*object, *offset, *width))
                    }
                    _ => None,
                };
                if *target == FfWriteTarget::ProcessLocal || final_sink.is_some() {
                    let (slot, value_offset, local_ty) = if let Some((index, plan)) = final_sink {
                        (
                            LocalSlot::FinalRange(index),
                            match offset {
                                SIROffset::Static(offset) => offset - plan.access.lsb,
                                _ => unreachable!("final range accepts only static writes"),
                            },
                            part_type(object_type(module, *object)?, plan.width()),
                        )
                    } else {
                        (
                            LocalSlot::Object(*object),
                            match offset {
                                SIROffset::Static(offset) => *offset,
                                _ => 0,
                            },
                            object_type(module, *object)?,
                        )
                    };
                    let complete = matches!(offset, SIROffset::Static(_))
                        && value_offset == 0
                        && *width == local_ty.width;
                    let updated = if complete {
                        resize_if_needed(ir, ValueScope::Process(process), region, value, local_ty)
                    } else {
                        let base = *locals
                            .get(&slot)
                            .ok_or(FfEirBuildError::MissingLocalValue(slot, block))?;
                        let local_offset = match offset {
                            SIROffset::Static(_) if final_sink.is_some() => {
                                ValueOffset::Static(value_offset)
                            }
                            _ => lower_value_offset(offset, register_values)?,
                        };
                        ir.add_value(Value {
                            ty: local_ty,
                            scope: ValueScope::Process(process),
                            region,
                            kind: ValueKind::UpdateRange {
                                base,
                                offset: local_offset,
                                value,
                                width: *width,
                            },
                        })
                    };
                    locals.insert(slot, updated);
                } else {
                    debug_assert!(matches!(
                        target,
                        FfWriteTarget::StagedState | FfWriteTarget::WriteOnlyPublication
                    ));
                    let write_only = *target == FfWriteTarget::WriteOnlyPublication;
                    let object_ty = object_type(module, *object)?;
                    let target = object_access(
                        instance_id,
                        *object,
                        offset,
                        *width,
                        object_ty.width,
                        register_values,
                    )?;
                    let stage = EffectId(ir.effects().len());
                    let predecessors = effect_order.stage_predecessors_and_update(&target, stage);
                    let actual_stage = ir.add_effect(Effect {
                        region,
                        predecessors,
                        kind: EffectKind::StageNextFf {
                            process,
                            target,
                            value,
                            guard: None,
                            priority: stages.len(),
                            stage_kind: if write_only {
                                FfStageKind::WriteOnlyPublication
                            } else {
                                FfStageKind::Fragment
                            },
                        },
                    });
                    debug_assert_eq!(actual_stage, stage);
                    stages.push(actual_stage);
                }
            }
            FfBuildOp::Concat(destination, inputs) => {
                let parts = inputs
                    .iter()
                    .map(|input| register_value(register_values, *input))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = ir.add_value(Value {
                    ty: value_type(self.register_type(*destination)?),
                    scope: ValueScope::Process(process),
                    region,
                    kind: ValueKind::Concat { parts },
                });
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Slice(destination, input, offset, width) => {
                let value = ir.add_value(Value {
                    ty: value_type(self.register_type(*destination)?),
                    scope: ValueScope::Process(process),
                    region,
                    kind: ValueKind::Slice {
                        source: register_value(register_values, *input)?,
                        access: bit_access(*offset, *width)
                            .ok_or(FfEirBuildError::RangeOverflow(VarId::SYNTHETIC))?,
                    },
                });
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::Mux(destination, condition, then_value, else_value) => {
                let ty = value_type(self.register_type(*destination)?);
                let then_value = resize_if_needed(
                    ir,
                    ValueScope::Process(process),
                    region,
                    register_value(register_values, *then_value)?,
                    ty,
                );
                let else_value = resize_if_needed(
                    ir,
                    ValueScope::Process(process),
                    region,
                    register_value(register_values, *else_value)?,
                    ty,
                );
                let value = ir.add_value(Value {
                    ty,
                    scope: ValueScope::Process(process),
                    region,
                    kind: ValueKind::Mux {
                        condition: register_value(register_values, *condition)?,
                        then_value,
                        else_value,
                    },
                });
                register_values[destination.0] = Some(value);
            }
            FfBuildOp::RuntimeEvent { site_id, arguments } => {
                let arguments = arguments
                    .iter()
                    .map(|argument| register_value(register_values, *argument))
                    .collect::<Result<Vec<_>, _>>()?;
                let predecessors = effect_order.last_observation.iter().copied().collect();
                let effect = ir.add_effect(Effect {
                    region,
                    predecessors,
                    kind: EffectKind::RuntimeEvent {
                        site_id: *site_id,
                        arguments,
                        guard: None,
                    },
                });
                effect_order.last_observation = Some(effect);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_snapshot_read(
        &self,
        ir: &mut EventIr,
        module: &Module,
        instance_id: InstanceId,
        process: ProcessId,
        region: crate::event_ir::RegionId,
        object: VarId,
        offset: &SIROffset,
        width: usize,
        ty: ValueType,
        register_values: &[Option<ValueId>],
    ) -> Result<ValueId, FfEirBuildError> {
        match offset {
            SIROffset::Static(offset) => {
                self.lower_settled_static_range(ir, instance_id, object, *offset, width, ty)
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                let object_ty = object_type(module, object)?;
                let complete_range = object_range(instance_id, object, 0, object_ty.width)?;
                if ir
                    .comb_graph()
                    .overlapping_definitions(complete_range)?
                    .is_empty()
                {
                    return Ok(ir.add_value(Value {
                        ty,
                        scope: ValueScope::Process(process),
                        region,
                        kind: ValueKind::ReadPersistentMemory {
                            object: complete_range.object,
                            offset: lower_value_offset(offset, register_values)?,
                            width,
                        },
                    }));
                }
                let base = self.lower_settled_static_range(
                    ir,
                    instance_id,
                    object,
                    0,
                    object_ty.width,
                    object_ty,
                )?;
                self.lower_select(
                    ir,
                    process,
                    region,
                    base,
                    offset,
                    width,
                    ty,
                    register_values,
                )
            }
        }
    }

    fn lower_settled_static_range(
        &self,
        ir: &mut EventIr,
        instance_id: InstanceId,
        object: VarId,
        offset: usize,
        width: usize,
        ty: ValueType,
    ) -> Result<ValueId, FfEirBuildError> {
        let requested = object_range(instance_id, object, offset, width)?;
        let definitions = ir.comb_graph().overlapping_definitions(requested)?;
        let mut cursor = requested.access.lsb;
        let end = requested
            .access
            .msb
            .checked_add(1)
            .ok_or(FfEirBuildError::RangeOverflow(object))?;
        let mut parts = Vec::new();

        for definition_id in definitions {
            let definition_target = ir.comb_definitions()[definition_id.0].target;
            let start = cursor.max(definition_target.access.lsb);
            let definition_end = definition_target
                .access
                .msb
                .checked_add(1)
                .ok_or(FfEirBuildError::RangeOverflow(object))?
                .min(end);
            if cursor < start {
                parts.push(self.add_snapshot_part(
                    ir,
                    instance_id,
                    object,
                    cursor,
                    start - cursor,
                    ty,
                )?);
            }
            if start < definition_end {
                let part_width = definition_end - start;
                let relative = start - definition_target.access.lsb;
                let part_ty = part_type(ty, part_width);
                let value = ir.add_value(Value {
                    ty: part_ty,
                    scope: ValueScope::Event,
                    region: ir.root_region(),
                    kind: ValueKind::ReadCombDefinition {
                        definition: definition_id,
                        access: bit_access(relative, part_width)
                            .ok_or(FfEirBuildError::RangeOverflow(object))?,
                    },
                });
                parts.push(value);
                cursor = definition_end;
            }
        }
        if cursor < end {
            parts.push(self.add_snapshot_part(
                ir,
                instance_id,
                object,
                cursor,
                end - cursor,
                ty,
            )?);
        }

        if parts.len() == 1 {
            let value = parts[0];
            return Ok(resize_if_needed(
                ir,
                ValueScope::Event,
                ir.root_region(),
                value,
                ty,
            ));
        }
        parts.reverse();
        Ok(ir.add_value(Value {
            ty,
            scope: ValueScope::Event,
            region: ir.root_region(),
            kind: ValueKind::Concat { parts },
        }))
    }

    fn add_snapshot_part(
        &self,
        ir: &mut EventIr,
        instance_id: InstanceId,
        object: VarId,
        offset: usize,
        width: usize,
        ty: ValueType,
    ) -> Result<ValueId, FfEirBuildError> {
        let range = object_range(instance_id, object, offset, width)?;
        Ok(ir.add_value(Value {
            ty: part_type(ty, width),
            scope: ValueScope::Event,
            region: ir.root_region(),
            kind: ValueKind::ReadClockSnapshot(range),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_select(
        &self,
        ir: &mut EventIr,
        process: ProcessId,
        region: crate::event_ir::RegionId,
        base: ValueId,
        offset: &SIROffset,
        width: usize,
        ty: ValueType,
        register_values: &[Option<ValueId>],
    ) -> Result<ValueId, FfEirBuildError> {
        match offset {
            SIROffset::Static(offset) => {
                if *offset == 0 && width == ir.values()[base.0].ty.width {
                    Ok(resize_if_needed(
                        ir,
                        ValueScope::Process(process),
                        region,
                        base,
                        ty,
                    ))
                } else {
                    Ok(ir.add_value(Value {
                        ty,
                        scope: ValueScope::Process(process),
                        region,
                        kind: ValueKind::Slice {
                            source: base,
                            access: bit_access(*offset, width)
                                .ok_or(FfEirBuildError::RangeOverflow(VarId::SYNTHETIC))?,
                        },
                    }))
                }
            }
            SIROffset::Dynamic(offset) => Ok(ir.add_value(Value {
                ty,
                scope: ValueScope::Process(process),
                region,
                kind: ValueKind::DynamicSelect {
                    source: base,
                    offset: ValueOffset::Dynamic(register_value(register_values, *offset)?),
                    width,
                },
            })),
            SIROffset::Element {
                index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            } => Ok(ir.add_value(Value {
                ty,
                scope: ValueScope::Process(process),
                region,
                kind: ValueKind::DynamicSelect {
                    source: base,
                    offset: ValueOffset::Element {
                        index: register_value(register_values, *index)?,
                        element_width: *element_width,
                        bit_offset: *bit_offset,
                        dynamic_bit_offset: dynamic_bit_offset
                            .map(|offset| register_value(register_values, offset))
                            .transpose()?,
                    },
                    width,
                },
            })),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_terminator(
        &self,
        block: BlockId,
        ir: &mut EventIr,
        process: ProcessId,
        region: crate::event_ir::RegionId,
        process_blocks: &[ControlBlockId],
        local_parameter_order: &[Vec<LocalSlot>],
        locals: &HashMap<LocalSlot, ValueId>,
        register_values: &[Option<ValueId>],
    ) -> Result<ControlTerminator, FfEirBuildError> {
        let recorded = self
            .block(block)?
            .terminator
            .as_ref()
            .ok_or(FfEirBuildError::UnterminatedBlock(block))?;
        match recorded {
            RecordedTerminator::Return => Ok(ControlTerminator::Return),
            RecordedTerminator::Air(FfTerminator::Error(code)) => {
                Ok(ControlTerminator::Error(*code))
            }
            RecordedTerminator::Air(FfTerminator::Jump(target, arguments)) => {
                Ok(ControlTerminator::Jump {
                    target: process_blocks[target.0],
                    arguments: edge_arguments(
                        ir,
                        process,
                        region,
                        *target,
                        process_blocks[target.0],
                        arguments,
                        local_parameter_order,
                        locals,
                        register_values,
                    )?,
                })
            }
            RecordedTerminator::Air(FfTerminator::Branch {
                condition,
                true_block,
                false_block,
            }) => {
                let condition = normalize_condition(
                    ir,
                    process,
                    region,
                    register_value(register_values, *condition)?,
                );
                Ok(ControlTerminator::Branch {
                    condition,
                    true_target: process_blocks[true_block.0.0],
                    true_arguments: edge_arguments(
                        ir,
                        process,
                        region,
                        true_block.0,
                        process_blocks[true_block.0.0],
                        &true_block.1,
                        local_parameter_order,
                        locals,
                        register_values,
                    )?,
                    false_target: process_blocks[false_block.0.0],
                    false_arguments: edge_arguments(
                        ir,
                        process,
                        region,
                        false_block.0,
                        process_blocks[false_block.0.0],
                        &false_block.1,
                        local_parameter_order,
                        locals,
                        register_values,
                    )?,
                })
            }
        }
    }

    fn register_type(&self, register: RegisterId) -> Result<&RegisterType, FfEirBuildError> {
        self.registers
            .get(register.0)
            .ok_or(FfEirBuildError::MissingRegister(register))
    }

    fn successors(&self, block: BlockId) -> Vec<BlockId> {
        match self.blocks[block.0].terminator.as_ref() {
            Some(RecordedTerminator::Air(FfTerminator::Jump(target, _))) => vec![*target],
            Some(RecordedTerminator::Air(FfTerminator::Branch {
                true_block,
                false_block,
                ..
            })) => vec![true_block.0, false_block.0],
            Some(RecordedTerminator::Air(FfTerminator::Error(_)))
            | Some(RecordedTerminator::Return)
            | None => Vec::new(),
        }
    }

    fn mark_write_only_publications(&mut self) {
        let read_objects = self
            .blocks
            .iter()
            .flat_map(|block| block.operations.iter())
            .filter_map(|operation| match operation {
                FfBuildOp::Read { object, .. } => Some(*object),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let write_only_objects = self
            .blocks
            .iter()
            .flat_map(|block| block.operations.iter())
            .filter_map(|operation| match operation {
                FfBuildOp::Write {
                    object,
                    target: FfWriteTarget::StagedState,
                    ..
                } if !read_objects.contains(object) => Some(*object),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for block in &mut self.blocks {
            for operation in &mut block.operations {
                if let FfBuildOp::Write { object, target, .. } = operation
                    && write_only_objects.contains(object)
                    && *target == FfWriteTarget::StagedState
                {
                    *target = FfWriteTarget::WriteOnlyPublication;
                }
            }
        }
    }

    fn final_sink_plan(&self, _module: &Module) -> Result<Vec<FinalSinkPlan>, FfEirBuildError> {
        let successors = (0..self.blocks.len())
            .map(|block| {
                self.successors(BlockId(block))
                    .into_iter()
                    .map(|successor| successor.0)
                    .collect()
            })
            .collect();
        let Ok(cfg) = ControlFlowGraph::analyze_structure(successors, 0) else {
            return Ok(Vec::new());
        };

        let mut writes = HashMap::<VarId, Vec<(usize, usize, usize)>>::default();
        let mut reads = HashMap::<VarId, Vec<(usize, Option<(usize, usize)>)>>::default();
        let mut dynamic_writes = HashSet::default();
        for (block_index, block) in self.blocks.iter().enumerate() {
            for operation in &block.operations {
                match operation {
                    FfBuildOp::Write {
                        object,
                        target: FfWriteTarget::StagedState,
                        offset,
                        width,
                        ..
                    } => {
                        if let SIROffset::Static(offset) = offset {
                            writes
                                .entry(*object)
                                .or_default()
                                .push((block_index, *offset, *width));
                        } else {
                            dynamic_writes.insert(*object);
                        }
                    }
                    FfBuildOp::Read {
                        object,
                        source: FfReadSource::ClockSnapshot,
                        offset,
                        width,
                        ..
                    } => {
                        reads.entry(*object).or_default().push((
                            block_index,
                            match offset {
                                SIROffset::Static(offset) => Some((*offset, *width)),
                                _ => None,
                            },
                        ));
                    }
                    _ => {}
                }
            }
        }

        struct Candidate {
            plan: FinalSinkPlan,
            live_in: Vec<bool>,
            benefit: usize,
        }

        let mut candidates = Vec::new();
        for (object, mut object_writes) in writes {
            if dynamic_writes.contains(&object) {
                continue;
            }
            object_writes.sort_unstable_by_key(|&(_, offset, width)| (offset, width));
            let mut ranges = Vec::<BitAccess>::new();
            for &(_, offset, width) in &object_writes {
                let Some(access) = bit_access(offset, width) else {
                    continue;
                };
                if let Some(last) = ranges.last_mut()
                    && access.lsb <= last.msb
                {
                    last.msb = last.msb.max(access.msb);
                } else {
                    ranges.push(access);
                }
            }

            for access in ranges {
                let range_width = access.msb - access.lsb + 1;
                if range_width > 64 {
                    continue;
                }
                let range_writes = object_writes
                    .iter()
                    .copied()
                    .filter(|&(_, offset, width)| {
                        bit_access(offset, width)
                            .is_some_and(|write| write.lsb <= access.msb && access.lsb <= write.msb)
                    })
                    .collect::<Vec<_>>();
                let mut blocks = range_writes
                    .iter()
                    .map(|&(block, _, _)| block)
                    .collect::<Vec<_>>();
                blocks.extend(reads.get(&object).into_iter().flatten().filter_map(
                    |&(block, read)| {
                        let overlaps = read.is_none_or(|(offset, width)| {
                            bit_access(offset, width).is_some_and(|read| {
                                read.lsb <= access.msb && access.lsb <= read.msb
                            })
                        });
                        overlaps.then_some(block)
                    },
                ));
                blocks.sort_unstable();
                blocks.dedup();
                let Some((&first, rest)) = blocks.split_first() else {
                    continue;
                };
                let mut sink = first;
                let mut valid = true;
                for &block in rest {
                    let Some(common) = cfg.postdominators.common_postdominator(sink, block) else {
                        valid = false;
                        break;
                    };
                    sink = common;
                }
                if !valid || cfg.sccs[cfg.scc_for_block[sink]].cyclic {
                    continue;
                }

                // Solve pruned accumulator liveness before mutating EIR. Every
                // block parameter is a real pressure and edge-copy cost.
                let mut uses = vec![false; self.blocks.len()];
                let mut definitions = vec![false; self.blocks.len()];
                for (block_index, block) in self.blocks.iter().enumerate() {
                    let mut defined = false;
                    for operation in &block.operations {
                        let FfBuildOp::Write {
                            object: written,
                            target: FfWriteTarget::StagedState,
                            offset: SIROffset::Static(offset),
                            width,
                            ..
                        } = operation
                        else {
                            continue;
                        };
                        if *written != object
                            || !(FinalSinkPlan {
                                object,
                                access,
                                sink: BlockId(sink),
                            })
                            .contains(*written, *offset, *width)
                        {
                            continue;
                        }
                        let complete = *offset == access.lsb && *width == range_width;
                        if !complete && !defined {
                            uses[block_index] = true;
                        }
                        defined = true;
                        definitions[block_index] = true;
                    }
                    if block_index == sink && !defined {
                        uses[block_index] = true;
                    }
                }
                let mut live_in = uses.clone();
                let mut live_out = vec![false; self.blocks.len()];
                let mut queued = vec![true; self.blocks.len()];
                let mut worklist = (0..self.blocks.len()).collect::<VecDeque<_>>();
                while let Some(block) = worklist.pop_front() {
                    queued[block] = false;
                    let new_out = cfg.successors[block]
                        .iter()
                        .any(|&successor| live_in[successor]);
                    let new_in = uses[block] || (new_out && !definitions[block]);
                    if new_out != live_out[block] || new_in != live_in[block] {
                        live_out[block] = new_out;
                        live_in[block] = new_in;
                        for &predecessor in &cfg.predecessors[block] {
                            if !queued[predecessor] {
                                queued[predecessor] = true;
                                worklist.push_back(predecessor);
                            }
                        }
                    }
                }
                let boundary_cost = live_in
                    .iter()
                    .enumerate()
                    .filter(|(block, live)| *block != 0 && **live)
                    .count();
                // Replacing N fragments by one sink saves N-1 Stores. A direct
                // sink additionally removes one seed and one commit.
                let benefit = range_writes.len().saturating_add(1);
                if boundary_cost > benefit {
                    continue;
                }
                candidates.push(Candidate {
                    plan: FinalSinkPlan {
                        object,
                        access,
                        sink: BlockId(sink),
                    },
                    live_in,
                    benefit,
                });
            }
        }

        candidates.sort_unstable_by_key(|candidate| {
            (
                std::cmp::Reverse(candidate.benefit),
                candidate.live_in.iter().filter(|live| **live).count(),
                candidate.plan.object,
                candidate.plan.access,
            )
        });
        let mut result = Vec::new();
        let mut boundary_pressure = vec![0usize; self.blocks.len()];
        for candidate in candidates {
            // Leave most GPRs available to the value cone scheduled in each
            // block. Zero-boundary accumulators consume no pressure budget.
            if candidate
                .live_in
                .iter()
                .enumerate()
                .any(|(block, live)| *live && block != 0 && boundary_pressure[block] >= 2)
            {
                continue;
            }
            for (block, live) in candidate.live_in.into_iter().enumerate() {
                if live && block != 0 {
                    boundary_pressure[block] += 1;
                }
            }
            result.push(candidate.plan);
        }
        result.sort_unstable_by_key(|plan| (plan.object, plan.access));
        Ok(result)
    }

    fn analyze_local_liveness(
        &self,
        module: &Module,
        final_sinks: &[FinalSinkPlan],
    ) -> Result<LocalDataflow, FfEirBuildError> {
        let block_count = self.blocks.len();
        let mut uses = vec![HashSet::<LocalSlot>::default(); block_count];
        let mut definitions = vec![HashSet::<LocalSlot>::default(); block_count];
        for (block_index, block) in self.blocks.iter().enumerate() {
            let mut defined = HashSet::default();
            for operation in &block.operations {
                match operation {
                    FfBuildOp::Read {
                        object,
                        source: FfReadSource::ProcessLocal,
                        ..
                    } => {
                        let slot = LocalSlot::Object(*object);
                        if !defined.contains(&slot) {
                            uses[block_index].insert(slot);
                        }
                    }
                    FfBuildOp::Write {
                        object,
                        target,
                        offset,
                        width,
                        ..
                    } => {
                        let (slot, complete) = if *target == FfWriteTarget::ProcessLocal {
                            let object_width = object_type(module, *object)?.width;
                            (
                                LocalSlot::Object(*object),
                                matches!(offset, SIROffset::Static(0)) && *width == object_width,
                            )
                        } else if let SIROffset::Static(offset) = offset
                            && let Some((index, plan)) = final_sinks
                                .iter()
                                .enumerate()
                                .find(|(_, plan)| plan.contains(*object, *offset, *width))
                        {
                            (
                                LocalSlot::FinalRange(index),
                                *offset == plan.access.lsb && *width == plan.width(),
                            )
                        } else {
                            continue;
                        };
                        if !complete && !defined.contains(&slot) {
                            uses[block_index].insert(slot);
                        }
                        defined.insert(slot);
                        definitions[block_index].insert(slot);
                    }
                    _ => {}
                }
            }
            for (index, plan) in final_sinks.iter().enumerate() {
                let slot = LocalSlot::FinalRange(index);
                if plan.sink.0 == block_index && !defined.contains(&slot) {
                    uses[block_index].insert(slot);
                }
            }
        }

        let mut predecessors = vec![Vec::new(); block_count];
        for block in 0..block_count {
            for successor in self.successors(BlockId(block)) {
                predecessors[successor.0].push(block);
            }
        }
        let mut live_in = uses.clone();
        let mut live_out = vec![HashSet::default(); block_count];
        let mut queued = vec![true; block_count];
        let mut worklist = (0..block_count).collect::<VecDeque<_>>();
        while let Some(block) = worklist.pop_front() {
            queued[block] = false;
            let mut new_out = HashSet::default();
            for successor in self.successors(BlockId(block)) {
                new_out.extend(live_in[successor.0].iter().copied());
            }
            let mut new_in = uses[block].clone();
            new_in.extend(
                new_out
                    .iter()
                    .filter(|object| !definitions[block].contains(*object))
                    .copied(),
            );
            if new_out != live_out[block] || new_in != live_in[block] {
                live_out[block] = new_out;
                live_in[block] = new_in;
                for predecessor in &predecessors[block] {
                    if !queued[*predecessor] {
                        queued[*predecessor] = true;
                        worklist.push_back(*predecessor);
                    }
                }
            }
        }

        let parameter_order = live_in
            .iter()
            .map(|objects| {
                let mut objects = objects.iter().copied().collect::<Vec<_>>();
                objects.sort_unstable();
                objects
            })
            .collect();
        Ok(LocalDataflow {
            live_in,
            parameter_order,
            final_sinks: final_sinks.to_vec(),
        })
    }
}

impl FfBuilder for FfEirBuilder {
    fn alloc_logic(&mut self, width: usize) -> RegisterId {
        let id = RegisterId(self.registers.len());
        self.registers.push(RegisterType::Logic { width });
        id
    }

    fn alloc_bit(&mut self, width: usize, signed: bool) -> RegisterId {
        let id = RegisterId(self.registers.len());
        self.registers.push(RegisterType::Bit { width, signed });
        id
    }

    fn register(&self, id: &RegisterId) -> &RegisterType {
        &self.registers[id.0]
    }

    fn emit(&mut self, operation: FfBuildOp) {
        let block = self
            .current_block
            .expect("FF EIR builder has no active block");
        self.blocks[block.0].operations.push(operation);
    }

    fn new_block(&mut self) -> BlockId {
        self.new_block_with(Vec::new())
    }

    fn new_block_with(&mut self, parameters: Vec<RegisterId>) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(RecordedBlock::new(parameters));
        id
    }

    fn switch_to_block(&mut self, block: BlockId) {
        assert!(
            self.current_block.is_none(),
            "attempted to switch an unsealed FF EIR block"
        );
        assert!(block.0 < self.blocks.len(), "absent FF EIR block");
        self.current_block = Some(block);
    }

    fn seal_block(&mut self, terminator: FfTerminator) -> BlockId {
        let block = self
            .current_block
            .take()
            .expect("FF EIR builder has no active block");
        let previous = self.blocks[block.0]
            .terminator
            .replace(RecordedTerminator::Air(terminator));
        assert!(previous.is_none(), "FF EIR block is already terminated");
        block
    }
}

struct LocalDataflow {
    live_in: Vec<HashSet<LocalSlot>>,
    parameter_order: Vec<Vec<LocalSlot>>,
    final_sinks: Vec<FinalSinkPlan>,
}

fn object_type(module: &Module, object: VarId) -> Result<ValueType, FfEirBuildError> {
    let variable = module
        .variables
        .get(&object)
        .ok_or(FfEirBuildError::MissingObject(object))?;
    let width = crate::parser::resolve_total_width(module, variable)
        .map_err(|error| FfEirBuildError::ObjectWidth(object, error.to_string()))?;
    Ok(if variable.r#type.is_2state() {
        ValueType::bit(width, variable.r#type.signed)
    } else {
        ValueType::logic(width, variable.r#type.signed)
    })
}

fn local_slot_type(
    module: &Module,
    final_sinks: &[FinalSinkPlan],
    slot: LocalSlot,
) -> Result<ValueType, FfEirBuildError> {
    match slot {
        LocalSlot::Object(object) => object_type(module, object),
        LocalSlot::FinalRange(index) => {
            let plan = final_sinks[index];
            Ok(part_type(object_type(module, plan.object)?, plan.width()))
        }
    }
}

fn value_type(register: &RegisterType) -> ValueType {
    match register {
        RegisterType::Logic { width } => ValueType::logic(*width, false),
        RegisterType::Bit { width, signed } => ValueType::bit(*width, *signed),
    }
}

fn part_type(ty: ValueType, width: usize) -> ValueType {
    ValueType {
        width,
        signed: false,
        four_state: ty.four_state,
    }
}

fn width_mask(width: usize) -> BigUint {
    (BigUint::one() << width) - BigUint::one()
}

fn bit_access(offset: usize, width: usize) -> Option<BitAccess> {
    let msb = offset.checked_add(width)?.checked_sub(1)?;
    Some(BitAccess::new(offset, msb))
}

fn object_range(
    instance_id: InstanceId,
    object: VarId,
    offset: usize,
    width: usize,
) -> Result<ObjectRange, FfEirBuildError> {
    Ok(ObjectRange::new(
        AbsoluteAddr {
            instance_id,
            var_id: object,
        },
        bit_access(offset, width).ok_or(FfEirBuildError::RangeOverflow(object))?,
    ))
}

fn lower_value_offset(
    offset: &SIROffset,
    register_values: &[Option<ValueId>],
) -> Result<ValueOffset, FfEirBuildError> {
    Ok(match offset {
        SIROffset::Static(offset) => ValueOffset::Static(*offset),
        SIROffset::Dynamic(offset) => {
            ValueOffset::Dynamic(register_value(register_values, *offset)?)
        }
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => ValueOffset::Element {
            index: register_value(register_values, *index)?,
            element_width: *element_width,
            bit_offset: *bit_offset,
            dynamic_bit_offset: dynamic_bit_offset
                .map(|offset| register_value(register_values, offset))
                .transpose()?,
        },
    })
}

fn object_access(
    instance_id: InstanceId,
    object: VarId,
    offset: &SIROffset,
    width: usize,
    object_width: usize,
    register_values: &[Option<ValueId>],
) -> Result<ObjectAccess, FfEirBuildError> {
    let logical_offset = lower_value_offset(offset, register_values)?;
    let alias = match offset {
        SIROffset::Static(offset) => {
            bit_access(*offset, width).ok_or(FfEirBuildError::RangeOverflow(object))?
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            bit_access(0, object_width).ok_or(FfEirBuildError::RangeOverflow(object))?
        }
    };
    Ok(ObjectAccess {
        object: AbsoluteAddr {
            instance_id,
            var_id: object,
        },
        offset: logical_offset,
        width,
        alias,
    })
}

fn register_value(
    values: &[Option<ValueId>],
    register: RegisterId,
) -> Result<ValueId, FfEirBuildError> {
    values
        .get(register.0)
        .copied()
        .flatten()
        .ok_or(FfEirBuildError::MissingValue(register))
}

fn edge_arguments(
    ir: &mut EventIr,
    process: ProcessId,
    region: crate::event_ir::RegionId,
    target: BlockId,
    event_target: ControlBlockId,
    explicit: &[RegisterId],
    local_parameter_order: &[Vec<LocalSlot>],
    locals: &HashMap<LocalSlot, ValueId>,
    register_values: &[Option<ValueId>],
) -> Result<Vec<ValueId>, FfEirBuildError> {
    let mut arguments = explicit
        .iter()
        .map(|register| register_value(register_values, *register))
        .collect::<Result<Vec<_>, _>>()?;
    for slot in &local_parameter_order[target.0] {
        arguments.push(
            *locals
                .get(slot)
                .ok_or(FfEirBuildError::MissingLocalValue(*slot, target))?,
        );
    }
    let parameters = ir.blocks()[event_target.0].parameters.clone();
    debug_assert_eq!(arguments.len(), parameters.len());
    for (argument, parameter) in arguments.iter_mut().zip(parameters) {
        let target_ty = ir.values()[parameter.0].ty;
        *argument = resize_if_needed(
            ir,
            ValueScope::Process(process),
            region,
            *argument,
            target_ty,
        );
    }
    Ok(arguments)
}

fn resize_if_needed(
    ir: &mut EventIr,
    scope: ValueScope,
    region: crate::event_ir::RegionId,
    mut input: ValueId,
    ty: ValueType,
) -> ValueId {
    if ir.values()[input.0].ty == ty {
        return input;
    }
    if ir.values()[input.0].ty.four_state && !ty.four_state {
        let input_ty = ir.values()[input.0].ty;
        input = ir.add_value(Value {
            ty: ValueType::bit(input_ty.width, input_ty.signed),
            scope,
            region,
            kind: ValueKind::Unary {
                op: UnaryOp::ToTwoState,
                input,
            },
        });
        if ir.values()[input.0].ty == ty {
            return input;
        }
    }
    ir.add_value(Value {
        ty,
        scope,
        region,
        kind: ValueKind::Resize { input },
    })
}

fn normalize_condition(
    ir: &mut EventIr,
    process: ProcessId,
    region: crate::event_ir::RegionId,
    condition: ValueId,
) -> ValueId {
    let ty = ir.values()[condition.0].ty;
    if ty == ValueType::bit(1, false) {
        return condition;
    }
    let truth = if ty.width == 1 {
        condition
    } else {
        ir.add_value(Value {
            ty: ValueType {
                width: 1,
                signed: false,
                four_state: ty.four_state,
            },
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Unary {
                op: UnaryOp::Or,
                input: condition,
            },
        })
    };
    if !ir.values()[truth.0].ty.four_state {
        truth
    } else {
        ir.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Unary {
                op: UnaryOp::ToTwoState,
                input: truth,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir, Module},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    use super::*;
    use crate::{
        event_ir::{CombGraph, EventDomain, EventProjection, lower_event_projection},
        ir::SIRInstruction,
        logic_tree::SLTNodeArena,
        parser::{BuildConfig, ff::FfParser},
    };

    fn analyze_module(code: &str) -> Module {
        symbol_table::clear();
        attribute_table::clear();
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut analyzer_ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut analyzer_ir),)
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&analyzer_ir).is_empty());
        analyzer_ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn merges_static_branch_writes_into_one_verified_final_ff_sink() {
        let code = r#"
module Top (
    clk: input clock,
    enable: input logic,
    data: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk) {
        if enable && data[0] {
            q = data + 8'd1;
        } else {
            q = q;
        }
    }
}
"#;
        let module = analyze_module(code);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .unwrap();
        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = FfEirBuilder::new();
        parser.parse_ff_group(&[declaration], &mut builder).unwrap();

        let instance_id = InstanceId(0);
        let clock = AbsoluteAddr {
            instance_id,
            var_id: declaration.clock.id,
        };
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let stages = builder
            .lower_into(&mut event, &module, instance_id, 0, Vec::new())
            .unwrap();
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });

        event.verify().unwrap();
        assert_eq!(event.processes().len(), 1);
        let stages = event
            .effects()
            .iter()
            .filter_map(|effect| match &effect.kind {
                EffectKind::StageNextFf { stage_kind, .. } => Some(*stage_kind),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stages, vec![FfStageKind::FinalProcessSink]);
        assert!(
            event
                .blocks()
                .iter()
                .any(|block| matches!(block.terminator, Some(ControlTerminator::Branch { .. }))),
            "AIR control must remain explicit EIR control"
        );
        assert!(
            event
                .values()
                .iter()
                .any(|value| matches!(value.kind, ValueKind::ReadClockSnapshot(_)))
        );
    }

    #[test]
    fn sinks_a_small_static_range_without_accumulating_its_wide_object() {
        let code = r#"
module Top (
    clk   : input  clock,
    enable: input  logic,
    data  : input  logic<32>,
    q     : output logic<256>,
) {
    always_ff (clk) {
        if enable {
            q[95:64] = data;
        } else {
            q[95:64] = q[95:64];
        }
    }
}
"#;
        let module = analyze_module(code);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .unwrap();
        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = FfEirBuilder::new();
        parser.parse_ff_group(&[declaration], &mut builder).unwrap();

        let instance_id = InstanceId(0);
        let clock = AbsoluteAddr {
            instance_id,
            var_id: declaration.clock.id,
        };
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let stages = builder
            .lower_into(&mut event, &module, instance_id, 0, Vec::new())
            .unwrap();
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });
        event.verify().unwrap();

        let stage_targets = event
            .effects()
            .iter()
            .filter_map(|effect| match &effect.kind {
                EffectKind::StageNextFf {
                    target, stage_kind, ..
                } => Some((target.clone(), *stage_kind)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stage_targets.len(), 1);
        assert_eq!(stage_targets[0].0.offset, ValueOffset::Static(64));
        assert_eq!(stage_targets[0].0.width, 32);
        assert_eq!(stage_targets[0].0.alias, BitAccess::new(64, 95));
        assert_eq!(stage_targets[0].1, FfStageKind::FinalProcessSink);
    }

    #[test]
    fn marks_only_air_proven_write_only_state_for_direct_publication() {
        let code = r#"
module Top (
    clk : input  clock,
    data: input  logic<64>,
) {
    var write_only: logic<64>;
    var feedback  : logic<64>;
    always_ff (clk) {
        write_only = data;
        feedback = feedback + data;
    }
}
"#;
        let module = analyze_module(code);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .unwrap();
        let write_only = module
            .variables
            .values()
            .find(|variable| variable.token.beg.text.to_string() == "write_only")
            .unwrap()
            .id;
        let feedback = module
            .variables
            .values()
            .find(|variable| variable.token.beg.text.to_string() == "feedback")
            .unwrap()
            .id;
        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = FfEirBuilder::new();
        parser.parse_ff_group(&[declaration], &mut builder).unwrap();

        let instance_id = InstanceId(0);
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: AbsoluteAddr {
                    instance_id,
                    var_id: declaration.clock.id,
                },
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        builder
            .lower_into(&mut event, &module, instance_id, 0, Vec::new())
            .unwrap();
        let kinds = event
            .effects()
            .iter()
            .filter_map(|effect| match &effect.kind {
                EffectKind::StageNextFf {
                    target, stage_kind, ..
                } => Some((target.object.var_id, *stage_kind)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        assert_eq!(kinds[&write_only], FfStageKind::WriteOnlyPublication);
        assert_ne!(kinds[&feedback], FfStageKind::WriteOnlyPublication);
    }

    #[test]
    fn dynamic_snapshot_memory_read_remains_a_narrow_element_load() {
        let code = r#"
module Top (
    clk : input  clock,
    addr: input  logic<20>,
    q   : output logic<32>,
) {
    var mem: logic<32> [1048576];
    always_ff (clk) {
        q = mem[addr];
    }
}
"#;
        let module = analyze_module(code);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .unwrap();
        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = FfEirBuilder::new();
        parser.parse_ff_group(&[declaration], &mut builder).unwrap();

        let instance_id = InstanceId(0);
        let clock = AbsoluteAddr {
            instance_id,
            var_id: declaration.clock.id,
        };
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock,
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let stages = builder
            .lower_into(&mut event, &module, instance_id, 0, Vec::new())
            .unwrap();
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });
        event.verify().unwrap();

        let memory = event
            .values()
            .iter()
            .find_map(|value| match &value.kind {
                ValueKind::ReadPersistentMemory {
                    object,
                    offset:
                        ValueOffset::Element {
                            element_width: 32, ..
                        },
                    width: 32,
                } => Some(object.var_id),
                _ => None,
            })
            .expect("dynamic array read must remain an element-addressed memory read");
        assert!(
            event.values().iter().all(|value| value.ty.width <= 64),
            "the complete 32-Mibit memory must not become an EIR SSA value"
        );

        let sir = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &SLTNodeArena::new(),
            false,
            clock,
        )
        .unwrap();
        assert!(
            sir.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Load(
                        _,
                        address,
                        SIROffset::Element {
                            element_width: 32,
                            ..
                        },
                        32,
                    ) if address.var_id == memory
                ))
        );
        assert!(
            sir.register_map
                .values()
                .all(|register| register.width() <= 64),
            "the complete 32-Mibit memory must not become a SIR register"
        );
    }
}
