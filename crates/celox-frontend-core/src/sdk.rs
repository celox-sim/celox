//! Projection from the stable frontend SDK into Celox symbolic internals.

use celox_design::{
    BinaryOp, BitAccess, DomainKind, InitialStateData, InitialStateValue, ModuleId, PortTypeKind,
    RegionedVarAddrBase, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, VariableMetadata,
    WORKING_REGION,
};
use celox_frontend_sdk::{
    ActiveLevel, Direction, Edge, ExprId, ExprNode, FrontendArtifact, SignalId, SignalSlice,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, merge_sir_eus,
};
use celox_slt::{LogicPath, LogicPathTarget, NodeId, SLTNode, SLTNodeArena};
use thiserror::Error;

use crate::symbolic::artifact::{
    ExternalHierarchy, ExternalModule, SimModule, SymbolicRtl, SymbolicVariable,
};
use crate::{HashMap, HashSet, SourceVarId, VariableKind};

type RegionedSourceAddr = RegionedVarAddrBase<SourceVarId>;

/// Internal artifacts derived from one stable SDK value.
///
/// `symbolic` compiles the artifact as a standalone design. `external` lets a
/// Veryl native testbench instantiate the same module without teaching the SDK
/// about Veryl syntax.
pub struct LoweredFrontendArtifact {
    pub symbolic: SymbolicRtl,
    pub external: ExternalHierarchy,
}

