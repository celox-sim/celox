//! Projection from the stable frontend SDK into Celox symbolic internals.

use celox_design::{
    BinaryOp, BitAccess, CounterKind, DomainKind, InitialStateData, InitialStateValue, ModuleId,
    NbaSite, PROCESS_DELAY_WIDTH, PROCESS_STATUS_WIDTH, PortTypeKind, ProcessSlots,
    RegionedVarAddrBase, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, VariableMetadata,
    WORKING_REGION, WaitSite,
};
use celox_frontend_sdk::{
    ActiveLevel, Direction, Edge, EventEdge, ExprId, ExprNode, FrontendArtifact, SignalId,
    SignalSlice, Statement, ValueType,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, merge_sir_eus,
};
use celox_slt::{LogicPath, LogicPathTarget, NodeId, SLTNode, SLTNodeArena};
use thiserror::Error;

use crate::process::{PROCESS_RESUME_WIDTH, ProcessKernelBuilder, ProcessKernelError};
use crate::symbolic::artifact::{
    ExternalHierarchy, ExternalModule, SimModule, SymbolicProcess, SymbolicRtl, SymbolicVariable,
};
use crate::symbolic::width::coerce_node_width;
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
    #[error(
        "async reset `{reset}` is shared by distinct clock domains `{first_clock}` and `{second_clock}`"
    )]
    SharedResetAcrossClocks {
        reset: String,
        first_clock: String,
        second_clock: String,
    },
    #[error("frontend SDK expression is invalid: {0}")]
    InvalidExpression(#[from] celox_slt::SLTNodeFactsError),
    #[error(transparent)]
    Process(#[from] ProcessKernelError),
    #[error(
        "frontend artifact `{module}` has processes, which do not run inside a Veryl native testbench"
    )]
    ProcessesInTestbench { module: String },
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

fn signal_slice_type(
    artifact: &FrontendArtifact,
    slice: SignalSlice,
) -> Result<ValueType, FrontendArtifactError> {
    let signal_type = artifact
        .signal(slice.signal())
        .ok_or(FrontendArtifactError::UnknownSignal(slice.signal().index()))?
        .value_type();
    Ok(ValueType::new(
        slice.width(),
        signal_type.is_signed() && slice.width() == signal_type.width(),
        signal_type.is_four_state(),
    )?)
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
    visited: &mut HashSet<ExprId>,
) -> Result<(), FrontendArtifactError> {
    if !visited.insert(id) {
        return Ok(());
    }
    let expression = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?;
    match expression.node() {
        ExprNode::Signal(slice) => {
            sources.insert(signal_atom(*slice));
        }
        ExprNode::Constant(_) => {}
        ExprNode::Binary { lhs, rhs, .. } => {
            expression_sources(artifact, *lhs, sources, visited)?;
            expression_sources(artifact, *rhs, sources, visited)?;
        }
        ExprNode::Unary { input, .. } | ExprNode::Slice { input, .. } => {
            expression_sources(artifact, *input, sources, visited)?;
        }
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expression_sources(artifact, *condition, sources, visited)?;
            expression_sources(artifact, *then_expr, sources, visited)?;
            expression_sources(artifact, *else_expr, sources, visited)?;
        }
        ExprNode::Concat(parts) => {
            for part in parts {
                expression_sources(artifact, *part, sources, visited)?;
            }
        }
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    }
    Ok(())
}

fn coerce_slt_expression(
    artifact: &FrontendArtifact,
    id: ExprId,
    target_width: usize,
    arena: &mut SLTNodeArena<SourceVarId>,
    cache: &mut HashMap<ExprId, NodeId>,
) -> Result<NodeId, FrontendArtifactError> {
    let value_type = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?
        .value_type();
    let node = lower_slt_expression(artifact, id, arena, cache)?;
    Ok(coerce_node_width(
        arena,
        node,
        Some(target_width),
        value_type.is_signed(),
    )?)
}

fn coerce_slt_expression_to_type(
    artifact: &FrontendArtifact,
    id: ExprId,
    target_type: ValueType,
    arena: &mut SLTNodeArena<SourceVarId>,
    cache: &mut HashMap<ExprId, NodeId>,
) -> Result<NodeId, FrontendArtifactError> {
    let value_type = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?
        .value_type();
    let node = lower_slt_expression(artifact, id, arena, cache)?;
    if value_type == target_type {
        Ok(node)
    } else {
        finish_slt_expression(arena, node, target_type)
    }
}

