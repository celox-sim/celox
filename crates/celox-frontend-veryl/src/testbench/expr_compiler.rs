//! Compiling testbench expressions.

use super::*;

pub(super) fn compile_assert_message(
    args: &[SystemFunctionInput],
    ec: &ExprCompiler<'_>,
) -> Option<GenericAssertMessage<SemanticArgument<StateAddr>>> {
    if args.is_empty() {
        return None;
    }
    if let Some(template) = static_string_expr(&args[0].0) {
        let compiled_args = args[1..]
            .iter()
            .map(|arg| compile_assert_arg(arg, ec))
            .collect::<Vec<_>>();
        Some(GenericAssertMessage::Formatted {
            template,
            args: compiled_args,
        })
    } else {
        Some(GenericAssertMessage::DynamicArgs(
            args.iter().map(|arg| compile_assert_arg(arg, ec)).collect(),
        ))
    }
}

pub(super) fn lower_testbench_operator(op: VerylOp) -> Op {
    match op {
        VerylOp::Add => Op::Add,
        VerylOp::Sub => Op::Sub,
        VerylOp::Mul => Op::Mul,
        VerylOp::Div => Op::Div,
        VerylOp::Rem => Op::Rem,
        VerylOp::Pow => Op::Pow,
        VerylOp::BitAnd => Op::BitAnd,
        VerylOp::BitOr => Op::BitOr,
        VerylOp::BitXor => Op::BitXor,
        VerylOp::BitXnor => Op::BitXnor,
        VerylOp::BitNand => Op::BitNand,
        VerylOp::BitNor => Op::BitNor,
        VerylOp::LogicShiftL => Op::LogicShiftL,
        VerylOp::LogicShiftR => Op::LogicShiftR,
        VerylOp::ArithShiftL => Op::ArithShiftL,
        VerylOp::ArithShiftR => Op::ArithShiftR,
        VerylOp::Eq => Op::Eq,
        VerylOp::EqWildcard => Op::EqWildcard,
        VerylOp::Ne => Op::Ne,
        VerylOp::NeWildcard => Op::NeWildcard,
        VerylOp::Less => Op::Less,
        VerylOp::LessEq => Op::LessEq,
        VerylOp::Greater => Op::Greater,
        VerylOp::GreaterEq => Op::GreaterEq,
        VerylOp::LogicAnd => Op::LogicAnd,
        VerylOp::LogicOr => Op::LogicOr,
        VerylOp::LogicNot => Op::LogicNot,
        VerylOp::BitNot => Op::BitNot,
        _ => unreachable!("operator cannot be represented in testbench bytecode: {op:?}"),
    }
}

pub(super) struct ExprCompiler<'a> {
    pub(super) lookup: &'a FrontendLookup,
    pub(super) id_map: &'a VerylIdMap,
    pub(super) functions: &'a HashMap<VarId, Function>,
    pub(super) constant_strings: &'a HashMap<StateAddr, Expression>,
    pub(super) base_instance: InstanceId,
}