#[derive(Debug, Error)]
pub enum FrontendArtifactError {
    #[error("invalid frontend artifact: {0}")]
    Validation(#[from] celox_frontend_sdk::BuildError),
    #[error("frontend artifact references unknown signal {0}")]
    UnknownSignal(u32),
    #[error("frontend artifact references unknown expression {0}")]
    UnknownExpression(u32),
    #[error("unsupported frontend SDK expression or operation")]
    UnsupportedOperation,
    #[error("signal `{signal}` is used with conflicting clock/reset roles")]
    ConflictingSignalRole { signal: String },
    #[error("frontend SDK expression is invalid: {0}")]
    InvalidExpression(#[from] celox_slt::SLTNodeFactsError),
}

fn source_id(id: SignalId) -> SourceVarId {
    SourceVarId(id.index())
}

fn signal_atom(slice: SignalSlice) -> VarAtomBase<SourceVarId> {
    VarAtomBase::new(
        source_id(slice.signal()),
        slice.lsb(),
        slice.lsb() + slice.width() - 1,
    )
}

fn binary_op(op: celox_frontend_sdk::BinaryOp) -> Result<BinaryOp, FrontendArtifactError> {
    Ok(match op {
        celox_frontend_sdk::BinaryOp::Add => BinaryOp::Add,
        celox_frontend_sdk::BinaryOp::Sub => BinaryOp::Sub,
        celox_frontend_sdk::BinaryOp::Mul => BinaryOp::Mul,
        celox_frontend_sdk::BinaryOp::DivUnsigned => BinaryOp::DivU,
        celox_frontend_sdk::BinaryOp::DivSigned => BinaryOp::DivS,
        celox_frontend_sdk::BinaryOp::RemUnsigned => BinaryOp::RemU,
        celox_frontend_sdk::BinaryOp::RemSigned => BinaryOp::RemS,
        celox_frontend_sdk::BinaryOp::And => BinaryOp::And,
        celox_frontend_sdk::BinaryOp::Or => BinaryOp::Or,
        celox_frontend_sdk::BinaryOp::Xor => BinaryOp::Xor,
        celox_frontend_sdk::BinaryOp::ShiftLeft => BinaryOp::Shl,
        celox_frontend_sdk::BinaryOp::ShiftRight => BinaryOp::Shr,
        celox_frontend_sdk::BinaryOp::ArithmeticShiftRight => BinaryOp::Sar,
        celox_frontend_sdk::BinaryOp::Equal => BinaryOp::Eq,
        celox_frontend_sdk::BinaryOp::NotEqual => BinaryOp::Ne,
        celox_frontend_sdk::BinaryOp::CaseEqual => BinaryOp::EqCase,
        celox_frontend_sdk::BinaryOp::CaseNotEqual => BinaryOp::NeCase,
        celox_frontend_sdk::BinaryOp::LessUnsigned => BinaryOp::LtU,
        celox_frontend_sdk::BinaryOp::LessSigned => BinaryOp::LtS,
        celox_frontend_sdk::BinaryOp::LessEqualUnsigned => BinaryOp::LeU,
        celox_frontend_sdk::BinaryOp::LessEqualSigned => BinaryOp::LeS,
        celox_frontend_sdk::BinaryOp::GreaterUnsigned => BinaryOp::GtU,
        celox_frontend_sdk::BinaryOp::GreaterSigned => BinaryOp::GtS,
        celox_frontend_sdk::BinaryOp::GreaterEqualUnsigned => BinaryOp::GeU,
        celox_frontend_sdk::BinaryOp::GreaterEqualSigned => BinaryOp::GeS,
        celox_frontend_sdk::BinaryOp::LogicAnd => BinaryOp::LogicAnd,
        celox_frontend_sdk::BinaryOp::LogicOr => BinaryOp::LogicOr,
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    })
}

fn unary_op(op: celox_frontend_sdk::UnaryOp) -> Result<UnaryOp, FrontendArtifactError> {
    Ok(match op {
        celox_frontend_sdk::UnaryOp::ToTwoState => UnaryOp::ToTwoState,
        celox_frontend_sdk::UnaryOp::Negate => UnaryOp::Minus,
        celox_frontend_sdk::UnaryOp::BitNot => UnaryOp::BitNot,
        celox_frontend_sdk::UnaryOp::LogicNot => UnaryOp::LogicNot,
        celox_frontend_sdk::UnaryOp::ReduceAnd => UnaryOp::And,
        celox_frontend_sdk::UnaryOp::ReduceOr => UnaryOp::Or,
        celox_frontend_sdk::UnaryOp::ReduceXor => UnaryOp::Xor,
        celox_frontend_sdk::UnaryOp::PopCount => UnaryOp::PopCount,
        celox_frontend_sdk::UnaryOp::CountLeadingZeros => UnaryOp::CountLeadingZeros,
        celox_frontend_sdk::UnaryOp::CountTrailingZeros => UnaryOp::CountTrailingZeros,
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    })
}

fn expression_sources(
    artifact: &FrontendArtifact,
    id: ExprId,
    sources: &mut HashSet<VarAtomBase<SourceVarId>>,
) -> Result<(), FrontendArtifactError> {
    let expression = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?;
    match expression.node() {
        ExprNode::Signal(slice) => {
            sources.insert(signal_atom(*slice));
        }
        ExprNode::Constant(_) => {}
        ExprNode::Binary { lhs, rhs, .. } => {
            expression_sources(artifact, *lhs, sources)?;
            expression_sources(artifact, *rhs, sources)?;
        }
        ExprNode::Unary { input, .. } | ExprNode::Slice { input, .. } => {
            expression_sources(artifact, *input, sources)?;
        }
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expression_sources(artifact, *condition, sources)?;
            expression_sources(artifact, *then_expr, sources)?;
            expression_sources(artifact, *else_expr, sources)?;
        }
        ExprNode::Concat(parts) => {
            for part in parts {
                expression_sources(artifact, *part, sources)?;
            }
        }
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    }
    Ok(())
}

fn lower_slt_expression(
    artifact: &FrontendArtifact,
    id: ExprId,
    arena: &mut SLTNodeArena<SourceVarId>,
    cache: &mut HashMap<ExprId, NodeId>,
) -> Result<NodeId, FrontendArtifactError> {
    if let Some(node) = cache.get(&id) {
        return Ok(*node);
    }
    let expression = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?;
    let node = match expression.node() {
        ExprNode::Signal(slice) => SLTNode::Input {
            variable: source_id(slice.signal()),
            signed: artifact
                .signal(slice.signal())
                .ok_or(FrontendArtifactError::UnknownSignal(slice.signal().index()))?
                .value_type()
                .is_signed(),
            index: Vec::new(),
            access: BitAccess::new(slice.lsb(), slice.lsb() + slice.width() - 1),
        },
        ExprNode::Constant(value) => SLTNode::Constant(
            value.payload().clone(),
            value.mask().clone(),
            value.value_type().width(),
            value.value_type().is_signed(),
        ),
        ExprNode::Binary { op, lhs, rhs } => SLTNode::Binary(
            lower_slt_expression(artifact, *lhs, arena, cache)?,
            binary_op(*op)?,
            lower_slt_expression(artifact, *rhs, arena, cache)?,
        ),
        ExprNode::Unary { op, input } => SLTNode::Unary(
            unary_op(*op)?,
            lower_slt_expression(artifact, *input, arena, cache)?,
        ),
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => SLTNode::Mux {
            cond: lower_slt_expression(artifact, *condition, arena, cache)?,
            then_expr: lower_slt_expression(artifact, *then_expr, arena, cache)?,
            else_expr: lower_slt_expression(artifact, *else_expr, arena, cache)?,
        },
        ExprNode::Concat(parts) => SLTNode::Concat(
            parts
                .iter()
                .map(|part| {
                    let expression = artifact
                        .expression(*part)
                        .ok_or(FrontendArtifactError::UnknownExpression(part.index()))?;
                    Ok((
                        lower_slt_expression(artifact, *part, arena, cache)?,
                        expression.value_type().width(),
                    ))
                })
                .collect::<Result<Vec<_>, FrontendArtifactError>>()?,
        ),
        ExprNode::Slice { input, lsb } => SLTNode::Slice {
            expr: lower_slt_expression(artifact, *input, arena, cache)?,
            access: BitAccess::new(*lsb, *lsb + expression.value_type().width() - 1),
        },
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    };
    let node = arena.alloc(node)?;
    cache.insert(id, node);
    Ok(node)
}

fn alloc_register(
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    ty: celox_frontend_sdk::ValueType,
) -> RegisterId {
    if ty.is_four_state() {
        builder.alloc_logic(ty.width())
    } else {
        builder.alloc_bit(ty.width(), ty.is_signed())
    }
}

fn lower_sir_expression(
    artifact: &FrontendArtifact,
    id: ExprId,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    cache: &mut HashMap<ExprId, RegisterId>,
) -> Result<RegisterId, FrontendArtifactError> {
    if let Some(register) = cache.get(&id) {
        return Ok(*register);
    }
    let expression = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?;
    let result = alloc_register(builder, expression.value_type());
    match expression.node() {
        ExprNode::Signal(slice) => builder.emit(SIRInstruction::Load(
            result,
            RegionedSourceAddr {
                region: STABLE_REGION,
                var_id: source_id(slice.signal()),
            },
            SIROffset::Static(slice.lsb()),
            slice.width(),
        )),
        ExprNode::Constant(value) => builder.emit(SIRInstruction::Imm(
            result,
            SIRValue::new_four_state(value.payload().clone(), value.mask().clone()),
        )),
        ExprNode::Binary { op, lhs, rhs } => {
            let lhs = lower_sir_expression(artifact, *lhs, builder, cache)?;
            let rhs = lower_sir_expression(artifact, *rhs, builder, cache)?;
            builder.emit(SIRInstruction::Binary(result, lhs, binary_op(*op)?, rhs));
        }
        ExprNode::Unary { op, input } => {
            let input = lower_sir_expression(artifact, *input, builder, cache)?;
            builder.emit(SIRInstruction::Unary(result, unary_op(*op)?, input));
        }
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = lower_sir_expression(artifact, *condition, builder, cache)?;
            let then_expr = lower_sir_expression(artifact, *then_expr, builder, cache)?;
            let else_expr = lower_sir_expression(artifact, *else_expr, builder, cache)?;
            builder.emit(SIRInstruction::Mux(result, condition, then_expr, else_expr));
        }
        ExprNode::Concat(parts) => {
            let parts = parts
                .iter()
                .map(|part| lower_sir_expression(artifact, *part, builder, cache))
                .collect::<Result<Vec<_>, _>>()?;
            builder.emit(SIRInstruction::Concat(result, parts));
        }
        ExprNode::Slice { input, lsb } => {
            let input = lower_sir_expression(artifact, *input, builder, cache)?;
            builder.emit(SIRInstruction::Slice(
                result,
                input,
                *lsb,
                expression.value_type().width(),
            ));
        }
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    }
    cache.insert(id, result);
    Ok(result)
}