fn finish_slt_expression(
    arena: &mut SLTNodeArena<SourceVarId>,
    node: NodeId,
    value_type: ValueType,
) -> Result<NodeId, FrontendArtifactError> {
    let node = coerce_node_width(
        arena,
        node,
        Some(value_type.width()),
        value_type.is_signed(),
    )?;
    if value_type.is_four_state() {
        Ok(node)
    } else {
        Ok(arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?)
    }
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
            signed: expression.value_type().is_signed(),
            index: Vec::new(),
            access: BitAccess::new(slice.lsb(), slice.lsb() + slice.width() - 1),
        },
        ExprNode::Constant(value) => SLTNode::Constant(
            value.payload().clone(),
            value.mask().clone(),
            value.value_type().width(),
            value.value_type().is_signed(),
        ),
        ExprNode::Binary { op, lhs, rhs } => {
            use celox_frontend_sdk::BinaryOp as SdkBinaryOp;

            let lhs_type = artifact
                .expression(*lhs)
                .ok_or(FrontendArtifactError::UnknownExpression(lhs.index()))?
                .value_type();
            let rhs_type = artifact
                .expression(*rhs)
                .ok_or(FrontendArtifactError::UnknownExpression(rhs.index()))?
                .value_type();
            let (lhs, rhs) = match op {
                SdkBinaryOp::ShiftLeft
                | SdkBinaryOp::ShiftRight
                | SdkBinaryOp::ArithmeticShiftRight => (
                    coerce_slt_expression(
                        artifact,
                        *lhs,
                        expression.value_type().width(),
                        arena,
                        cache,
                    )?,
                    lower_slt_expression(artifact, *rhs, arena, cache)?,
                ),
                SdkBinaryOp::Equal
                | SdkBinaryOp::NotEqual
                | SdkBinaryOp::CaseEqual
                | SdkBinaryOp::CaseNotEqual
                | SdkBinaryOp::LessUnsigned
                | SdkBinaryOp::LessSigned
                | SdkBinaryOp::LessEqualUnsigned
                | SdkBinaryOp::LessEqualSigned
                | SdkBinaryOp::GreaterUnsigned
                | SdkBinaryOp::GreaterSigned
                | SdkBinaryOp::GreaterEqualUnsigned
                | SdkBinaryOp::GreaterEqualSigned => {
                    let operand_width = lhs_type.width().max(rhs_type.width());
                    (
                        coerce_slt_expression(artifact, *lhs, operand_width, arena, cache)?,
                        coerce_slt_expression(artifact, *rhs, operand_width, arena, cache)?,
                    )
                }
                SdkBinaryOp::LogicAnd | SdkBinaryOp::LogicOr => (
                    lower_slt_expression(artifact, *lhs, arena, cache)?,
                    lower_slt_expression(artifact, *rhs, arena, cache)?,
                ),
                _ => (
                    coerce_slt_expression(
                        artifact,
                        *lhs,
                        expression.value_type().width(),
                        arena,
                        cache,
                    )?,
                    coerce_slt_expression(
                        artifact,
                        *rhs,
                        expression.value_type().width(),
                        arena,
                        cache,
                    )?,
                ),
            };
            SLTNode::Binary(lhs, binary_op(*op)?, rhs)
        }
        ExprNode::Unary { op, input } => {
            let input = match op {
                celox_frontend_sdk::UnaryOp::Negate | celox_frontend_sdk::UnaryOp::BitNot => {
                    coerce_slt_expression(
                        artifact,
                        *input,
                        expression.value_type().width(),
                        arena,
                        cache,
                    )?
                }
                _ => lower_slt_expression(artifact, *input, arena, cache)?,
            };
            SLTNode::Unary(unary_op(*op)?, input)
        }
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => SLTNode::Mux {
            cond: lower_slt_expression(artifact, *condition, arena, cache)?,
            then_expr: coerce_slt_expression(
                artifact,
                *then_expr,
                expression.value_type().width(),
                arena,
                cache,
            )?,
            else_expr: coerce_slt_expression(
                artifact,
                *else_expr,
                expression.value_type().width(),
                arena,
                cache,
            )?,
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
    let node = finish_slt_expression(arena, node, expression.value_type())?;
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

fn coerce_sir_register(
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    input: RegisterId,
    input_type: ValueType,
    target_type: ValueType,
) -> Result<RegisterId, FrontendArtifactError> {
    let mut current = input;
    let mut current_type = input_type;

    if current_type.width() > target_type.width() {
        let narrowed_type = ValueType::new(
            target_type.width(),
            current_type.is_signed(),
            current_type.is_four_state(),
        )?;
        let narrowed = alloc_register(builder, narrowed_type);
        builder.emit(SIRInstruction::Slice(
            narrowed,
            current,
            0,
            target_type.width(),
        ));
        current = narrowed;
        current_type = narrowed_type;
    } else if current_type.width() < target_type.width() {
        let extension_width = target_type.width() - current_type.width();
        let extension_type = ValueType::new(extension_width, false, current_type.is_four_state())?;
        let extension = alloc_register(builder, extension_type);
        if current_type.is_signed() {
            let sign_type = ValueType::new(1, false, current_type.is_four_state())?;
            let sign = alloc_register(builder, sign_type);
            builder.emit(SIRInstruction::Slice(
                sign,
                current,
                current_type.width() - 1,
                1,
            ));
            builder.emit(SIRInstruction::Concat(
                extension,
                vec![sign; extension_width],
            ));
        } else {
            builder.emit(SIRInstruction::Imm(extension, SIRValue::new(0u8)));
        }
        let widened_type = ValueType::new(
            target_type.width(),
            current_type.is_signed(),
            current_type.is_four_state(),
        )?;
        let widened = alloc_register(builder, widened_type);
        builder.emit(SIRInstruction::Concat(widened, vec![extension, current]));
        current = widened;
        current_type = widened_type;
    }

    if current_type.is_four_state() && !target_type.is_four_state() {
        let converted = alloc_register(builder, target_type);
        builder.emit(SIRInstruction::Unary(
            converted,
            UnaryOp::ToTwoState,
            current,
        ));
        return Ok(converted);
    }

    if current_type.is_four_state() != target_type.is_four_state()
        || (!target_type.is_four_state() && current_type.is_signed() != target_type.is_signed())
    {
        let converted = alloc_register(builder, target_type);
        builder.emit(SIRInstruction::Unary(converted, UnaryOp::Ident, current));
        return Ok(converted);
    }

    Ok(current)
}

fn coerce_sir_expression(
    artifact: &FrontendArtifact,
    id: ExprId,
    target_type: ValueType,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    cache: &mut HashMap<ExprId, RegisterId>,
) -> Result<RegisterId, FrontendArtifactError> {
    let input_type = artifact
        .expression(id)
        .ok_or(FrontendArtifactError::UnknownExpression(id.index()))?
        .value_type();
    let input = lower_sir_expression(artifact, id, builder, cache)?;
    coerce_sir_register(builder, input, input_type, target_type)
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
    let result = match expression.node() {
        ExprNode::Signal(slice) => {
            let result = alloc_register(builder, expression.value_type());
            builder.emit(SIRInstruction::Load(
                result,
                RegionedSourceAddr {
                    region: STABLE_REGION,
                    var_id: source_id(slice.signal()),
                },
                SIROffset::Static(slice.lsb()),
                slice.width(),
            ));
            result
        }
        ExprNode::Constant(value) => {
            let result = alloc_register(builder, expression.value_type());
            builder.emit(SIRInstruction::Imm(
                result,
                SIRValue::new_four_state(value.payload().clone(), value.mask().clone()),
            ));
            result
        }
        ExprNode::Binary { op, lhs, rhs } => {
            use celox_frontend_sdk::BinaryOp as SdkBinaryOp;

            let lhs_type = artifact
                .expression(*lhs)
                .ok_or(FrontendArtifactError::UnknownExpression(lhs.index()))?
                .value_type();
            let rhs_type = artifact
                .expression(*rhs)
                .ok_or(FrontendArtifactError::UnknownExpression(rhs.index()))?
                .value_type();
            let (lhs, rhs) = match op {
                SdkBinaryOp::ShiftLeft
                | SdkBinaryOp::ShiftRight
                | SdkBinaryOp::ArithmeticShiftRight => (
                    coerce_sir_expression(
                        artifact,
                        *lhs,
                        ValueType::new(
                            expression.value_type().width(),
                            lhs_type.is_signed(),
                            lhs_type.is_four_state(),
                        )?,
                        builder,
                        cache,
                    )?,
                    lower_sir_expression(artifact, *rhs, builder, cache)?,
                ),
                SdkBinaryOp::Equal
                | SdkBinaryOp::NotEqual
                | SdkBinaryOp::CaseEqual
                | SdkBinaryOp::CaseNotEqual
                | SdkBinaryOp::LessUnsigned
                | SdkBinaryOp::LessSigned
                | SdkBinaryOp::LessEqualUnsigned
                | SdkBinaryOp::LessEqualSigned
                | SdkBinaryOp::GreaterUnsigned
                | SdkBinaryOp::GreaterSigned
                | SdkBinaryOp::GreaterEqualUnsigned
                | SdkBinaryOp::GreaterEqualSigned => {
                    let width = lhs_type.width().max(rhs_type.width());
                    (
                        coerce_sir_expression(
                            artifact,
                            *lhs,
                            ValueType::new(width, lhs_type.is_signed(), lhs_type.is_four_state())?,
                            builder,
                            cache,
                        )?,
                        coerce_sir_expression(
                            artifact,
                            *rhs,
                            ValueType::new(width, rhs_type.is_signed(), rhs_type.is_four_state())?,
                            builder,
                            cache,
                        )?,
                    )
                }
                SdkBinaryOp::LogicAnd | SdkBinaryOp::LogicOr => (
                    lower_sir_expression(artifact, *lhs, builder, cache)?,
                    lower_sir_expression(artifact, *rhs, builder, cache)?,
                ),
                _ => (
                    coerce_sir_expression(
                        artifact,
                        *lhs,
                        ValueType::new(
                            expression.value_type().width(),
                            lhs_type.is_signed(),
                            lhs_type.is_four_state(),
                        )?,
                        builder,
                        cache,
                    )?,
                    coerce_sir_expression(
                        artifact,
                        *rhs,
                        ValueType::new(
                            expression.value_type().width(),
                            rhs_type.is_signed(),
                            rhs_type.is_four_state(),
                        )?,
                        builder,
                        cache,
                    )?,
                ),
            };
            let is_boolean = matches!(
                op,
                SdkBinaryOp::Equal
                    | SdkBinaryOp::NotEqual
                    | SdkBinaryOp::CaseEqual
                    | SdkBinaryOp::CaseNotEqual
                    | SdkBinaryOp::LessUnsigned
                    | SdkBinaryOp::LessSigned
                    | SdkBinaryOp::LessEqualUnsigned
                    | SdkBinaryOp::LessEqualSigned
                    | SdkBinaryOp::GreaterUnsigned
                    | SdkBinaryOp::GreaterSigned
                    | SdkBinaryOp::GreaterEqualUnsigned
                    | SdkBinaryOp::GreaterEqualSigned
                    | SdkBinaryOp::LogicAnd
                    | SdkBinaryOp::LogicOr
            );
            let case_equality = matches!(op, SdkBinaryOp::CaseEqual | SdkBinaryOp::CaseNotEqual);
            let operation_four_state = expression.value_type().is_four_state()
                || (!case_equality && (lhs_type.is_four_state() || rhs_type.is_four_state()));
            let operation_type = ValueType::new(
                if is_boolean {
                    1
                } else {
                    expression.value_type().width()
                },
                !is_boolean && expression.value_type().is_signed(),
                operation_four_state,
            )?;
            let operation_result = alloc_register(builder, operation_type);
            builder.emit(SIRInstruction::Binary(
                operation_result,
                lhs,
                binary_op(*op)?,
                rhs,
            ));
            coerce_sir_register(
                builder,
                operation_result,
                operation_type,
                expression.value_type(),
            )?
        }
        ExprNode::Unary { op, input } => {
            use celox_frontend_sdk::UnaryOp as SdkUnaryOp;

            let input_type = artifact
                .expression(*input)
                .ok_or(FrontendArtifactError::UnknownExpression(input.index()))?
                .value_type();
            let (input, operation_type) = match op {
                SdkUnaryOp::Negate | SdkUnaryOp::BitNot => (
                    coerce_sir_expression(
                        artifact,
                        *input,
                        ValueType::new(
                            expression.value_type().width(),
                            input_type.is_signed(),
                            input_type.is_four_state(),
                        )?,
                        builder,
                        cache,
                    )?,
                    ValueType::new(
                        expression.value_type().width(),
                        expression.value_type().is_signed(),
                        expression.value_type().is_four_state() || input_type.is_four_state(),
                    )?,
                ),
                SdkUnaryOp::LogicNot
                | SdkUnaryOp::ReduceAnd
                | SdkUnaryOp::ReduceOr
                | SdkUnaryOp::ReduceXor => (
                    lower_sir_expression(artifact, *input, builder, cache)?,
                    ValueType::new(
                        1,
                        false,
                        expression.value_type().is_four_state() || input_type.is_four_state(),
                    )?,
                ),
                SdkUnaryOp::ToTwoState => (
                    lower_sir_expression(artifact, *input, builder, cache)?,
                    ValueType::new(input_type.width(), input_type.is_signed(), false)?,
                ),
                SdkUnaryOp::PopCount
                | SdkUnaryOp::CountLeadingZeros
                | SdkUnaryOp::CountTrailingZeros => (
                    lower_sir_expression(artifact, *input, builder, cache)?,
                    ValueType::new(
                        unary_op(*op)?.result_width(input_type.width()),
                        false,
                        expression.value_type().is_four_state() || input_type.is_four_state(),
                    )?,
                ),
                _ => return Err(FrontendArtifactError::UnsupportedOperation),
            };
            let operation_result = alloc_register(builder, operation_type);
            builder.emit(SIRInstruction::Unary(
                operation_result,
                unary_op(*op)?,
                input,
            ));
            coerce_sir_register(
                builder,
                operation_result,
                operation_type,
                expression.value_type(),
            )?
        }
        ExprNode::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition_type = artifact
                .expression(*condition)
                .ok_or(FrontendArtifactError::UnknownExpression(condition.index()))?
                .value_type();
            let condition = lower_sir_expression(artifact, *condition, builder, cache)?;
            let then_type = artifact
                .expression(*then_expr)
                .ok_or(FrontendArtifactError::UnknownExpression(then_expr.index()))?
                .value_type();
            let else_type = artifact
                .expression(*else_expr)
                .ok_or(FrontendArtifactError::UnknownExpression(else_expr.index()))?
                .value_type();
            let then_expr = coerce_sir_expression(
                artifact,
                *then_expr,
                ValueType::new(
                    expression.value_type().width(),
                    then_type.is_signed(),
                    then_type.is_four_state(),
                )?,
                builder,
                cache,
            )?;
            let else_expr = coerce_sir_expression(
                artifact,
                *else_expr,
                ValueType::new(
                    expression.value_type().width(),
                    else_type.is_signed(),
                    else_type.is_four_state(),
                )?,
                builder,
                cache,
            )?;
            let operation_type = ValueType::new(
                expression.value_type().width(),
                expression.value_type().is_signed(),
                expression.value_type().is_four_state()
                    || condition_type.is_four_state()
                    || then_type.is_four_state()
                    || else_type.is_four_state(),
            )?;
            let operation_result = alloc_register(builder, operation_type);
            builder.emit(SIRInstruction::Mux(
                operation_result,
                condition,
                then_expr,
                else_expr,
            ));
            coerce_sir_register(
                builder,
                operation_result,
                operation_type,
                expression.value_type(),
            )?
        }
        ExprNode::Concat(parts) => {
            let parts = parts
                .iter()
                .map(|part| lower_sir_expression(artifact, *part, builder, cache))
                .collect::<Result<Vec<_>, _>>()?;
            let result = alloc_register(builder, expression.value_type());
            builder.emit(SIRInstruction::Concat(result, parts));
            result
        }
        ExprNode::Slice { input, lsb } => {
            let input = lower_sir_expression(artifact, *input, builder, cache)?;
            let result = alloc_register(builder, expression.value_type());
            builder.emit(SIRInstruction::Slice(
                result,
                input,
                *lsb,
                expression.value_type().width(),
            ));
            result
        }
        _ => return Err(FrontendArtifactError::UnsupportedOperation),
    };
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
    let polarized = if active == ActiveLevel::Low {
        let inverted = alloc_register(
            builder,
            ValueType::new(1, false, signal_info.value_type().is_four_state())?,
        );
        builder.emit(SIRInstruction::Unary(inverted, UnaryOp::LogicNot, loaded));
        inverted
    } else {
        loaded
    };
    if signal_info.value_type().is_four_state() {
        let result = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(
            result,
            UnaryOp::ToTwoState,
            polarized,
        ));
        Ok(result)
    } else {
        Ok(polarized)
    }
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
            let reset_id = source_id(reset.signal());
            let clock_id = source_id(register.clock());
            if let Some(first_clock_id) = reset_clock_map.get(&reset_id)
                && *first_clock_id != clock_id
            {
                let signal_name = |id: SignalId| {
                    artifact
                        .signal(id)
                        .map(|signal| signal.name().to_string())
                        .ok_or(FrontendArtifactError::UnknownSignal(id.index()))
                };
                let first_clock = artifact
                    .signals()
                    .get(first_clock_id.0 as usize)
                    .ok_or(FrontendArtifactError::UnknownSignal(first_clock_id.0))?;
                return Err(FrontendArtifactError::SharedResetAcrossClocks {
                    reset: signal_name(reset.signal())?,
                    first_clock: first_clock.name().to_string(),
                    second_clock: signal_name(register.clock())?,
                });
            }
            reset_clock_map.insert(reset_id, clock_id);
        }

        let build_eval =
            |commit: bool| -> Result<ExecutionUnit<RegionedSourceAddr>, FrontendArtifactError> {
                let mut builder = SIRBuilder::new();
                let target_info = artifact.signal(target.signal()).ok_or(
                    FrontendArtifactError::UnknownSignal(target.signal().index()),
                )?;
                let target_type = target_info.value_type();
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
                let mut next = coerce_sir_expression(
                    artifact,
                    register.next(),
                    target_type,
                    &mut builder,
                    &mut cache,
                )?;
                if let Some(enable) = register.enable() {
                    let condition =
                        lower_control(artifact, enable.signal(), enable.active(), &mut builder)?;
                    let current = alloc_register(&mut builder, target_type);
                    builder.emit(SIRInstruction::Load(
                        current,
                        RegionedSourceAddr {
                            region: STABLE_REGION,
                            var_id: target_id,
                        },
                        SIROffset::Static(0),
                        target.width(),
                    ));
                    let selected = alloc_register(&mut builder, target_type);
                    builder.emit(SIRInstruction::Mux(selected, condition, next, current));
                    next = selected;
                }
                if let Some(reset) = register.async_reset() {
                    let condition =
                        lower_control(artifact, reset.signal(), reset.active(), &mut builder)?;
                    let reset_value = coerce_sir_expression(
                        artifact,
                        reset.value(),
                        target_type,
                        &mut builder,
                        &mut cache,
                    )?;
                    let selected = alloc_register(&mut builder, target_type);
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

/// Hidden state objects the lowering declares after the artifact's signals:
/// process control slots, counters, staging and gate objects. None of them
/// is a signal of the design.
struct HiddenObjects<'a> {
    next_id: u32,
    variables: &'a mut HashMap<SourceVarId, SymbolicVariable>,
}

impl HiddenObjects<'_> {
    fn declare(&mut self, path: Vec<String>, width: usize, four_state: bool) -> SourceVarId {
        let id = SourceVarId(self.next_id);
        self.next_id += 1;
        self.variables.insert(
            id,
            SymbolicVariable {
                path,
                kind: VariableKind::Variable,
                signed: false,
                metadata: VariableMetadata {
                    width,
                    is_4state: four_state,
                    kind: DomainKind::Other,
                    type_kind: if four_state {
                        PortTypeKind::Logic
                    } else {
                        PortTypeKind::Bit
                    },
                    array_dims: Vec::new(),
                    value_kind: Default::default(),
                },
                unpacked_ranges: Vec::new(),
                packed_dims: vec![width],
                source: None,
                module_affiliated: false,
            },
        );
        id
    }
}

