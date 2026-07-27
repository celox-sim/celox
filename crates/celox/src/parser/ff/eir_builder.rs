use std::collections::VecDeque;

use num_bigint::BigUint;
use num_traits::One;
use thiserror::Error;
use veryl_analyzer::ir::{Module, VarId};

use crate::{
    HashMap, HashSet,
    event_ir::{
        BitAccess, ControlBlockId, ControlTerminator, Effect, EffectId, EffectKind, EventIr,
        ObjectAccess, ObjectRange, ProcessId, Value, ValueId, ValueKind, ValueOffset, ValueScope,
        ValueType,
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
    #[error("FF EIR builder local {0} has no reaching value in block {1:?}")]
    MissingLocalValue(VarId, BlockId),
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
        let process = ir.add_process_with_resets(source_order, resets);
        let process_blocks = self.create_control_blocks(ir, process);
        let local_dataflow = self.analyze_local_liveness(module)?;
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
        let mut block_last_effect = vec![None; self.blocks.len()];

        for block in block_order.iter().copied() {
            let eir_block = process_blocks[block.0];
            let region = ir.blocks()[eir_block.0].region;
            let mut locals = if block == BlockId(0) {
                self.create_entry_locals(ir, module, instance_id, &local_dataflow.live_in[block.0])?
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
                    &mut block_last_effect[block.0],
                    &mut stages,
                )?;
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
        local_parameters: &mut [HashMap<VarId, ValueId>],
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
            for object in &local_dataflow.parameter_order[block_index] {
                let ty = object_type(module, *object)?;
                let value = ir.add_block_parameter(eir_block, ty);
                local_parameters[block_index].insert(*object, value);
            }
        }
        Ok(())
    }

    fn create_entry_locals(
        &self,
        ir: &mut EventIr,
        module: &Module,
        instance_id: InstanceId,
        live_in: &HashSet<VarId>,
    ) -> Result<HashMap<VarId, ValueId>, FfEirBuildError> {
        let mut objects = live_in.iter().copied().collect::<Vec<_>>();
        objects.sort_unstable();
        let mut values = HashMap::default();
        for object in objects {
            let ty = object_type(module, object)?;
            let range = object_range(instance_id, object, 0, ty.width)?;
            let value = ir.add_value(Value {
                ty,
                scope: ValueScope::Event,
                region: ir.root_region(),
                kind: ValueKind::ReadClockSnapshot(range),
            });
            values.insert(object, value);
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
        locals: &mut HashMap<VarId, ValueId>,
        last_effect: &mut Option<EffectId>,
        stages: &mut Vec<EffectId>,
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
                        let base = *locals
                            .get(object)
                            .ok_or(FfEirBuildError::MissingLocalValue(*object, block))?;
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
                match target {
                    FfWriteTarget::ProcessLocal => {
                        let object_ty = object_type(module, *object)?;
                        let complete =
                            matches!(offset, SIROffset::Static(0)) && *width == object_ty.width;
                        let updated = if complete {
                            resize_if_needed(
                                ir,
                                ValueScope::Process(process),
                                region,
                                value,
                                object_ty,
                            )
                        } else {
                            let base = *locals
                                .get(object)
                                .ok_or(FfEirBuildError::MissingLocalValue(*object, block))?;
                            ir.add_value(Value {
                                ty: object_ty,
                                scope: ValueScope::Process(process),
                                region,
                                kind: ValueKind::UpdateRange {
                                    base,
                                    offset: lower_value_offset(offset, register_values)?,
                                    value,
                                    width: *width,
                                },
                            })
                        };
                        locals.insert(*object, updated);
                    }
                    FfWriteTarget::StagedState => {
                        let object_ty = object_type(module, *object)?;
                        let target = object_access(
                            instance_id,
                            *object,
                            offset,
                            *width,
                            object_ty.width,
                            register_values,
                        )?;
                        let predecessors = last_effect.iter().copied().collect();
                        let stage = ir.add_effect(Effect {
                            region,
                            predecessors,
                            kind: EffectKind::StageNextFf {
                                process,
                                target,
                                value,
                                guard: None,
                                priority: stages.len(),
                            },
                        });
                        *last_effect = Some(stage);
                        stages.push(stage);
                    }
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
                let predecessors = last_effect.iter().copied().collect();
                let effect = ir.add_effect(Effect {
                    region,
                    predecessors,
                    kind: EffectKind::RuntimeEvent {
                        site_id: *site_id,
                        arguments,
                        guard: None,
                    },
                });
                *last_effect = Some(effect);
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
        local_parameter_order: &[Vec<VarId>],
        locals: &HashMap<VarId, ValueId>,
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

    fn analyze_local_liveness(&self, module: &Module) -> Result<LocalDataflow, FfEirBuildError> {
        let block_count = self.blocks.len();
        let mut uses = vec![HashSet::default(); block_count];
        let mut definitions = vec![HashSet::default(); block_count];
        for (block_index, block) in self.blocks.iter().enumerate() {
            let mut defined = HashSet::default();
            for operation in &block.operations {
                match operation {
                    FfBuildOp::Read {
                        object,
                        source: FfReadSource::ProcessLocal,
                        ..
                    } => {
                        if !defined.contains(object) {
                            uses[block_index].insert(*object);
                        }
                    }
                    FfBuildOp::Write {
                        object,
                        target: FfWriteTarget::ProcessLocal,
                        offset,
                        width,
                        ..
                    } => {
                        let object_width = object_type(module, *object)?.width;
                        let complete =
                            matches!(offset, SIROffset::Static(0)) && *width == object_width;
                        if !complete && !defined.contains(object) {
                            uses[block_index].insert(*object);
                        }
                        defined.insert(*object);
                        definitions[block_index].insert(*object);
                    }
                    _ => {}
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
    live_in: Vec<HashSet<VarId>>,
    parameter_order: Vec<Vec<VarId>>,
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
    local_parameter_order: &[Vec<VarId>],
    locals: &HashMap<VarId, ValueId>,
    register_values: &[Option<ValueId>],
) -> Result<Vec<ValueId>, FfEirBuildError> {
    let mut arguments = explicit
        .iter()
        .map(|register| register_value(register_values, *register))
        .collect::<Result<Vec<_>, _>>()?;
    for object in &local_parameter_order[target.0] {
        arguments.push(
            *locals
                .get(object)
                .ok_or(FfEirBuildError::MissingLocalValue(*object, target))?,
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
    fn lowers_air_branch_reads_and_stages_directly_to_verified_eir() {
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
        assert_eq!(
            event
                .effects()
                .iter()
                .filter(|effect| matches!(effect.kind, EffectKind::StageNextFf { .. }))
                .count(),
            2
        );
        assert!(
            event
                .values()
                .iter()
                .any(|value| matches!(value.kind, ValueKind::ReadClockSnapshot(_)))
        );
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