fn lower_control(
    artifact: &FrontendArtifact,
    signal: SignalId,
    active: ActiveLevel,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
) -> Result<RegisterId, FrontendArtifactError> {
    let signal_info = artifact
        .signal(signal)
        .ok_or(FrontendArtifactError::UnknownSignal(signal.index()))?;
    let loaded = alloc_register(builder, signal_info.value_type());
    builder.emit(SIRInstruction::Load(
        loaded,
        RegionedSourceAddr {
            region: STABLE_REGION,
            var_id: source_id(signal),
        },
        SIROffset::Static(0),
        1,
    ));
    let two_state = if signal_info.value_type().is_four_state() {
        let result = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(result, UnaryOp::ToTwoState, loaded));
        result
    } else {
        loaded
    };
    if active == ActiveLevel::High {
        return Ok(two_state);
    }
    let inverted = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Unary(
        inverted,
        UnaryOp::LogicNot,
        two_state,
    ));
    Ok(inverted)
}

fn seal_builder(mut builder: SIRBuilder<RegionedSourceAddr>) -> ExecutionUnit<RegionedSourceAddr> {
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

fn insert_or_merge(
    blocks: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedSourceAddr>>,
    trigger: TriggerSet<SourceVarId>,
    unit: ExecutionUnit<RegionedSourceAddr>,
) {
    if let Some(existing) = blocks.remove(&trigger) {
        blocks.insert(trigger, merge_sir_eus(&[existing, unit]).0);
    } else {
        blocks.insert(trigger, unit);
    }
}

fn lower_registers(
    artifact: &FrontendArtifact,
    eval_only: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedSourceAddr>>,
    apply: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedSourceAddr>>,
    eval_apply: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedSourceAddr>>,
    reset_clock_map: &mut HashMap<SourceVarId, SourceVarId>,
) -> Result<(), FrontendArtifactError> {
    for register in artifact.registers() {
        let target = register.target();
        let target_id = source_id(target.signal());
        let trigger = TriggerSet {
            clock: source_id(register.clock()),
            resets: register
                .async_reset()
                .into_iter()
                .map(|reset| source_id(reset.signal()))
                .collect(),
        };
        if let Some(reset) = register.async_reset() {
            reset_clock_map.insert(source_id(reset.signal()), source_id(register.clock()));
        }

        let build_eval =
            |commit: bool| -> Result<ExecutionUnit<RegionedSourceAddr>, FrontendArtifactError> {
                let mut builder = SIRBuilder::new();
                builder.emit(SIRInstruction::Commit(
                    RegionedSourceAddr {
                        region: STABLE_REGION,
                        var_id: target_id,
                    },
                    RegionedSourceAddr {
                        region: WORKING_REGION,
                        var_id: target_id,
                    },
                    SIROffset::Static(0),
                    target.width(),
                    Vec::new(),
                ));
                let mut cache = HashMap::default();
                let mut next =
                    lower_sir_expression(artifact, register.next(), &mut builder, &mut cache)?;
                if let Some(enable) = register.enable() {
                    let condition =
                        lower_control(artifact, enable.signal(), enable.active(), &mut builder)?;
                    let target_info = artifact.signal(target.signal()).ok_or(
                        FrontendArtifactError::UnknownSignal(target.signal().index()),
                    )?;
                    let current = alloc_register(&mut builder, target_info.value_type());
                    builder.emit(SIRInstruction::Load(
                        current,
                        RegionedSourceAddr {
                            region: STABLE_REGION,
                            var_id: target_id,
                        },
                        SIROffset::Static(0),
                        target.width(),
                    ));
                    let selected = alloc_register(&mut builder, target_info.value_type());
                    builder.emit(SIRInstruction::Mux(selected, condition, next, current));
                    next = selected;
                }
                if let Some(reset) = register.async_reset() {
                    let condition =
                        lower_control(artifact, reset.signal(), reset.active(), &mut builder)?;
                    let reset_value =
                        lower_sir_expression(artifact, reset.value(), &mut builder, &mut cache)?;
                    let target_info = artifact.signal(target.signal()).ok_or(
                        FrontendArtifactError::UnknownSignal(target.signal().index()),
                    )?;
                    let selected = alloc_register(&mut builder, target_info.value_type());
                    builder.emit(SIRInstruction::Mux(selected, condition, reset_value, next));
                    next = selected;
                }
                builder.emit(SIRInstruction::Store(
                    RegionedSourceAddr {
                        region: WORKING_REGION,
                        var_id: target_id,
                    },
                    SIROffset::Static(0),
                    target.width(),
                    next,
                    Vec::new(),
                    Vec::new(),
                ));
                if commit {
                    builder.emit(SIRInstruction::Commit(
                        RegionedSourceAddr {
                            region: WORKING_REGION,
                            var_id: target_id,
                        },
                        RegionedSourceAddr {
                            region: STABLE_REGION,
                            var_id: target_id,
                        },
                        SIROffset::Static(0),
                        target.width(),
                        Vec::new(),
                    ));
                }
                Ok(seal_builder(builder))
            };

        let mut apply_builder = SIRBuilder::new();
        apply_builder.emit(SIRInstruction::Commit(
            RegionedSourceAddr {
                region: WORKING_REGION,
                var_id: target_id,
            },
            RegionedSourceAddr {
                region: STABLE_REGION,
                var_id: target_id,
            },
            SIROffset::Static(0),
            target.width(),
            Vec::new(),
        ));
        insert_or_merge(eval_only, trigger.clone(), build_eval(false)?);
        insert_or_merge(apply, trigger.clone(), seal_builder(apply_builder));
        insert_or_merge(eval_apply, trigger, build_eval(true)?);
    }
    Ok(())
}

fn set_role(
    roles: &mut HashMap<SignalId, (DomainKind, PortTypeKind)>,
    artifact: &FrontendArtifact,
    signal: SignalId,
    role: (DomainKind, PortTypeKind),
) -> Result<(), FrontendArtifactError> {
    if let Some(existing) = roles.get(&signal) {
        if *existing != role {
            let signal = artifact
                .signal(signal)
                .ok_or(FrontendArtifactError::UnknownSignal(signal.index()))?;
            return Err(FrontendArtifactError::ConflictingSignalRole {
                signal: signal.name().to_string(),
            });
        }
    } else {
        roles.insert(signal, role);
    }
    Ok(())
}

/// Validate and project a stable SDK artifact into Celox symbolic structures.
pub fn lower_frontend_artifact(
    artifact: &FrontendArtifact,
) -> Result<LoweredFrontendArtifact, FrontendArtifactError> {
    artifact.validate()?;
    let mut roles = HashMap::default();
    for register in artifact.registers() {
        set_role(
            &mut roles,
            artifact,
            register.clock(),
            match register.edge() {
                Edge::Posedge => (DomainKind::ClockPosedge, PortTypeKind::Clock),
                Edge::Negedge => (DomainKind::ClockNegedge, PortTypeKind::Clock),
            },
        )?;
        if let Some(reset) = register.async_reset() {
            set_role(
                &mut roles,
                artifact,
                reset.signal(),
                match reset.active() {
                    ActiveLevel::High => (DomainKind::ResetAsyncHigh, PortTypeKind::ResetAsyncHigh),
                    ActiveLevel::Low => (DomainKind::ResetAsyncLow, PortTypeKind::ResetAsyncLow),
                },
            )?;
        }
    }

    let variables = artifact
        .signals()
        .iter()
        .map(|signal| {
            let (kind, type_kind) = roles.get(&signal.id()).copied().unwrap_or((
                DomainKind::Other,
                if signal.value_type().is_four_state() {
                    PortTypeKind::Logic
                } else {
                    PortTypeKind::Bit
                },
            ));
            let variable_kind = match signal.direction() {
                Direction::Input => VariableKind::Input,
                Direction::Output => VariableKind::Output,
                Direction::Inout => VariableKind::Inout,
                Direction::Internal => VariableKind::Variable,
                _ => VariableKind::Variable,
            };
            (
                source_id(signal.id()),
                SymbolicVariable {
                    path: vec![signal.name().to_string()],
                    kind: variable_kind,
                    signed: signal.value_type().is_signed(),
                    metadata: VariableMetadata {
                        width: signal.value_type().width(),
                        is_4state: signal.value_type().is_four_state(),
                        kind,
                        type_kind,
                        array_dims: Vec::new(),
                    },
                    packed_dims: vec![signal.value_type().width()],
                    source: None,
                    module_affiliated: true,
                },
            )
        })
        .collect();

    let mut arena = SLTNodeArena::new();
    let mut node_cache = HashMap::default();
    let mut comb_blocks = Vec::new();
    for assignment in artifact.assignments() {
        let mut sources = HashSet::default();
        expression_sources(artifact, assignment.value(), &mut sources)?;
        comb_blocks.push(LogicPath {
            target: LogicPathTarget::Var(signal_atom(assignment.target())),
            sources,
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr: lower_slt_expression(artifact, assignment.value(), &mut arena, &mut node_cache)?,
        });
    }

    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();
    lower_registers(
        artifact,
        &mut eval_only_ff_blocks,
        &mut apply_ff_blocks,
        &mut eval_apply_ff_blocks,
        &mut reset_clock_map,
    )?;

    let initial_memory_values = artifact
        .signals()
        .iter()
        .filter_map(|signal| {
            signal.initial().map(|initial| InitialStateValue {
                address: source_id(signal.id()),
                data: InitialStateData::Packed {
                    value: initial.payload().clone(),
                    mask: initial.mask().clone(),
                    written_mask: (num_bigint::BigUint::from(1u8) << signal.value_type().width())
                        - num_bigint::BigUint::from(1u8),
                },
            })
        })
        .collect();

    let module_id = ModuleId(0);
    let sim_module = SimModule {
        name: artifact.module_name().to_string(),
        variables,
        ff_access_summaries: HashMap::default(),
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        glue_blocks: HashMap::default(),
        indexed_instance_names: HashSet::default(),
        comb_blocks,
        comb_observers: Vec::new(),
        runtime_errors: HashMap::default(),
        runtime_event_sites: Vec::new(),
        initial_memory_values,
        comb_boundaries: HashMap::default(),
        arena,
        reset_clock_map,
    };
    let symbolic = SymbolicRtl {
        modules: [(module_id, sim_module.clone())].into_iter().collect(),
        module_names: [(module_id, artifact.module_name().to_string())]
            .into_iter()
            .collect(),
        root_id: module_id,
    };
    let external = ExternalHierarchy {
        modules: [(
            module_id,
            ExternalModule {
                sim_module,
                port_order: artifact
                    .port_order()
                    .iter()
                    .map(|signal| source_id(*signal))
                    .collect(),
                unresolved_instances: Vec::new(),
            },
        )]
        .into_iter()
        .collect(),
        roots: [(artifact.module_name().to_string(), module_id)]
            .into_iter()
            .collect(),
    };
    Ok(LoweredFrontendArtifact { symbolic, external })
}