/// Timing metadata collected while lowering the processes of an artifact.
struct ProcessLowering<'a, 'h> {
    artifact: &'a FrontendArtifact,
    hidden: &'a mut HiddenObjects<'h>,
    wait_sites: Vec<WaitSite<SourceVarId>>,
    nba_sites: Vec<NbaSite<SourceVarId>>,
    /// Signals that exactly one nonblocking statement of the artifact
    /// assigns. An artifact is one flat module whose processes never run
    /// inside another design, so nothing outside it writes its signals and
    /// these sites stage a single pending value.
    single_nba_targets: HashSet<SignalId>,
    process: usize,
    process_wait_sites: u32,
    process_nba_sites: u32,
    /// Loops given an iteration counter so far.
    counted_loops: usize,
}

fn collect_nonblocking_targets(statements: &[Statement], targets: &mut Vec<SignalId>) {
    for statement in statements {
        match statement {
            Statement::Nonblocking { target, .. } => targets.push(target.signal()),
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_nonblocking_targets(then_body, targets);
                collect_nonblocking_targets(else_body, targets);
            }
            Statement::While { body, .. } | Statement::Forever { body } => {
                collect_nonblocking_targets(body, targets);
            }
            _ => {}
        }
    }
}

/// A one-bit two-state register that holds when `condition` is nonzero: some
/// bit is a known one.
fn truth(
    artifact: &FrontendArtifact,
    condition: ExprId,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
) -> Result<RegisterId, FrontendArtifactError> {
    let value_type = artifact
        .expression(condition)
        .ok_or(FrontendArtifactError::UnknownExpression(condition.index()))?
        .value_type();
    let value = lower_sir_expression(artifact, condition, builder, &mut HashMap::default())?;
    let any = alloc_register(
        builder,
        ValueType::new(1, false, value_type.is_four_state())?,
    );
    builder.emit(SIRInstruction::Unary(any, UnaryOp::Or, value));
    if !value_type.is_four_state() {
        return Ok(any);
    }
    let known = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, any));
    Ok(known)
}

