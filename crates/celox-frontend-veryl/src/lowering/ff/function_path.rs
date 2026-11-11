//! Path conditions of the symbolic lowering of function bodies.

use super::*;

#[derive(Clone)]
pub(super) enum FunctionPathCondition {
    Always,
    Never,
    Conditional(Box<Expression>),
}

impl<'a> FfParser<'a> {
    pub(super) fn function_path_and(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Self::normalize_function_control_condition(condition);
        match path {
            FunctionPathCondition::Always => {
                FunctionPathCondition::Conditional(Box::new(condition))
            }
            FunctionPathCondition::Never => FunctionPathCondition::Never,
            FunctionPathCondition::Conditional(path) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    path,
                    Op::LogicAnd,
                    Box::new(condition),
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    pub(super) fn function_path_and_not(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Expression::Unary(
            Op::LogicNot,
            Box::new(Self::normalize_function_control_condition(condition)),
            Box::new(Comptime::create_unknown(TokenRange::default())),
        );
        Self::function_path_and(path, condition)
    }

    pub(super) fn function_path_or(
        lhs: FunctionPathCondition,
        rhs: FunctionPathCondition,
    ) -> FunctionPathCondition {
        match (lhs, rhs) {
            (FunctionPathCondition::Always, _) | (_, FunctionPathCondition::Always) => {
                FunctionPathCondition::Always
            }
            (FunctionPathCondition::Never, rhs) => rhs,
            (lhs, FunctionPathCondition::Never) => lhs,
            (FunctionPathCondition::Conditional(lhs), FunctionPathCondition::Conditional(rhs)) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    lhs,
                    Op::LogicOr,
                    rhs,
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    pub(super) fn parse_function_path_condition<A>(
        &mut self,
        condition: &Expression,
        state: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.frames.function_arg_stack.push(state.clone());
        self.frames
            .function_array_view_stack
            .push(HashMap::default());
        self.frames.function_array_view_enabled_stack.push(false);
        let result = self.parse_expression(
            condition, targets, domain, convert, sources, ir_builder, None,
        );
        self.frames.function_array_view_enabled_stack.pop();
        self.frames.function_array_view_stack.pop();
        self.frames.function_arg_stack.pop();
        result?;
        let condition = self
            .stack
            .pop_back()
            .expect("Function path condition evaluation failed");
        Ok(self.lower_procedural_condition(condition, ir_builder))
    }

    pub(super) fn normalize_function_control_condition(condition: Expression) -> Expression {
        let token = TokenRange::default();
        let already_one_bit = condition.comptime().r#type.total_width() == Some(1)
            || matches!(
                &condition,
                Expression::Binary(
                    _,
                    Op::Eq
                        | Op::EqWildcard
                        | Op::Ne
                        | Op::NeWildcard
                        | Op::Less
                        | Op::LessEq
                        | Op::Greater
                        | Op::GreaterEq
                        | Op::LogicAnd
                        | Op::LogicOr,
                    _,
                    _,
                ) | Expression::Unary(
                    Op::BitAnd
                        | Op::BitOr
                        | Op::BitXor
                        | Op::BitNand
                        | Op::BitNor
                        | Op::BitXnor
                        | Op::LogicNot,
                    _,
                    _,
                )
            );
        let truth = if already_one_bit {
            condition
        } else {
            Expression::Unary(
                Op::BitOr,
                Box::new(condition),
                Box::new(Comptime::create_unknown(token)),
            )
        };
        let mut bit_type = Type::new(TypeKind::Bit);
        bit_type.set_concrete_width(Shape::new(vec![Some(1)]));
        let cast_target = Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Type(bit_type),
            r#type: Type::new(TypeKind::Type),
            is_const: true,
            is_global: true,
            token,
            ..Default::default()
        })));
        Expression::Binary(
            Box::new(truth),
            Op::As,
            Box::new(cast_target),
            Box::new(Comptime::create_unknown(token)),
        )
    }

    pub(super) fn function_control_may_be_true(condition: Expression) -> Expression {
        let token = TokenRange::default();
        let definitely_false = Self::normalize_function_control_condition(Expression::Unary(
            Op::LogicNot,
            Box::new(condition),
            Box::new(Comptime::create_unknown(token)),
        ));
        Expression::Unary(
            Op::LogicNot,
            Box::new(definitely_false),
            Box::new(Comptime::create_unknown(token)),
        )
    }
}