impl ExprCompiler<'_> {
    pub(super) fn local_variable(&self, var_id: VarId) -> Option<(StateAddr, &VariableInfo)> {
        let source_id = self
            .id_map
            .instance_var(self.lookup, self.base_instance, var_id)?;
        self.lookup.instance_variable(self.base_instance, source_id)
    }

    pub(super) fn hierarchical_variable(
        &self,
        reference: &HierVarRef,
    ) -> Result<(StateAddr, &VariableInfo), ParserError> {
        resolve_hierarchical_reference_from(self.lookup, self.base_instance, reference)
    }

    pub(super) fn compile(&self, expr: &Expression) -> ExprBytecode<StateLocation<StateAddr>> {
        let mut ops = Vec::new();
        self.emit(expr, &mut ops);
        ExprBytecode::new(ops)
    }

    pub(super) fn compile_with_width(
        &self,
        expr: &Expression,
        width: usize,
    ) -> ExprBytecode<StateLocation<StateAddr>> {
        let mut ops = Vec::new();
        self.emit_in_context(
            expr,
            &mut ops,
            Some(ValueContext {
                width,
                signed: expression_signed(expr),
            }),
        );
        ExprBytecode::new(ops)
    }

    pub(super) fn natural_width(&self, expr: &Expression) -> usize {
        self.resolved_variable_access_width(expr)
            .or_else(|| get_expr_width(expr))
            .filter(|width| *width != 0)
            .unwrap_or_else(|| self.infer_expr_width(expr).max(1))
    }

    pub(super) fn resolved_variable_access_width(&self, expr: &Expression) -> Option<usize> {
        let Expression::Term(factor) = expr else {
            return None;
        };
        match factor.as_ref() {
            Factor::Variable(var_id, index, select, _) => {
                let (_, info) = self.local_variable(*var_id)?;
                Some(variable_access_width(info, index, select).unwrap_or(info.width))
            }
            Factor::HierVariable(reference) => {
                let (_, info) = self.hierarchical_variable(reference).ok()?;
                Some(
                    variable_access_width(info, &reference.index, &reference.select)
                        .unwrap_or(info.width),
                )
            }
            _ => None,
        }
    }

    pub(super) fn root_context(&self, expr: &Expression) -> ValueContext {
        ValueContext {
            width: self
                .natural_width(expr)
                .max(expr.comptime().expr_context.width),
            signed: expression_signed(expr),
        }
    }

    pub(super) fn emit(&self, expr: &Expression, ops: &mut Vec<UnboundTbOpcode>) {
        self.emit_in_context(expr, ops, Some(self.root_context(expr)));
    }

    pub(super) fn resize_result(
        &self,
        ops: &mut Vec<UnboundTbOpcode>,
        source: ValueContext,
        target: Option<ValueContext>,
    ) -> ValueContext {
        let Some(target) = target else {
            return source;
        };
        if source.width != target.width {
            ops.push(TbOpcode::Resize {
                source_width: source.width,
                target_width: target.width,
                signed: target.signed,
            });
        }
        target
    }

    pub(super) fn emit_in_context(
        &self,
        expr: &Expression,
        ops: &mut Vec<UnboundTbOpcode>,
        context: Option<ValueContext>,
    ) -> ValueContext {
        // A string's declared width is nominal; its runtime length must never
        // pass through integer context sizing or assignment truncation.
        let context = if expr.comptime().r#type.is_string()
            || crate::float::precision(crate::float::type_of(expr)).is_some()
        {
            None
        } else {
            context
        };
        match expr {
            Expression::Term(f) => {
                // A fill literal (`'0`, `'1`, `'x`, `'z`) fills its context
                // (IEEE 1800-2023 5.7.1).
                if let (Some(context), Factor::Value(comptime)) = (context, f.as_ref())
                    && comptime.get_value().is_ok_and(|value| value.width() == 0)
                    && let Some((payload, mask, width, _)) =
                        crate::bitaccess::celox_value_from_comptime_in_context(
                            comptime,
                            Some(context.width),
                        )
                {
                    if mask.is_zero() {
                        ops.push(if width <= 64 {
                            TbOpcode::ConstU64(payload.to_u64().unwrap_or(0))
                        } else {
                            TbOpcode::ConstWide(payload)
                        });
                    } else {
                        ops.push(TbOpcode::ConstLogic {
                            payload,
                            mask,
                            width,
                        });
                    }
                    return context;
                }
                self.emit_factor(f, ops);
                let source = ValueContext {
                    width: self.natural_width(expr),
                    signed: expression_signed(expr),
                };
                self.resize_result(ops, source, context)
            }
            Expression::Unary(op, inner, _) => {
                if let Some(precision) = crate::float::precision(crate::float::type_of(inner)) {
                    let operand = self.emit_in_context(inner, ops, None);
                    if *op == VerylOp::Sub {
                        ops.push(TbOpcode::FloatUnary {
                            op: celox_design::FloatUnaryOp::Neg,
                            precision,
                        });
                    }
                    return operand;
                }
                let reduction = matches!(
                    op,
                    VerylOp::BitAnd
                        | VerylOp::BitNand
                        | VerylOp::BitOr
                        | VerylOp::BitNor
                        | VerylOp::BitXor
                        | VerylOp::BitXnor
                        | VerylOp::LogicNot
                );
                let natural_width = self.natural_width(inner);
                let operand_context = if reduction {
                    None
                } else {
                    Some(ValueContext {
                        width: natural_width.max(context.map(|x| x.width).unwrap_or(0)),
                        signed: context
                            .map(|x| x.signed)
                            .unwrap_or_else(|| expression_signed(inner)),
                    })
                };
                let operand = self.emit_in_context(inner, ops, operand_context);
                let result_width = if reduction { 1 } else { operand.width };
                match op {
                    VerylOp::Add
                    | VerylOp::Sub
                    | VerylOp::BitNot
                    | VerylOp::BitAnd
                    | VerylOp::BitNand
                    | VerylOp::BitOr
                    | VerylOp::BitNor
                    | VerylOp::BitXor
                    | VerylOp::BitXnor
                    | VerylOp::LogicNot => ops.push(TbOpcode::TypedUnary {
                        op: lower_testbench_operator(*op),
                        operand_width: operand.width,
                        result_width,
                    }),
                    _ => unreachable!("operator is not unary in a testbench expression: {op:?}"),
                }
                let result = ValueContext {
                    width: result_width,
                    signed: if reduction { false } else { operand.signed },
                };
                self.resize_result(ops, result, context)
            }
            Expression::Binary(lhs, op, rhs, _) => {
                if matches!(op, VerylOp::As) {
                    if let Some(celox_design::UnaryOp::Float { op, precision }) =
                        crate::float::conversion(
                            crate::float::type_of(lhs),
                            crate::float::type_of(expr),
                        )
                    {
                        self.emit_in_context(lhs, ops, None);
                        ops.push(TbOpcode::FloatUnary { op, precision });
                        return self.resize_result(
                            ops,
                            ValueContext {
                                width: self.natural_width(expr),
                                signed: expression_signed(expr),
                            },
                            context,
                        );
                    }
                    let cast = cast_semantics(lhs, rhs)
                        .expect("analyzed testbench cast must have a concrete target");
                    let source = self.emit_in_context(
                        lhs,
                        ops,
                        Some(ValueContext {
                            width: cast.width,
                            signed: cast.source_signed,
                        }),
                    );
                    if cast.result_is_2state && !cast.source_is_2state {
                        ops.push(TbOpcode::ToTwoState);
                    }
                    let casted = ValueContext {
                        width: source.width,
                        signed: cast.result_signed,
                    };
                    return self.resize_result(ops, casted, context);
                }

                if crate::float::precision(crate::float::type_of(lhs)).is_some()
                    || crate::float::precision(crate::float::type_of(rhs)).is_some()
                {
                    let target = if matches!(
                        crate::float::precision(crate::float::type_of(lhs)),
                        Some(celox_design::FloatPrecision::F64)
                    ) {
                        crate::float::type_of(lhs)
                    } else if crate::float::precision(crate::float::type_of(rhs)).is_some() {
                        crate::float::type_of(rhs)
                    } else {
                        crate::float::type_of(lhs)
                    };
                    self.emit_in_context(&crate::float::converted(lhs, target), ops, None);
                    self.emit_in_context(&crate::float::converted(rhs, target), ops, None);
                    ops.push(TbOpcode::FloatBinary {
                        op: crate::float::binary(*op).expect("validated float operator"),
                        precision: crate::float::precision(target).unwrap(),
                    });
                    return self.resize_result(
                        ops,
                        ValueContext {
                            width: self.natural_width(expr),
                            signed: expression_signed(expr),
                        },
                        context,
                    );
                }
                if lhs.comptime().r#type.is_string() && rhs.comptime().r#type.is_string() {
                    self.emit_in_context(lhs, ops, None);
                    self.emit_in_context(rhs, ops, None);
                    ops.push(TbOpcode::BinOp(lower_testbench_operator(*op)));
                    return self.resize_result(
                        ops,
                        ValueContext {
                            width: 1,
                            signed: false,
                        },
                        context,
                    );
                }
                let lhs_width = self.natural_width(lhs);
                let rhs_width = self.natural_width(rhs);
                let semantics = binary_semantics(
                    *op,
                    lhs_width,
                    rhs_width,
                    expression_signed(lhs),
                    expression_signed(rhs),
                    context,
                );
                let lhs = self.emit_in_context(lhs, ops, semantics.lhs_context);
                let rhs = self.emit_in_context(rhs, ops, semantics.rhs_context);
                match op {
                    VerylOp::Pow
                    | VerylOp::Div
                    | VerylOp::Rem
                    | VerylOp::Mul
                    | VerylOp::Add
                    | VerylOp::Sub
                    | VerylOp::ArithShiftL
                    | VerylOp::ArithShiftR
                    | VerylOp::LogicShiftL
                    | VerylOp::LogicShiftR
                    | VerylOp::LessEq
                    | VerylOp::GreaterEq
                    | VerylOp::Less
                    | VerylOp::Greater
                    | VerylOp::Eq
                    | VerylOp::EqWildcard
                    | VerylOp::Ne
                    | VerylOp::NeWildcard
                    | VerylOp::BitAnd
                    | VerylOp::BitOr
                    | VerylOp::BitXor
                    | VerylOp::BitXnor
                    | VerylOp::LogicAnd
                    | VerylOp::LogicOr => ops.push(TbOpcode::TypedBinOp {
                        op: lower_testbench_operator(*op),
                        lhs_width: lhs.width,
                        rhs_width: rhs.width,
                        result_width: semantics.result_width,
                        lhs_signed: semantics.lhs_signed,
                        rhs_signed: semantics.rhs_signed,
                    }),
                    _ => unreachable!("operator is not binary in a testbench expression: {op:?}"),
                }
                let result = ValueContext {
                    width: semantics.result_width,
                    signed: semantics.result_signed,
                };
                self.resize_result(ops, result, context)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                let target_type = crate::float::type_of(expr);
                let then_value = crate::float::converted(then_expr, target_type);
                let else_value = crate::float::converted(else_expr, target_type);
                let then_expr = &then_value;
                let else_expr = &else_value;
                self.emit_in_context(cond, ops, None);
                let branch_context = ValueContext {
                    width: self
                        .natural_width(then_expr)
                        .max(self.natural_width(else_expr))
                        .max(context.map(|x| x.width).unwrap_or(0)),
                    signed: context.map(|x| x.signed).unwrap_or_else(|| {
                        expression_signed(then_expr) && expression_signed(else_expr)
                    }),
                };
                let mut then_ops = Vec::new();
                self.emit_in_context(then_expr, &mut then_ops, Some(branch_context));
                let mut else_ops = Vec::new();
                self.emit_in_context(else_expr, &mut else_ops, Some(branch_context));
                ops.push(TbOpcode::Ternary {
                    then_len: then_ops.len(),
                    else_len: else_ops.len(),
                });
                ops.extend(then_ops);
                ops.extend(else_ops);
                branch_context
            }
            Expression::Concatenation(parts, _) => {
                ops.push(TbOpcode::ConstU64(0));
                let mut result_width = 0usize;
                for (val_expr, repeat_expr) in parts {
                    let part_width = self.natural_width(val_expr);
                    let repeat = repeat_expr
                        .as_ref()
                        .and_then(Self::try_const_usize)
                        .unwrap_or(1);
                    for _ in 0..repeat {
                        self.emit_in_context(val_expr, ops, None);
                        result_width = result_width.saturating_add(part_width);
                        ops.push(TbOpcode::ConcatPart {
                            part_width,
                            result_width,
                        });
                    }
                }
                self.resize_result(
                    ops,
                    ValueContext {
                        width: result_width.max(1),
                        signed: false,
                    },
                    context,
                )
            }
            Expression::ArrayLiteral(..) | Expression::StructConstructor(..) => {
                unreachable!("aggregate testbench expressions must be lowered before bytecode")
            }
        }
    }

    pub(super) fn emit_factor(&self, factor: &Factor, ops: &mut Vec<UnboundTbOpcode>) {
        if factor.comptime().r#type.is_string() {
            match factor {
                Factor::Variable(id, index, select, comptime) if !comptime.is_const => {
                    let (address, info) =
                        self.local_variable(*id).expect("string variable metadata");
                    self.emit_string_access(address, info, index, select, ops);
                    return;
                }
                Factor::HierVariable(reference) => {
                    let (address, info) = self
                        .hierarchical_variable(reference)
                        .expect("string variable metadata");
                    self.emit_string_access(
                        address,
                        info,
                        &reference.index,
                        &reference.select,
                        ops,
                    );
                    return;
                }
                _ => {
                    if let Ok(value) = factor.comptime().get_value() {
                        let len = value.width().div_ceil(8);
                        let bytes = value.payload().to_bytes_be();
                        let mut result = vec![0; len];
                        let copied = len.min(bytes.len());
                        result[len - copied..].copy_from_slice(&bytes[bytes.len() - copied..]);
                        ops.push(TbOpcode::ConstString(result));
                        return;
                    }
                }
            }
        }
        match factor {
            Factor::Variable(var_id, index, select, comptime) => {
                if comptime.is_const
                    && let Ok(value) = comptime.get_value()
                {
                    self.emit_constant_value(value, ops);
                } else if let Some((address, info)) = self.local_variable(*var_id) {
                    self.emit_var_access(address, info, index, select, ops);
                } else if let Ok(value) = comptime.get_value() {
                    self.emit_constant_value(value, ops);
                } else {
                    unreachable!("unresolved non-constant testbench variable {var_id:?}");
                }
            }
            Factor::Value(comptime) => {
                if let Ok(val) = comptime.get_value() {
                    self.emit_constant_value(val, ops);
                } else {
                    unreachable!("analyzed testbench literal has no value");
                }
            }
            Factor::FunctionCall(fc) => {
                self.emit_function_call(fc, ops);
            }
            Factor::HierVariable(reference) => {
                let (address, info) = self
                    .hierarchical_variable(reference)
                    .expect("hierarchical testbench references are validated before emission");
                self.emit_var_access(address, info, &reference.index, &reference.select, ops);
            }
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
                    self.emit(&input.0, ops);
                }
                _ => {
                    let value = call
                        .comptime
                        .get_value()
                        .expect("testbench system function must be compile-time evaluable");
                    self.emit_constant_value(value, ops);
                }
            },
            Factor::Anonymous(comptime) | Factor::Unknown(comptime) => {
                let value = comptime
                    .get_value()
                    .expect("resolved testbench factor must have a value");
                self.emit_constant_value(value, ops);
            }
        }
    }

    pub(super) fn emit_string_access(
        &self,
        address: StateAddr,
        info: &VariableInfo,
        index: &VarIndex,
        select: &VarSelect,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        if let Some(expression) = self.constant_strings.get(&address) {
            self.emit(expression, ops);
            return;
        }

        let target = self
            .resolve_target_parts(
                SemanticSignal {
                    address,
                    width: info.width,
                },
                info,
                index,
                select,
            )
            .expect("validated string access");
        if let Some(selection) = target.selection {
            ops.extend_from_slice(selection.offset.ops());
        } else {
            ops.push(TbOpcode::ConstU64(0));
        }
        ops.push(TbOpcode::LoadString {
            location: Self::state_location_at(address, 0),
            elements: info.width / celox_design::HANDLE_WIDTH,
        });
    }

    pub(super) fn emit_constant_value(
        &self,
        value: &veryl_analyzer::value::Value,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let mask = value.mask_xz();
        if !mask.is_zero() {
            // Celox encodes an X bit as (1, 1) and a Z bit as (0, 1); the
            // analyzer has the opposite value bits.
            ops.push(TbOpcode::ConstLogic {
                payload: value.payload().as_ref() ^ mask.as_ref(),
                mask: mask.into_owned(),
                width: value.width().max(1),
            });
        } else if value.width() <= 64 {
            ops.push(TbOpcode::ConstU64(value.payload_u64()));
        } else {
            ops.push(TbOpcode::ConstWide(value.payload().into_owned()));
        }
    }

    /// Emit bytecode for a function call used as an expression value.
    /// Inline-expands: store args → emit body assigns → load return value.
    pub(super) fn emit_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let func = match self.functions.get(&fc.id) {
            Some(f) => f,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };
        let func_body = match if let Some(idx) = &fc.index {
            func.get_function(idx)
        } else {
            func.get_function(&[])
        } {
            Some(fb) => fb,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };

        // 1. Store input arguments into memory
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                if let Some(sig) = self.resolve_var(&arg_var_id) {
                    let converted = func
                        .args
                        .iter()
                        .flat_map(|arg| &arg.members)
                        .find(|(path, _, _)| path == arg_path)
                        .map(|(_, ct, _)| crate::float::converted(arg_expr, &ct.r#type))
                        .unwrap_or(std::borrow::Cow::Borrowed(arg_expr));
                    self.emit_in_context(
                        &converted,
                        ops,
                        Some(ValueContext {
                            width: sig.width,
                            signed: expression_signed(arg_expr),
                        }),
                    );
                    if arg_expr.comptime().r#type.is_string() {
                        ops.push(TbOpcode::StoreString {
                            location: self.state_location(arg_var_id, 0),
                        });
                    } else {
                        ops.push(TbOpcode::StoreU64 {
                            location: self.state_location(arg_var_id, 0),
                            byte_size: get_byte_size(sig.width),
                        });
                    }
                }
            }
        }

        // 2. Emit body statements as bytecode (only Assign is supported)
        for stmt in &func_body.statements {
            if let veryl_analyzer::ir::Statement::Assign(a) = stmt {
                if let Some(first_dst) = a.dst.first() {
                    if let Some(dst_sig) = self.resolve_var(&first_dst.id) {
                        self.emit_in_context(
                            &crate::float::converted(&a.expr, &first_dst.comptime.r#type),
                            ops,
                            Some(ValueContext {
                                width: dst_sig.width,
                                signed: expression_signed(&a.expr),
                            }),
                        );
                        if a.expr.comptime().r#type.is_string() {
                            ops.push(TbOpcode::StoreString {
                                location: self.state_location(first_dst.id, 0),
                            });
                        } else {
                            ops.push(TbOpcode::StoreU64 {
                                location: self.state_location(first_dst.id, 0),
                                byte_size: get_byte_size(dst_sig.width),
                            });
                        }
                    }
                }
            }
            // Non-assign statements (if/for in function body) are skipped.
            // This covers the common case of pure computation functions.
        }

        // 3. Load return value
        if let Some(ret_var_id) = &func_body.ret {
            if let Some(sig) = self.resolve_var(ret_var_id) {
                if fc.comptime.r#type.is_string() {
                    ops.push(TbOpcode::ConstU64(0));
                    ops.push(TbOpcode::LoadString {
                        location: self.state_location(*ret_var_id, 0),
                        elements: 1,
                    });
                } else {
                    self.emit_load(*ret_var_id, 0, sig.width, ops);
                }
            } else {
                ops.push(TbOpcode::ConstU64(0));
            }
        } else {
            ops.push(TbOpcode::ConstU64(0));
        }
    }

    /// Emit bytecode for a variable access, handling static and dynamic
    /// array indices and bit selects.
    pub(super) fn emit_var_access(
        &self,
        address: StateAddr,
        info: &VariableInfo,
        index: &veryl_analyzer::ir::VarIndex,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        // No index or select → whole variable
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            self.emit_load_at(address, 0, info.width, ops);
            return;
        }

        let array_total: usize = info.array_dims.iter().product::<usize>().max(1);
        let element_width = info.width / array_total;

        // Compute array strides
        let mut strides_bits = vec![element_width; info.array_dims.len()];
        if !info.array_dims.is_empty() {
            let mut stride = element_width;
            for i in (0..info.array_dims.len()).rev() {
                strides_bits[i] = stride;
                stride *= info.array_dims[i];
            }
        }

        // Build one flattened bit offset for all unpacked and packed indices.
        // Dynamic terms remain on the expression stack until the final load.
        let mut static_bit_offset: usize = 0;
        let mut dynamic_terms = 0usize;

        for (i, idx_expr) in index.0.iter().enumerate() {
            if i >= info.array_dims.len() {
                break;
            }
            let stride = strides_bits[i];

            if let Some(idx_val) = Self::try_const_usize(idx_expr) {
                // Static index: accumulate into offset
                static_bit_offset += idx_val * stride;
            } else {
                // Keep the dynamic flattened bit offset on the stack. All
                // dimensions are consumed before the selected subarray is
                // loaded, so a dynamic outer index does not discard inner
                // indices or force the result down to one scalar element.
                self.emit(idx_expr, ops);
                Self::finish_dynamic_offset_term(stride, ops, &mut dynamic_terms);
            }
        }

        let accessed_width = if index.0.len() >= info.array_dims.len() {
            element_width
        } else if index.0.is_empty() {
            info.width
        } else {
            strides_bits[index.0.len() - 1]
        };

        let (select_offset, selected_width) =
            self.emit_select_offset(info, select, accessed_width, ops, &mut dynamic_terms);
        static_bit_offset += select_offset;

        if dynamic_terms != 0 {
            ops.push(TbOpcode::LoadIndexed {
                location: Self::state_location_at(address, 0),
                stride_bits: 1,
                base_bit_offset: static_bit_offset,
                element_width: selected_width,
            });
            return;
        }

        let bit_offset = static_bit_offset;
        let byte_offset = bit_offset / 8;
        let sub = bit_offset % 8;
        if sub == 0 {
            self.emit_load_at(address, byte_offset, selected_width, ops);
        } else {
            let load_width = selected_width + sub;
            self.emit_load_at(address, byte_offset, load_width, ops);
            ops.push(TbOpcode::ConstU64(sub as u64));
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if selected_width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << selected_width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    pub(super) fn finish_dynamic_offset_term(
        multiplier: usize,
        ops: &mut Vec<UnboundTbOpcode>,
        dynamic_terms: &mut usize,
    ) {
        if multiplier != 1 {
            ops.push(TbOpcode::ConstU64(multiplier as u64));
            ops.push(TbOpcode::BinOp(Op::Mul));
        }
        if *dynamic_terms != 0 {
            ops.push(TbOpcode::BinOp(Op::Add));
        }
        *dynamic_terms += 1;
    }

    /// Append packed indices to the flattened bit offset and return the
    /// remaining static offset plus the selected result width.
    pub(super) fn emit_select_offset(
        &self,
        info: &VariableInfo,
        select: &VarSelect,
        accessed_width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
        dynamic_terms: &mut usize,
    ) -> (usize, usize) {
        if select.0.is_empty() && select.1.is_none() {
            return (0, accessed_width);
        }

        let packed_dimensions = if info.packed_dims.is_empty() {
            vec![accessed_width]
        } else {
            info.packed_dims.clone()
        };
        let mut strides = vec![1usize; packed_dimensions.len()];
        let mut stride = 1usize;
        for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
            strides[dimension] = stride;
            stride = stride.saturating_mul(*size);
        }

        let prefix_count = if select.1.is_some() {
            select.0.len().saturating_sub(1)
        } else {
            select.0.len()
        };
        let mut static_offset = 0usize;
        for (dimension, expression) in select.0[..prefix_count].iter().enumerate() {
            let scale = strides[dimension];
            if let Some(index) = Self::try_const_usize(expression) {
                static_offset += index * scale;
            } else {
                self.emit(expression, ops);
                Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
            }
        }

        let Some((op, range_expression)) = &select.1 else {
            let selected_width = strides[select.0.len() - 1];
            return (static_offset, selected_width);
        };
        let anchor_expression = select
            .0
            .last()
            .expect("validated part select has an anchor");
        let range = Self::try_const_usize(range_expression)
            .expect("validated part-select range is compile-time constant");
        let scale = strides[prefix_count];
        let elements = match op {
            VarSelectOp::Colon => {
                let anchor = Self::try_const_usize(anchor_expression)
                    .expect("validated colon-select anchor is compile-time constant");
                static_offset += range * scale;
                anchor - range + 1
            }
            VarSelectOp::PlusColon => {
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += anchor * scale;
                } else {
                    self.emit(anchor_expression, ops);
                    Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
                }
                range
            }
            VarSelectOp::MinusColon => {
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += (anchor + 1 - range) * scale;
                } else {
                    self.emit(anchor_expression, ops);
                    ops.push(TbOpcode::ConstU64(1));
                    ops.push(TbOpcode::BinOp(Op::Add));
                    ops.push(TbOpcode::ConstU64(range as u64));
                    ops.push(TbOpcode::BinOp(Op::Sub));
                    Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
                }
                range
            }
            VarSelectOp::Step => {
                let multiplier = range.saturating_mul(scale);
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += anchor * multiplier;
                } else {
                    self.emit(anchor_expression, ops);
                    Self::finish_dynamic_offset_term(multiplier, ops, dynamic_terms);
                }
                range
            }
        };
        (static_offset, elements * scale)
    }

    pub(super) fn append_offset_term(
        ops: &mut Vec<UnboundTbOpcode>,
        term: impl FnOnce(&mut Vec<UnboundTbOpcode>),
    ) {
        term(ops);
        ops.push(TbOpcode::BinOp(Op::Add));
    }

    pub(super) fn selection_offset(
        &self,
        select: &veryl_analyzer::ir::VarSelect,
        packed_dims: &[usize],
    ) -> Option<(Vec<UnboundTbOpcode>, usize)> {
        if packed_dims.is_empty() || select.0.is_empty() {
            return None;
        }

        let mut strides = vec![1usize; packed_dims.len()];
        let mut stride = 1usize;
        for i in (0..packed_dims.len()).rev() {
            strides[i] = stride;
            stride = stride.checked_mul(packed_dims[i])?;
        }

        let mut ops = vec![TbOpcode::ConstU64(0)];
        let append_index =
            |ops: &mut Vec<UnboundTbOpcode>, expr: &Expression, scale: usize| -> Option<()> {
                if let Some(index) = Self::try_const_usize(expr) {
                    let offset = index.checked_mul(scale)?;
                    Self::append_offset_term(ops, |ops| {
                        ops.push(TbOpcode::ConstU64(offset as u64));
                    });
                } else {
                    Self::append_offset_term(ops, |ops| {
                        self.emit(expr, ops);
                        if scale != 1 {
                            ops.push(TbOpcode::ConstU64(scale as u64));
                            ops.push(TbOpcode::BinOp(Op::Mul));
                        }
                    });
                }
                Some(())
            };

        if let Some((op, range_expr)) = &select.1 {
            let dimension = select.0.len() - 1;
            let scale = *strides.get(dimension)?;
            for (index, expr) in select.0[..dimension].iter().enumerate() {
                append_index(&mut ops, expr, *strides.get(index)?)?;
            }

            let anchor = select.0.last()?;
            let range = Self::try_const_usize(range_expr)?;
            let (part_ops, elements) = match op {
                // In `base[msb:lsb]`, the second operand is the LSB, not
                // the selected width.  In particular, `[3:0]` is four bits.
                veryl_analyzer::ir::VarSelectOp::Colon => {
                    let anchor = Self::try_const_usize(anchor)?;
                    let elements = anchor.checked_sub(range)?.checked_add(1)?;
                    let offset = range.checked_mul(scale)?;
                    (vec![TbOpcode::ConstU64(offset as u64)], elements)
                }
                veryl_analyzer::ir::VarSelectOp::PlusColon => {
                    if range == 0 {
                        return None;
                    }
                    if let Some(anchor) = Self::try_const_usize(anchor) {
                        let offset = anchor.checked_mul(scale)?;
                        (vec![TbOpcode::ConstU64(offset as u64)], range)
                    } else {
                        let mut part_ops = Vec::new();
                        self.emit(anchor, &mut part_ops);
                        if scale != 1 {
                            part_ops.push(TbOpcode::ConstU64(scale as u64));
                            part_ops.push(TbOpcode::BinOp(Op::Mul));
                        }
                        (part_ops, range)
                    }
                }
                veryl_analyzer::ir::VarSelectOp::MinusColon => {
                    if range == 0 {
                        return None;
                    }
                    if let Some(anchor) = Self::try_const_usize(anchor) {
                        let lsb = anchor.checked_add(1)?.checked_sub(range)?;
                        let offset = lsb.checked_mul(scale)?;
                        (vec![TbOpcode::ConstU64(offset as u64)], range)
                    } else {
                        let mut part_ops = Vec::new();
                        self.emit(anchor, &mut part_ops);
                        part_ops.push(TbOpcode::ConstU64(1));
                        part_ops.push(TbOpcode::BinOp(Op::Add));
                        part_ops.push(TbOpcode::ConstU64(range as u64));
                        part_ops.push(TbOpcode::BinOp(Op::Sub));
                        if scale != 1 {
                            part_ops.push(TbOpcode::ConstU64(scale as u64));
                            part_ops.push(TbOpcode::BinOp(Op::Mul));
                        }
                        (part_ops, range)
                    }
                }
                veryl_analyzer::ir::VarSelectOp::Step => {
                    if range == 0 {
                        return None;
                    }
                    if let Some(anchor) = Self::try_const_usize(anchor) {
                        let offset = anchor.checked_mul(range)?.checked_mul(scale)?;
                        (vec![TbOpcode::ConstU64(offset as u64)], range)
                    } else {
                        let mut part_ops = Vec::new();
                        self.emit(anchor, &mut part_ops);
                        part_ops.push(TbOpcode::ConstU64(range as u64));
                        part_ops.push(TbOpcode::BinOp(Op::Mul));
                        if scale != 1 {
                            part_ops.push(TbOpcode::ConstU64(scale as u64));
                            part_ops.push(TbOpcode::BinOp(Op::Mul));
                        }
                        (part_ops, range)
                    }
                }
            };
            let selected_width = elements.checked_mul(scale)?;
            Self::append_offset_term(&mut ops, |ops| ops.extend(part_ops));
            Some((ops, selected_width))
        } else {
            if select.0.len() > packed_dims.len() {
                return None;
            }
            for (index, expr) in select.0.iter().enumerate() {
                append_index(&mut ops, expr, *strides.get(index)?)?;
            }
            let selected_width = if select.0.is_empty() {
                stride
            } else {
                *strides.get(select.0.len() - 1)?
            };
            Some((ops, selected_width))
        }
    }

    pub(super) fn resolve_target(
        &self,
        destination: &veryl_analyzer::ir::AssignDestination,
    ) -> Option<TestbenchTarget<SemanticSignal<StateAddr>, ExprBytecode<StateLocation<StateAddr>>>>
    {
        let signal = self.resolve_var(&destination.id)?;
        let info = self.local_variable(destination.id)?.1;
        self.resolve_target_parts(signal, info, &destination.index, &destination.select)
    }

    pub(super) fn resolve_hierarchical_target(
        &self,
        destination: &HierAssignDestination,
    ) -> Option<TestbenchTarget<SemanticSignal<StateAddr>, ExprBytecode<StateLocation<StateAddr>>>>
    {
        let reference = hierarchical_destination_reference(destination);
        let (address, info) = self.hierarchical_variable(&reference).ok()?;
        self.resolve_target_parts(
            SemanticSignal {
                address,
                width: info.width,
            },
            info,
            &destination.index,
            &destination.select,
        )
    }

    pub(super) fn resolve_target_parts(
        &self,
        signal: SemanticSignal<StateAddr>,
        info: &VariableInfo,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<TestbenchTarget<SemanticSignal<StateAddr>, ExprBytecode<StateLocation<StateAddr>>>>
    {
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            return Some(TestbenchTarget {
                signal,
                selection: None,
                width: signal.width,
            });
        }

        let array_total: usize = info.array_dims.iter().product::<usize>().max(1);
        let element_width = info.width / array_total;
        let mut strides_bits = vec![element_width; info.array_dims.len()];
        if !info.array_dims.is_empty() {
            let mut stride = element_width;
            for i in (0..info.array_dims.len()).rev() {
                strides_bits[i] = stride;
                stride *= info.array_dims[i];
            }
        }

        let mut offset_ops = vec![TbOpcode::ConstU64(0)];
        for (i, index) in index.0.iter().enumerate() {
            let stride = *strides_bits.get(i)?;
            Self::append_offset_term(&mut offset_ops, |ops| {
                if let Some(index) = Self::try_const_usize(index) {
                    ops.push(TbOpcode::ConstU64((index * stride) as u64));
                } else {
                    self.emit(index, ops);
                    ops.push(TbOpcode::ConstU64(stride as u64));
                    ops.push(TbOpcode::BinOp(Op::Mul));
                }
            });
        }

        let accessed_width = if index.0.len() >= info.array_dims.len() {
            element_width
        } else if index.0.is_empty() {
            info.width
        } else {
            *strides_bits.get(index.0.len() - 1)?
        };
        let (select_ops, select_width) = if select.is_empty() {
            (vec![TbOpcode::ConstU64(0)], accessed_width)
        } else {
            self.selection_offset(select, &info.packed_dims)?
        };
        Self::append_offset_term(&mut offset_ops, |ops| ops.extend(select_ops));

        Some(TestbenchTarget {
            signal,
            selection: Some(TestbenchSelection {
                offset: ExprBytecode::new(offset_ops),
                width: select_width,
            }),
            width: select_width,
        })
    }

    pub(super) fn resolve_component_input_target(
        &self,
        input: &VerylComponentInputBinding,
    ) -> Option<TestbenchTarget<SemanticSignal<StateAddr>, ExprBytecode<StateLocation<StateAddr>>>>
    {
        match input {
            VerylComponentInputBinding::Root { id, index, select } => {
                let signal = self.resolve_var(id)?;
                let info = self.local_variable(*id)?.1;
                self.resolve_target_parts(signal, info, index, select)
            }
            VerylComponentInputBinding::Hierarchical(reference) => {
                let (address, info) =
                    resolve_hierarchical_reference_from(self.lookup, self.base_instance, reference)
                        .ok()?;
                let signal = SemanticSignal {
                    address,
                    width: info.width,
                };
                self.resolve_target_parts(signal, info, &reference.index, &reference.select)
            }
        }
    }

    pub(super) fn validate_target_bounds(
        &self,
        destination: &veryl_analyzer::ir::AssignDestination,
    ) -> Result<(), ParserError> {
        let Some((_, info)) = self.local_variable(destination.id) else {
            return Ok(());
        };

        Self::validate_target_bounds_parts(info, &destination.index, &destination.select)
    }

    pub(super) fn validate_target_bounds_parts(
        info: &VariableInfo,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Result<(), ParserError> {
        for (dimension, index) in index.0.iter().enumerate() {
            let Some(&size) = info.array_dims.get(dimension) else {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    format!(
                        "unpacked index dimension {dimension} exceeds the variable's {} dimensions",
                        info.array_dims.len()
                    ),
                    None,
                ));
            };
            if let Some(index) = Self::try_const_usize(index)
                && index >= size
            {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    format!(
                        "unpacked index {index} is outside dimension {dimension} of size {size}"
                    ),
                    None,
                ));
            }
        }

        if select.0.is_empty() && select.1.is_none() {
            return Ok(());
        }

        let dims = &info.packed_dims;
        let mut strides = vec![1usize; dims.len()];
        let mut total_width = 1usize;
        for i in (0..dims.len()).rev() {
            strides[i] = total_width;
            total_width = total_width.checked_mul(dims[i]).ok_or_else(|| {
                ParserError::illegal_context(
                    "testbench selected destination",
                    "packed dimensions overflow the target width",
                    None,
                )
            })?;
        }

        if let Some((op, range_expr)) = &select.1 {
            let Some(dimension) = select.0.len().checked_sub(1) else {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    "part select is missing its anchor",
                    None,
                ));
            };
            let Some(&dimension_size) = dims.get(dimension) else {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    format!("packed part-select dimension {dimension} is out of range"),
                    None,
                ));
            };
            let scale = strides[dimension];
            let mut prefix_offset = 0usize;
            let mut prefix_is_static = true;
            for (prefix_dimension, index) in select.0[..dimension].iter().enumerate() {
                if let Some(index) = Self::try_const_usize(index) {
                    let size = dims[prefix_dimension];
                    if index >= size {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "packed index {index} is outside dimension {prefix_dimension} of size {size}"
                            ),
                            None,
                        ));
                    }
                    prefix_offset = prefix_offset
                        .checked_add(index.checked_mul(strides[prefix_dimension]).ok_or_else(
                            || {
                                ParserError::illegal_context(
                                    "testbench selected destination",
                                    "packed index offset overflows usize",
                                    None,
                                )
                            },
                        )?)
                        .ok_or_else(|| {
                            ParserError::illegal_context(
                                "testbench selected destination",
                                "packed index offset overflows usize",
                                None,
                            )
                        })?;
                } else {
                    prefix_is_static = false;
                }
            }

            let Some(range) = Self::try_const_usize(range_expr) else {
                return Ok(());
            };
            let anchor = Self::try_const_usize(select.0.last().unwrap());
            if !matches!(op, veryl_analyzer::ir::VarSelectOp::Colon) && range == 0 {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    "part-select width must be nonzero",
                    None,
                ));
            }
            if matches!(op, veryl_analyzer::ir::VarSelectOp::Colon) {
                if range >= dimension_size {
                    return Err(ParserError::illegal_context(
                        "testbench selected destination",
                        format!(
                            "colon-select low bound {range} is outside dimension size {dimension_size}"
                        ),
                        None,
                    ));
                }
            } else if range > dimension_size {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    format!("part-select width {range} exceeds dimension size {dimension_size}"),
                    None,
                ));
            }

            let Some(anchor) = anchor else {
                return Ok(());
            };
            let (relative_offset, elements) = match op {
                veryl_analyzer::ir::VarSelectOp::Colon => {
                    if anchor >= dimension_size || range > anchor {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "colon-select [{anchor}:{range}] is outside dimension size {dimension_size}"
                            ),
                            None,
                        ));
                    }
                    (range.checked_mul(scale), anchor - range + 1)
                }
                veryl_analyzer::ir::VarSelectOp::PlusColon => {
                    if anchor >= dimension_size
                        || anchor
                            .checked_add(range)
                            .is_none_or(|end| end > dimension_size)
                    {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "plus-colon select anchor {anchor} and width {range} are outside dimension size {dimension_size}"
                            ),
                            None,
                        ));
                    }
                    (anchor.checked_mul(scale), range)
                }
                veryl_analyzer::ir::VarSelectOp::MinusColon => {
                    if anchor >= dimension_size || anchor + 1 < range {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "minus-colon select anchor {anchor} and width {range} are outside dimension size {dimension_size}"
                            ),
                            None,
                        ));
                    }
                    ((anchor + 1 - range).checked_mul(scale), range)
                }
                veryl_analyzer::ir::VarSelectOp::Step => {
                    if anchor
                        .checked_mul(range)
                        .and_then(|start| start.checked_add(range))
                        .is_none_or(|end| end > dimension_size)
                    {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "step select anchor {anchor} and width {range} are outside dimension size {dimension_size}"
                            ),
                            None,
                        ));
                    }
                    (
                        anchor
                            .checked_mul(range)
                            .and_then(|offset| offset.checked_mul(scale)),
                        range,
                    )
                }
            };
            let relative_offset = relative_offset.ok_or_else(|| {
                ParserError::illegal_context(
                    "testbench selected destination",
                    "packed part-select offset overflows usize",
                    None,
                )
            })?;
            let selected_width = elements.checked_mul(scale).ok_or_else(|| {
                ParserError::illegal_context(
                    "testbench selected destination",
                    "packed part-select width overflows usize",
                    None,
                )
            })?;
            if prefix_is_static
                && prefix_offset
                    .checked_add(relative_offset)
                    .and_then(|offset| offset.checked_add(selected_width))
                    .is_none_or(|end| end > total_width)
            {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    "packed part-select exceeds the target width",
                    None,
                ));
            }
        } else {
            if select.0.len() > dims.len() {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    "packed index count exceeds the variable's dimensions",
                    None,
                ));
            }
            let mut offset = 0usize;
            let mut is_static = true;
            for (dimension, index) in select.0.iter().enumerate() {
                if let Some(index) = Self::try_const_usize(index) {
                    let size = dims[dimension];
                    if index >= size {
                        return Err(ParserError::illegal_context(
                            "testbench selected destination",
                            format!(
                                "packed index {index} is outside dimension {dimension} of size {size}"
                            ),
                            None,
                        ));
                    }
                    offset = offset
                        .checked_add(index.checked_mul(strides[dimension]).ok_or_else(|| {
                            ParserError::illegal_context(
                                "testbench selected destination",
                                "packed index offset overflows usize",
                                None,
                            )
                        })?)
                        .ok_or_else(|| {
                            ParserError::illegal_context(
                                "testbench selected destination",
                                "packed index offset overflows usize",
                                None,
                            )
                        })?;
                } else {
                    is_static = false;
                }
            }
            let selected_width = if select.0.is_empty() {
                total_width
            } else {
                strides[select.0.len() - 1]
            };
            if is_static
                && offset
                    .checked_add(selected_width)
                    .is_none_or(|end| end > total_width)
            {
                return Err(ParserError::illegal_context(
                    "testbench selected destination",
                    "packed selection exceeds the target width",
                    None,
                ));
            }
        }
        Ok(())
    }

    /// Emit a LoadU64 or LoadWide opcode for the given byte offset and bit width.
    pub(super) fn emit_load(
        &self,
        var_id: VarId,
        byte_offset: usize,
        width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let address = self
            .local_variable(var_id)
            .map(|(address, _)| address)
            .expect("frontend state projection is complete");
        self.emit_load_at(address, byte_offset, width, ops);
    }

    pub(super) fn emit_load_at(
        &self,
        address: StateAddr,
        byte_offset: usize,
        width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let byte_size = get_byte_size(width);
        if byte_size <= 8 {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            ops.push(TbOpcode::LoadU64 {
                location: Self::state_location_at(address, byte_offset),
                byte_size,
                mask,
            });
        } else {
            ops.push(TbOpcode::LoadWide {
                location: Self::state_location_at(address, byte_offset),
                byte_size,
                width,
            });
        }
    }

    pub(super) fn state_location(
        &self,
        var_id: VarId,
        byte_offset: usize,
    ) -> StateLocation<StateAddr> {
        let address = self
            .local_variable(var_id)
            .map(|(address, _)| address)
            .expect("frontend state projection is complete");
        Self::state_location_at(address, byte_offset)
    }

    pub(super) fn state_location_at(
        address: StateAddr,
        byte_offset: usize,
    ) -> StateLocation<StateAddr> {
        StateLocation {
            address,
            byte_offset,
        }
    }

    /// Resolve VarIndex (unpacked array) and VarSelect (bit select) to
    /// a concrete (byte_offset, bit_width) pair.
    ///
    /// For static indices, adjusts the offset and narrows the width.
    /// Dynamic indices are not supported and fall back to the full variable.
    /// Infer the bit width of an expression. Falls back to comptime if available,
    /// otherwise resolves from VariableInfo for variables.
    pub(super) fn infer_expr_width(&self, expr: &Expression) -> usize {
        if let Some(width) = self.resolved_variable_access_width(expr) {
            return width;
        }
        let ctx_width = expr.comptime().expr_context.width;
        if ctx_width > 0 {
            return ctx_width;
        }
        // Try type-level width
        if let Some(w) = expr.comptime().r#type.total_width() {
            if w > 0 {
                return w;
            }
        }
        // For terms, use constant value widths as a final fallback.
        if let Expression::Term(f) = expr {
            if let Factor::Value(c) = f.as_ref()
                && let Ok(v) = c.get_value()
            {
                return v.width();
            }
        }
        0
    }

    pub(super) fn try_const_usize(expr: &Expression) -> Option<usize> {
        eval_constexpr(expr).and_then(|value| value.to_usize())
    }

    pub(super) fn resolve_var(&self, var_id: &VarId) -> Option<SemanticSignal<StateAddr>> {
        let (address, info) = self.local_variable(*var_id)?;
        Some(SemanticSignal {
            address,
            width: info.width,
        })
    }
}