fn store_assignment(
    artifact: &FrontendArtifact,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    target: SignalSlice,
    value: ExprId,
    cache: &mut HashMap<ExprId, RegisterId>,
) -> Result<(), FrontendArtifactError> {
    let target_type = signal_slice_type(artifact, target)?;
    let value = coerce_sir_expression(artifact, value, target_type, builder, cache)?;
    builder.emit(SIRInstruction::Store(
        RegionedSourceAddr {
            region: STABLE_REGION,
            var_id: source_id(target.signal()),
        },
        SIROffset::Static(target.lsb()),
        target.width(),
        value,
        Vec::new(),
        Vec::new(),
    ));
    Ok(())
}

/// Lower `statements` at the kernel's open block. Returns whether control
/// can continue after them; if not, no block is left open.
/// Whether `statements` always suspend the process: a loop around them
/// returns to the runtime in each iteration. A level wait whose condition
/// holds does not suspend.
fn always_suspends(statements: &[Statement]) -> bool {
    statements.iter().any(|statement| {
        matches!(
            statement,
            Statement::Delay { .. } | Statement::InactiveDelay { .. } | Statement::WaitEvent { .. }
        )
    })
}

/// Iterations, as a power of two, between the yields of a loop that may not
/// suspend. A finite loop pays one counter update per iteration, and only a
/// loop of about 2^40 iterations in one time slot exhausts the runtime's
/// return budget.
const LOOP_YIELD_BITS: usize = 16;

