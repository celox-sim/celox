//! Projection from the stable frontend SDK into Celox symbolic internals.

use celox_design::{
    BinaryOp, BitAccess, DomainKind, InitialStateData, InitialStateValue, ModuleId, PortTypeKind,
    RegionedVarAddrBase, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, VariableMetadata,
    WORKING_REGION,
};
use celox_frontend_sdk::{
    ActiveLevel, Direction, Edge, ExprId, ExprNode, FrontendArtifact, SignalId, SignalSlice,
    ValueType,
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
        let mut visited = HashSet::default();
        expression_sources(artifact, assignment.value(), &mut sources, &mut visited)?;
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