/// Close a loop iteration that may not suspend: count it, yield every
/// 2^[`LOOP_YIELD_BITS`] iterations, and continue at `header`. The count
/// lives in a hidden object, so it survives the yield.
fn yield_now_and_then(
    lowering: &mut ProcessLowering<'_, '_>,
    kernel: &mut ProcessKernelBuilder,
    header: BlockId,
) -> Result<(), FrontendArtifactError> {
    let counter = stable(lowering.hidden.declare(
        vec![
            format!("$loop[{}]", lowering.counted_loops),
            "iterations".to_string(),
        ],
        LOOP_YIELD_BITS,
        false,
    ));
    lowering.counted_loops += 1;
    let builder = kernel.builder();
    let count = builder.alloc_bit(LOOP_YIELD_BITS, false);
    builder.emit(SIRInstruction::Load(
        count,
        counter,
        SIROffset::Static(0),
        LOOP_YIELD_BITS,
    ));
    let one = builder.alloc_bit(LOOP_YIELD_BITS, false);
    builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let next = builder.alloc_bit(LOOP_YIELD_BITS, false);
    builder.emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
    builder.emit(SIRInstruction::Store(
        counter,
        SIROffset::Static(0),
        LOOP_YIELD_BITS,
        next,
        Vec::new(),
        Vec::new(),
    ));
    let zero = builder.alloc_bit(LOOP_YIELD_BITS, false);
    builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
    let wrapped = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(wrapped, next, BinaryOp::Eq, zero));
    let yield_block = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: wrapped,
        true_block: (yield_block, Vec::new()),
        false_block: (header, Vec::new()),
    });
    builder.switch_to_block(yield_block);
    kernel.yield_now()?;
    kernel
        .builder()
        .seal_block(SIRTerminator::Jump(header, Vec::new()));
    Ok(())
}

/// Lower `statements` into `kernel`. A loop whose body may not suspend
/// yields every 2^[`LOOP_YIELD_BITS`] iterations, so that the runtime
/// charges it against the time slot's budget instead of spinning in the
/// kernel.
fn lower_statements(
    lowering: &mut ProcessLowering<'_, '_>,
    kernel: &mut ProcessKernelBuilder,
    statements: &[Statement],
) -> Result<bool, FrontendArtifactError> {
    let artifact = lowering.artifact;
    for statement in statements {
        // Registers do not live across statements: a suspension in between
        // returns to the runtime, and each read must see the current state.
        let mut cache = HashMap::default();
        match statement {
            Statement::Assign { target, value } => {
                store_assignment(artifact, kernel.builder(), *target, *value, &mut cache)?;
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                let builder = kernel.builder();
                let condition = lower_sir_expression(artifact, *condition, builder, &mut cache)?;
                let then_block = builder.new_block();
                let else_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (then_block, Vec::new()),
                    false_block: (else_block, Vec::new()),
                });
                let mut join = None;
                for (block, body) in [(then_block, then_body), (else_block, else_body)] {
                    kernel.builder().switch_to_block(block);
                    if lower_statements(lowering, kernel, body)? {
                        let builder = kernel.builder();
                        let target = *join.get_or_insert_with(|| builder.new_block());
                        builder.seal_block(SIRTerminator::Jump(target, Vec::new()));
                    }
                }
                match join {
                    Some(join) => kernel.builder().switch_to_block(join),
                    None => return Ok(false),
                }
            }
            Statement::While { condition, body } => {
                let builder = kernel.builder();
                let header = builder.new_block();
                builder.seal_block(SIRTerminator::Jump(header, Vec::new()));
                builder.switch_to_block(header);
                let condition = lower_sir_expression(artifact, *condition, builder, &mut cache)?;
                let body_block = builder.new_block();
                let exit = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (body_block, Vec::new()),
                    false_block: (exit, Vec::new()),
                });
                builder.switch_to_block(body_block);
                if lower_statements(lowering, kernel, body)? {
                    if always_suspends(body) {
                        kernel
                            .builder()
                            .seal_block(SIRTerminator::Jump(header, Vec::new()));
                    } else {
                        yield_now_and_then(lowering, kernel, header)?;
                    }
                }
                kernel.builder().switch_to_block(exit);
            }
            Statement::Forever { body } => {
                let builder = kernel.builder();
                let header = builder.new_block();
                builder.seal_block(SIRTerminator::Jump(header, Vec::new()));
                builder.switch_to_block(header);
                if lower_statements(lowering, kernel, body)? {
                    if always_suspends(body) {
                        kernel
                            .builder()
                            .seal_block(SIRTerminator::Jump(header, Vec::new()));
                    } else {
                        yield_now_and_then(lowering, kernel, header)?;
                    }
                }
                return Ok(false);
            }
            Statement::Delay { amount } | Statement::InactiveDelay { amount } => {
                let amount = coerce_sir_expression(
                    artifact,
                    *amount,
                    ValueType::bits(PROCESS_DELAY_WIDTH)?,
                    kernel.builder(),
                    &mut cache,
                )?;
                if matches!(statement, Statement::Delay { .. }) {
                    kernel.delay(amount)?;
                } else {
                    kernel.inactive_delay(amount)?;
                }
            }
            Statement::WaitEvent { terms } => {
                let index = lowering.process_wait_sites;
                lowering.process_wait_sites += 1;
                lowering.wait_sites.push(WaitSite {
                    process: lowering.process,
                    terms: terms
                        .iter()
                        .map(|term| {
                            let kind = match term.edge {
                                EventEdge::Change => CounterKind::Changes,
                                EventEdge::Posedge => CounterKind::Rises,
                                EventEdge::Negedge => CounterKind::Falls,
                            };
                            (source_id(term.signal), kind)
                        })
                        .collect(),
                });
                // The runtime resumes the process at the first occurrence
                // after the wait began; no condition is left to check.
                kernel.suspend_wait(index)?;
                let woken = kernel.builder().alloc_bit(1, false);
                kernel
                    .builder()
                    .emit(SIRInstruction::Imm(woken, SIRValue::new(1u8)));
                kernel.wake_if(woken);
            }
            Statement::WaitLevel { condition } => {
                let index = lowering.process_wait_sites;
                lowering.process_wait_sites += 1;
                let mut sources = HashSet::default();
                expression_sources(artifact, *condition, &mut sources, &mut HashSet::default())?;
                let mut signals = sources.into_iter().map(|atom| atom.id).collect::<Vec<_>>();
                signals.sort_unstable();
                signals.dedup();
                lowering.wait_sites.push(WaitSite {
                    process: lowering.process,
                    terms: signals
                        .into_iter()
                        .map(|signal| (signal, CounterKind::Changes))
                        .collect(),
                });
                let mut error = None;
                kernel.wait_level(index, |builder| {
                    truth(artifact, *condition, builder).unwrap_or_else(|e| {
                        error = Some(e);
                        let never = builder.alloc_bit(1, false);
                        builder.emit(SIRInstruction::Imm(never, SIRValue::new(0u8)));
                        never
                    })
                })?;
                if let Some(error) = error {
                    return Err(error);
                }
            }
            Statement::Nonblocking { target, value } => {
                let index = lowering.process_nba_sites;
                lowering.process_nba_sites += 1;
                let target_type = signal_slice_type(artifact, *target)?;
                let site_index = lowering.nba_sites.len();
                let name = |slot: &str| vec![format!("$nba[{site_index}]"), slot.to_string()];
                let staged = lowering.hidden.declare(
                    name("value"),
                    target.width(),
                    target_type.is_four_state(),
                );
                let pending = lowering
                    .single_nba_targets
                    .contains(&target.signal())
                    .then(|| lowering.hidden.declare(name("pending"), 1, false));
                let site = NbaSite {
                    process: lowering.process,
                    target: source_id(target.signal()),
                    lsb: target.lsb(),
                    width: target.width(),
                    value: staged,
                    offset: None,
                    pending,
                };
                kernel.nonblocking(index, &site, |builder| {
                    let value = coerce_sir_expression(
                        artifact,
                        *value,
                        target_type,
                        builder,
                        &mut HashMap::default(),
                    )?;
                    Ok::<_, FrontendArtifactError>((value, None))
                })?;
                lowering.nba_sites.push(site);
            }
            Statement::Finish => {
                kernel.finish();
                return Ok(false);
            }
            _ => return Err(FrontendArtifactError::UnsupportedOperation),
        }
    }
    Ok(true)
}

/// Lower the statements of a combinational process: assignments and
/// conditionals, writing stable state as they execute.
fn lower_comb_statements(
    artifact: &FrontendArtifact,
    builder: &mut SIRBuilder<RegionedSourceAddr>,
    statements: &[Statement],
) -> Result<(), FrontendArtifactError> {
    for statement in statements {
        let mut cache = HashMap::default();
        match statement {
            Statement::Assign { target, value } => {
                store_assignment(artifact, builder, *target, *value, &mut cache)?;
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                let condition = lower_sir_expression(artifact, *condition, builder, &mut cache)?;
                let then_block = builder.new_block();
                let else_block = builder.new_block();
                let join = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (then_block, Vec::new()),
                    false_block: (else_block, Vec::new()),
                });
                for (block, body) in [(then_block, then_body), (else_block, else_body)] {
                    builder.switch_to_block(block);
                    lower_comb_statements(artifact, builder, body)?;
                    builder.seal_block(SIRTerminator::Jump(join, Vec::new()));
                }
                builder.switch_to_block(join);
            }
            _ => return Err(FrontendArtifactError::UnsupportedOperation),
        }
    }
    Ok(())
}

/// The bits of `atom` outside every range of `written`.
fn unwritten_bits(
    atom: VarAtomBase<SourceVarId>,
    written: &[VarAtomBase<SourceVarId>],
) -> Vec<VarAtomBase<SourceVarId>> {
    let mut ranges = vec![(atom.access.lsb, atom.access.msb)];
    for written in written.iter().filter(|written| written.id == atom.id) {
        ranges = ranges
            .into_iter()
            .flat_map(|(lsb, msb)| {
                if written.access.msb < lsb || written.access.lsb > msb {
                    return vec![(lsb, msb)];
                }
                let mut kept = Vec::new();
                if lsb < written.access.lsb {
                    kept.push((lsb, written.access.lsb - 1));
                }
                if written.access.msb < msb {
                    kept.push((written.access.msb + 1, msb));
                }
                kept
            })
            .collect();
    }
    ranges
        .into_iter()
        .map(|(lsb, msb)| VarAtomBase::new(atom.id, lsb, msb))
        .collect()
}

fn statement_sources(
    artifact: &FrontendArtifact,
    statements: &[Statement],
    sources: &mut HashSet<VarAtomBase<SourceVarId>>,
) -> Result<(), FrontendArtifactError> {
    for statement in statements {
        let mut visited = HashSet::default();
        match statement {
            Statement::Assign { value, .. } => {
                expression_sources(artifact, *value, sources, &mut visited)?
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                expression_sources(artifact, *condition, sources, &mut visited)?;
                statement_sources(artifact, then_body, sources)?;
                statement_sources(artifact, else_body, sources)?;
            }
            _ => return Err(FrontendArtifactError::UnsupportedOperation),
        }
    }
    Ok(())
}

fn stable(var_id: SourceVarId) -> RegionedSourceAddr {
    RegionedSourceAddr {
        region: STABLE_REGION,
        var_id,
    }
}

fn stable_signal(address: &RegionedSourceAddr, signals: usize) -> Option<SourceVarId> {
    (address.region == STABLE_REGION && (address.var_id.0 as usize) < signals)
        .then_some(address.var_id)
}

/// Lower each combinational process to one unit that runs only when it has
/// not run yet or bits it reads and does not write changed (IEEE 1800-2023
/// 9.2.2.2, 9.2.2.2.1), and to a path for each slice it assigns.
fn lower_comb_processes(
    artifact: &FrontendArtifact,
    hidden: &mut HiddenObjects<'_>,
    arena: &mut SLTNodeArena<SourceVarId>,
    candidates: &mut Vec<SourceVarId>,
) -> Result<Vec<LogicPath<SourceVarId>>, FrontendArtifactError> {
    let mut paths = Vec::new();
    for (index, process) in artifact.comb_processes().iter().enumerate() {
        let mut builder = SIRBuilder::new();
        lower_comb_statements(artifact, &mut builder, process.body())?;
        let body = seal_builder(builder);
        candidates.extend(crate::watch::multi_store_candidates(&body, |address| {
            stable_signal(address, artifact.signals().len())
        }));

        let mut targets = Vec::new();
        for block in body.blocks.values() {
            for instruction in &block.instructions {
                if let SIRInstruction::Store(address, SIROffset::Static(lsb), width, ..) =
                    instruction
                {
                    let atom = VarAtomBase::new(address.var_id, *lsb, lsb + width - 1);
                    if !targets.contains(&atom) {
                        targets.push(atom);
                    }
                }
            }
        }
        targets.sort_unstable_by_key(|atom| (atom.id, atom.access.lsb, atom.access.msb));
        // One path per range the process drives, however often it writes it.
        let mut merged: Vec<VarAtomBase<SourceVarId>> = Vec::new();
        for target in targets {
            match merged.last_mut() {
                Some(last) if last.id == target.id && target.access.lsb <= last.access.msb + 1 => {
                    last.access.msb = last.access.msb.max(target.access.msb);
                }
                _ => merged.push(target),
            }
        }
        let targets = merged;
        // A process is not sensitive to what it writes (IEEE 1800-2023
        // 9.2.2.2.1): its inputs are the bits it reads outside its targets.
        let mut reads = HashSet::default();
        statement_sources(artifact, process.body(), &mut reads)?;
        let mut sources: Vec<VarAtomBase<SourceVarId>> = reads
            .into_iter()
            .flat_map(|atom| unwritten_bits(atom, &targets))
            .collect();
        sources.sort_unstable_by_key(|atom| (atom.id, atom.access.lsb, atom.access.msb));
        let mut merged: Vec<VarAtomBase<SourceVarId>> = Vec::new();
        for source in sources {
            match merged.last_mut() {
                Some(last) if last.id == source.id && source.access.lsb <= last.access.msb + 1 => {
                    last.access.msb = last.access.msb.max(source.access.msb);
                }
                _ => merged.push(source),
            }
        }
        let sources = merged;
        let name = |slot: String| vec![format!("$comb[{index}]"), slot];
        let gate_inputs = sources
            .iter()
            .map(|atom| {
                let value_type = artifact
                    .signals()
                    .get(atom.id.0 as usize)
                    .ok_or(FrontendArtifactError::UnknownSignal(atom.id.0))?
                    .value_type();
                let width = atom.access.msb - atom.access.lsb + 1;
                Ok(crate::watch::GateInput {
                    input: stable(atom.id),
                    lsb: atom.access.lsb,
                    shadow: stable(hidden.declare(
                        name(format!(
                            "input[{}][{}:{}]",
                            atom.id.0, atom.access.msb, atom.access.lsb
                        )),
                        width,
                        value_type.is_four_state(),
                    )),
                    width,
                    four_state: value_type.is_four_state(),
                })
            })
            .collect::<Result<Vec<_>, FrontendArtifactError>>()?;
        let ran = hidden.declare(name("ran".to_string()), 1, false);
        let unit = crate::watch::gate_on_input_change(&body, &gate_inputs, &stable(ran))
            .into_map_addr(&mut |address| address.var_id);
        let sources: HashSet<_> = sources.into_iter().collect();
        for target in targets {
            let expr = arena.alloc(SLTNode::Input {
                variable: target.id,
                signed: false,
                index: Vec::new(),
                access: target.access,
            })?;
            paths.push(LogicPath {
                transparent_copy: false,
                target: LogicPathTarget::Process {
                    process: ran,
                    after_initials: true,
                    sensitivity_counters: Vec::new(),
                    startup: None,
                    target,
                    unit: Box::new(unit.clone()),
                },
                sources: sources.clone(),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }
    }
    Ok(paths)
}

/// Lowered processes of an artifact and their timing metadata.
struct LoweredProcesses {
    processes: Vec<SymbolicProcess>,
    wait_sites: Vec<WaitSite<SourceVarId>>,
    nba_sites: Vec<NbaSite<SourceVarId>>,
}

/// Declare the control slots of every process after the artifact's signals
/// and lower each process body to a kernel.
fn lower_processes(
    artifact: &FrontendArtifact,
    hidden: &mut HiddenObjects<'_>,
    candidates: &mut Vec<SourceVarId>,
) -> Result<LoweredProcesses, FrontendArtifactError> {
    let mut nonblocking_targets = Vec::new();
    for process in artifact.processes() {
        collect_nonblocking_targets(process.body(), &mut nonblocking_targets);
    }
    let mut counts: HashMap<SignalId, usize> = HashMap::default();
    for target in nonblocking_targets {
        *counts.entry(target).or_default() += 1;
    }
    let mut lowering = ProcessLowering {
        artifact,
        hidden,
        wait_sites: Vec::new(),
        nba_sites: Vec::new(),
        // A port may have writers outside the module (`NbaSite::pending`).
        single_nba_targets: counts
            .into_iter()
            .filter(|&(target, count)| {
                count == 1
                    && artifact
                        .signal(target)
                        .is_some_and(|signal| signal.direction() == Direction::Internal)
            })
            .map(|(target, _)| target)
            .collect(),
        process: 0,
        process_wait_sites: 0,
        process_nba_sites: 0,
        counted_loops: 0,
    };
    let mut processes = Vec::new();
    for (index, process) in artifact.processes().iter().enumerate() {
        let name = |slot: &str| vec![format!("$process[{index}]"), slot.to_string()];
        let slots = ProcessSlots {
            is_initializer: false,
            requires_timed_simulation: false,
            resume: lowering
                .hidden
                .declare(name("resume"), PROCESS_RESUME_WIDTH, false),
            status: lowering
                .hidden
                .declare(name("status"), PROCESS_STATUS_WIDTH, false),
            delay: lowering
                .hidden
                .declare(name("delay"), PROCESS_DELAY_WIDTH, false),
        };
        lowering.process = index;
        lowering.process_wait_sites = 0;
        lowering.process_nba_sites = 0;
        let mut kernel = ProcessKernelBuilder::new(slots);
        lower_statements(&mut lowering, &mut kernel, process.body())?;
        let kernel = kernel.build();
        candidates.extend(crate::watch::multi_store_candidates(&kernel, |address| {
            stable_signal(address, artifact.signals().len())
        }));
        processes.push(SymbolicProcess {
            kernel,
            slots,
            origin: celox_design::ProcessOrigin {
                construct: "process".to_string(),
                location: None,
                scope: None,
            },
        });
    }
    Ok(LoweredProcesses {
        processes,
        wait_sites: lowering.wait_sites,
        nba_sites: lowering.nba_sites,
    })
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

    let mut variables: HashMap<SourceVarId, SymbolicVariable> = artifact
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
                        value_kind: Default::default(),
                    },
                    unpacked_ranges: Vec::new(),
                    packed_dims: vec![signal.value_type().width()],
                    source: None,
                    module_affiliated: true,
                },
            )
        })
        .collect();

    let mut hidden = HiddenObjects {
        next_id: artifact.signals().len() as u32,
        variables: &mut variables,
    };
    let mut candidates = Vec::new();
    let LoweredProcesses {
        processes,
        wait_sites,
        nba_sites,
    } = lower_processes(artifact, &mut hidden, &mut candidates)?;
    let mut arena = SLTNodeArena::new();
    let comb_process_paths =
        lower_comb_processes(artifact, &mut hidden, &mut arena, &mut candidates)?;
    let counter_slots = crate::watch::declare_counter_slots(hidden.variables, candidates);

    let mut node_cache = HashMap::default();
    let mut comb_blocks = Vec::new();
    for assignment in artifact.assignments() {
        let target_type = signal_slice_type(artifact, assignment.target())?;
        let mut sources = HashSet::default();
        let mut visited = HashSet::default();
        expression_sources(artifact, assignment.value(), &mut sources, &mut visited)?;
        comb_blocks.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(signal_atom(assignment.target())),
            sources,
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr: coerce_slt_expression_to_type(
                artifact,
                assignment.value(),
                target_type,
                &mut arena,
                &mut node_cache,
            )?,
        });
    }
    comb_blocks.extend(comb_process_paths);

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

    let initial_memory_values: Vec<_> = artifact
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
        time_precision: None,
        time_scale: celox_design::TimeScale::default(),
        name: artifact.module_name().to_string(),
        variables,
        ff_access_summaries: HashMap::default(),
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        split_commit_ff_blocks: HashMap::default(),
        parallel_ff_parts: HashMap::default(),
        glue_blocks: HashMap::default(),
        indexed_instance_names: HashSet::default(),
        instance_index_bases: HashMap::default(),
        comb_blocks,
        comb_observers: Vec::new(),
        runtime_errors: HashMap::default(),
        runtime_event_sites: Vec::new(),
        extern_functions: Vec::new(),
        folded_initial_memory_values: Vec::new(),
        declaration_initial_memory_values: initial_memory_values.clone(),
        initial_memory_values,
        comb_boundaries: HashMap::default(),
        arena,
        reset_clock_map,
        processes,
        package_bindings: HashMap::default(),
        counter_slots,
        pending_slots: Vec::new(),
        comb_alternatives: Vec::new(),
        wait_sites,
        nba_sites,
        nba_switches: Vec::new(),
        host_request_sites: Vec::new(),
        time_slot: None,
    };
    let symbolic = SymbolicRtl {
        modules: [(module_id, sim_module.clone())].into_iter().collect(),
        module_names: [(module_id, artifact.module_name().to_string())]
            .into_iter()
            .collect(),
        root_id: module_id,
        packages: Vec::new(),
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
        packages: Vec::new(),
    };
    Ok(LoweredFrontendArtifact { symbolic, external })
}
