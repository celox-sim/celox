//! Pattern matching (IEEE 1800-2023 12.6): a pattern is matched against
//! the flat value of its scrutinee once, giving a determined condition and
//! the assignments that bind its pattern variables. The constructs that
//! hold a pattern (12.6.1, 12.6.2, 12.6.3) are lowered to those conditions
//! and assignments around the bodies they guard; there is no pattern node
//! in the IR.
//!
//! A constant in a pattern compares as its construct compares its case
//! items: case equality (`===`) ordinarily, so the result is always the
//! determined value 12.6 requires. Under `casez` Z bits are wildcards,
//! and under `casex` both X and Z bits are wildcards.

use super::*;
use celox_sv_syntax::{ExprKind, Pattern, PatternField};

/// The layout of a value a pattern is matched against: its flat width and
/// how a structure or tagged union pattern decomposes it.
#[derive(Clone)]
pub(super) enum MatchLayout {
    Bits {
        ty: Type,
        width: usize,
        signed: bool,
    },
    Structure {
        ty: Type,
        fields: Vec<MatchField>,
    },
    Tagged {
        ty: Type,
        tag_width: usize,
        payload_width: usize,
        members: Vec<MatchMember>,
    },
}

#[derive(Clone)]
pub(super) struct MatchField {
    name: String,
    offset: usize,
    layout: MatchLayout,
}

#[derive(Clone)]
pub(super) struct MatchMember {
    name: String,
    index: usize,
    layout: Option<MatchLayout>,
}

impl MatchLayout {
    pub(super) fn width(&self) -> usize {
        match self {
            MatchLayout::Bits { width, .. } => *width,
            MatchLayout::Structure { fields, .. } => fields
                .iter()
                .map(|field| field.offset + field.layout.width())
                .max()
                .unwrap_or(0),
            MatchLayout::Tagged {
                tag_width,
                payload_width,
                ..
            } => tag_width + payload_width,
        }
    }

    fn ty(&self) -> &Type {
        match self {
            MatchLayout::Bits { ty, .. }
            | MatchLayout::Structure { ty, .. }
            | MatchLayout::Tagged { ty, .. } => ty,
        }
    }

    fn signed(&self) -> bool {
        match self {
            MatchLayout::Bits { signed, .. } => *signed,
            _ => false,
        }
    }
}

/// A pattern variable a compiled pattern binds: its source name, its type
/// and the value it takes from the scrutinee's flat value.
pub(super) struct PatternBinding {
    pub name: String,
    pub ty: Type,
    pub width: usize,
    pub signed: bool,
    pub value: Expr,
}

/// A compiled pattern: the condition under which it matches (none when it
/// always does) and its bindings. The bindings only mean anything when the
/// condition holds.
pub(super) struct CompiledPattern {
    pub condition: Option<Expr>,
    pub bindings: Vec<PatternBinding>,
}

fn slice(value: &Expr, offset: usize, width: usize, total: usize) -> Expr {
    if offset == 0 && width == total {
        return value.clone();
    }
    Expr::Select {
        expr: Box::new(value.clone()),
        msb: ConstExpr::Literal((offset + width - 1).to_string()),
        lsb: ConstExpr::Literal(offset.to_string()),
        signed: false,
    }
}

/// Compare `value` with `constant` (both already at `width`) as the
/// enclosing construct compares (IEEE 1800-2023 12.6, 12.6.1): case
/// equality ordinarily; under `casex`, wildcard equality accepted
/// unless definitely unequal, as ordinary casex items compare; under
/// `casez`, z bits of either side are ignored while x bits still
/// compare exactly.
fn compare_mode(value: Expr, constant: Expr, kind: CaseKind, width: usize) -> Expr {
    let binary = |left: Expr, op: BinaryOp, right: Expr| Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    match kind {
        CaseKind::Exact | CaseKind::Inside => binary(value, BinaryOp::EqCase, constant),
        CaseKind::X => binary(
            binary(value, BinaryOp::EqWildcard, constant),
            BinaryOp::NeCase,
            Expr::Literal("1'b0".into()),
        ),
        CaseKind::Z => {
            let bit = |expr: &Expr, index: usize| Expr::Select {
                expr: Box::new(expr.clone()),
                msb: ConstExpr::Literal(index.to_string()),
                lsb: ConstExpr::Literal(index.to_string()),
                signed: false,
            };
            let mut condition = None;
            for index in 0..width {
                let left = bit(&value, index);
                let right = bit(&constant, index);
                let z = Expr::Literal("1'bz".into());
                let equal = binary(left.clone(), BinaryOp::EqCase, right.clone());
                let left_z = binary(left, BinaryOp::EqCase, z.clone());
                let right_z = binary(right, BinaryOp::EqCase, z);
                let wildcard = binary(left_z, BinaryOp::LogicOr, right_z);
                let matched = binary(equal, BinaryOp::LogicOr, wildcard);
                condition = Some(match condition {
                    None => matched,
                    Some(previous) => binary(previous, BinaryOp::LogicAnd, matched),
                });
            }
            condition.unwrap_or_else(|| Expr::Literal("1'b1".into()))
        }
    }
}

fn and(left: Option<Expr>, right: Option<Expr>) -> Option<Expr> {
    match (left, right) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => Some(Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::LogicAnd,
            right: Box::new(right),
        }),
    }
}

impl<'a, 'b, 'c> BodyBuilder<'a, 'b, 'c> {
    /// The layout of values of `ty` for pattern matching, from its members
    /// or its flat width. A type with neither is not matchable.
    pub(super) fn pattern_layout(&self, ty: &Type) -> Converted<MatchLayout> {
        if ty.packed_ranges.len() == 1
            && let Some(first) = ty.members.first()
            && first.is_tagged()
        {
            let (_, tag_width, payload_width) = first
                .tag_info()
                .ok_or_else(|| unsupported("tagged union layout"))?;
            let members = ty
                .members
                .iter()
                .map(|member| {
                    let (index, ..) = member
                        .tag_info()
                        .ok_or_else(|| unsupported("tagged union layout"))?;
                    Ok(MatchMember {
                        name: member.name().to_owned(),
                        index,
                        layout: if member.is_void() {
                            None
                        } else {
                            Some(self.pattern_layout(member.r#type())?)
                        },
                    })
                })
                .collect::<Converted<Vec<_>>>()?;
            return Ok(MatchLayout::Tagged {
                ty: ty.clone(),
                tag_width,
                payload_width,
                members,
            });
        }
        if ty.packed_ranges.len() == 1 && !ty.members.is_empty() {
            let fields = ty
                .members
                .iter()
                .map(|member| {
                    Ok(MatchField {
                        name: member.name().to_owned(),
                        offset: member.offset(),
                        layout: self.pattern_layout(member.r#type())?,
                    })
                })
                .collect::<Converted<Vec<_>>>()?;
            return Ok(MatchLayout::Structure {
                ty: ty.clone(),
                fields,
            });
        }
        if let Some(aggregate) = ty.unpacked_struct_in(&self.session.types) {
            let widths = aggregate
                .members()
                .iter()
                .map(|member| Ok(self.pattern_layout(&member.r#type)?.width()))
                .collect::<Converted<Vec<_>>>()?;
            let total: usize = widths.iter().sum();
            let mut next = total;
            let fields = aggregate
                .members()
                .iter()
                .zip(&widths)
                .map(|(member, width)| {
                    next -= width;
                    Ok(MatchField {
                        name: member.name.clone(),
                        offset: next,
                        layout: self.pattern_layout(&member.r#type)?,
                    })
                })
                .collect::<Converted<Vec<_>>>()?;
            return Ok(MatchLayout::Structure {
                ty: ty.clone(),
                fields,
            });
        }
        // Only an integral value has a flat layout; a string, real or
        // handle member cannot be sliced into bits for a pattern.
        if !matches!(
            ty.kind,
            TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
        ) {
            return Err(unsupported("pattern matching against a non-integral value"));
        }
        let expr_type = expr_type_from_type(&self.tree.functions, ty, &self.dims.const_env)
            .ok_or_else(|| unsupported("pattern matching against a value of this type"))?;
        if expr_type.width == 0 {
            return Err(unsupported("pattern matching against a zero-width value"));
        }
        Ok(MatchLayout::Bits {
            ty: ty.clone(),
            width: expr_type.width,
            signed: expr_type.signed,
        })
    }

    /// Compile `pattern` against `value`, the flat value of layout
    /// `layout`, already saved in a variable that outlives the match.
    /// Constants compare as the enclosing construct compares: case
    /// equality for an ordinary construct, wildcard equality for
    /// `casez`/`casex` (12.6, 12.6.1).
    pub(super) fn compile_pattern(
        &mut self,
        pattern: &Pattern,
        layout: &MatchLayout,
        value: &Expr,
        kind: CaseKind,
    ) -> Converted<CompiledPattern> {
        let total = layout.width();
        match pattern {
            Pattern::Group(inner) => self.compile_pattern(inner, layout, value, kind),
            Pattern::Variable(name) => Ok(CompiledPattern {
                condition: None,
                bindings: vec![PatternBinding {
                    name: instances::normalize_identifier(&name.spelling),
                    ty: layout.ty().clone(),
                    width: total,
                    signed: layout.signed(),
                    value: value.clone(),
                }],
            }),
            Pattern::Wildcard => Ok(CompiledPattern {
                condition: None,
                bindings: Vec::new(),
            }),
            Pattern::Constant(id) => {
                let constant = self.native_value(*id)?;
                // Compare at the common width of the value and the
                // constant: a wider constant is extended, never
                // truncated to the value's width (11.6.1). Two signed
                // operands extend by sign (11.8.1).
                let constant_type = self.scrutinee_type(*id).ok();
                let constant_width = constant_type
                    .as_ref()
                    .and_then(|ty| self.pattern_layout(ty).ok())
                    .map(|layout| layout.width())
                    .unwrap_or(total);
                let signed =
                    layout.signed() && constant_type.as_ref().is_some_and(|ty| ty.is_signed);
                let common = total.max(constant_width);
                let value = Expr::Resize {
                    expr: Box::new(value.clone()),
                    width: common,
                    signed,
                };
                let constant = Expr::Resize {
                    expr: Box::new(constant),
                    width: common,
                    signed,
                };
                Ok(CompiledPattern {
                    condition: Some(compare_mode(value, constant, kind, common)),
                    bindings: Vec::new(),
                })
            }
            Pattern::Tagged { member, pattern } => {
                let MatchLayout::Tagged {
                    tag_width,
                    payload_width,
                    members,
                    ..
                } = layout
                else {
                    return Err(unsupported(
                        "tagged pattern against a value that is not a tagged union",
                    ));
                };
                let name = instances::normalize_identifier(&member.spelling);
                let member = members
                    .iter()
                    .find(|member| member.name == name)
                    .ok_or_else(|| {
                        unsupported(format!("pattern names no tagged union member `{name}`"))
                    })?;
                let mut condition = None;
                if *tag_width > 0 {
                    // The tag bits compare as the enclosing mode
                    // compares (12.6.1).
                    condition = Some(compare_mode(
                        slice(value, *payload_width, *tag_width, total),
                        Expr::Literal(format!("{tag_width}'d{}", member.index)),
                        kind,
                        *tag_width,
                    ));
                }
                match (pattern, &member.layout) {
                    (None, _) => Ok(CompiledPattern {
                        condition,
                        bindings: Vec::new(),
                    }),
                    (Some(_), None) => Err(unsupported(format!(
                        "pattern for void tagged union member `{name}`"
                    ))),
                    (Some(inner), Some(member_layout)) => {
                        let payload = slice(value, 0, member_layout.width(), total);
                        let inner = self.compile_pattern(inner, member_layout, &payload, kind)?;
                        Ok(CompiledPattern {
                            condition: and(condition, inner.condition),
                            bindings: inner.bindings,
                        })
                    }
                }
            }
            Pattern::Structure(fields) => {
                let MatchLayout::Structure {
                    fields: members, ..
                } = layout
                else {
                    return Err(unsupported(
                        "structure pattern against a value that is not a structure",
                    ));
                };
                let mut compiled = CompiledPattern {
                    condition: None,
                    bindings: Vec::new(),
                };
                let mut positional = 0usize;
                for field in fields {
                    let (member, inner) = match field {
                        PatternField::Positional(inner) => {
                            let member = members.get(positional).ok_or_else(|| {
                                unsupported("structure pattern has too many fields")
                            })?;
                            positional += 1;
                            (member, inner)
                        }
                        PatternField::Named(name, inner) => {
                            let name = instances::normalize_identifier(&name.spelling);
                            let member = members
                                .iter()
                                .find(|member| member.name == name)
                                .ok_or_else(|| {
                                    unsupported(format!("pattern names no member `{name}`"))
                                })?;
                            (member, inner)
                        }
                    };
                    let part = slice(value, member.offset, member.layout.width(), total);
                    let inner = self.compile_pattern(inner, &member.layout, &part, kind)?;
                    compiled.condition = and(compiled.condition, inner.condition);
                    compiled.bindings.extend(inner.bindings);
                }
                if positional > 0 && positional != members.len() {
                    return Err(unsupported(
                        "positional structure pattern does not cover every member",
                    ));
                }
                Ok(compiled)
            }
        }
    }

    /// The declared type of scrutinee expression `id`: the type its
    /// variable was declared with, or, for any other integral value, the
    /// integral type of its width and signing.
    pub(super) fn scrutinee_type(&mut self, id: celox_sv_syntax::ExprId) -> Converted<Type> {
        let native = self.syntax().declarations();
        if let Some(name) = expression_binding::native_name(&self.tree, id, native)
            && let Some(shape) = self.dims.get(&name)
            && let Some(ty) = type_names::declared_type(shape)
        {
            return Ok(ty.clone());
        }
        let semantic = expression_types::type_of(&self.tree, self.session, id, native, &self.dims)?;
        if matches!(
            self.session.types.data(semantic),
            semantic_types::TyData::Packed(semantic_types::Packed {
                element: semantic_types::PackedElement::Aggregate(_),
                ..
            }) | semantic_types::TyData::UnpackedStruct(_)
        ) {
            return bound_patterns::native_value_type(id, &self.tree, self.session, &self.dims)?
                .ok_or_else(|| {
                    unsupported("pattern matching against an unresolved aggregate layout")
                });
        }
        let semantic_types::TyData::Packed(packed) = self.session.types.data(semantic) else {
            return Err(unsupported(
                "pattern matching against a value without an integral or aggregate type",
            ));
        };
        let bits = usize::try_from(packed.bits())
            .map_err(|_| unsupported("pattern matching against a wide value"))?;
        if bits == 0 {
            return Err(unsupported("pattern matching against a zero-width value"));
        }
        let mut ty = Type::new(if packed.four_state {
            TypeKind::Logic
        } else {
            TypeKind::Bit
        });
        ty.is_signed = packed.signed;
        ty.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((bits - 1).to_string()),
            ConstExpr::Literal("0".into()),
        )];
        Ok(ty)
    }

    /// Evaluate scrutinee `id` once into a hidden variable: the statements
    /// that do it, the variable's flat value and the value's layout.
    pub(super) fn pattern_scrutinee(
        &mut self,
        id: celox_sv_syntax::ExprId,
    ) -> Converted<(Vec<Stmt>, Expr, MatchLayout)> {
        // An effects pass over a containing expression may already have
        // executed the scrutinee's updates (11.3.6); do not execute them
        // again.
        let outer = std::mem::take(&mut self.pattern_prelude);
        let mut statements = if self.scrutinee_effects_bound(id) {
            Vec::new()
        } else {
            self.effects(&[id])?
        };
        let ty = self.scrutinee_type(id)?;
        let layout = self.pattern_layout(&ty)?;
        let value = self.native_value(id)?;
        statements.extend(std::mem::replace(&mut self.pattern_prelude, outer));
        let hidden = self.hidden(ty).1;
        statements.push(Stmt::Assign {
            lhs: LValue::Ident(hidden.clone()),
            rhs: Expr::Resize {
                expr: Box::new(value),
                width: layout.width(),
                signed: layout.signed(),
            },
            nonblocking: false,
        });
        Ok((statements, Expr::Ident(hidden), layout))
    }

    /// Declare the pattern variables of `bindings` in the current scope
    /// and assign them their values: the binding assignments.
    pub(super) fn declare_pattern_bindings(
        &mut self,
        bindings: &[PatternBinding],
    ) -> Converted<Vec<Stmt>> {
        let mut seen = HashSet::default();
        let mut statements = Vec::new();
        for binding in bindings {
            if !seen.insert(binding.name.clone()) {
                return Err(unsupported(format!(
                    "pattern identifier `{}` is bound twice",
                    binding.name
                )));
            }
            let unique = self.declare(&binding.name, binding.ty.clone());
            // Bound expressions can outlive the source spelling's scope.
            // Queries such as $bits still need the renamed variable's type.
            if let Some(shape) = self.dims.get(&binding.name).cloned() {
                self.dims.insert(unique.clone(), shape);
            }
            statements.push(Stmt::Assign {
                lhs: LValue::Ident(unique),
                rhs: Expr::Resize {
                    expr: Box::new(binding.value.clone()),
                    width: binding.width,
                    signed: binding.signed,
                },
                nonblocking: false,
            });
        }
        Ok(statements)
    }

    /// Whether predicate expression `id` is a pattern-matching predicate:
    /// a `matches` clause, or more than one `&&&` clause (12.6.2).
    pub(super) fn predicate_is_patterned(&self, id: celox_sv_syntax::ExprId) -> bool {
        let mut clauses = Vec::new();
        self.predicate_clauses(id, &mut clauses);
        clauses.len() > 1
            || clauses
                .iter()
                .any(|clause| matches!(clause, PredicateClause::Match(..)))
    }

    /// One clause of a conditional predicate: a Boolean expression, or a
    /// pattern match (IEEE 1800-2023 12.6.2).
    pub(super) fn predicate_clauses(
        &self,
        id: celox_sv_syntax::ExprId,
        clauses: &mut Vec<PredicateClause>,
    ) {
        let native = self.syntax().declarations();
        match &native.expression(id).kind {
            ExprKind::Group(inner) => self.predicate_clauses(*inner, clauses),
            ExprKind::Binary {
                left,
                operator,
                right,
            } if operator == "&&&" => {
                self.predicate_clauses(*left, clauses);
                self.predicate_clauses(*right, clauses);
            }
            ExprKind::PatternMatch {
                expression,
                pattern,
            } => clauses.push(PredicateClause::Match(*expression, pattern.clone())),
            _ => clauses.push(PredicateClause::Bool(id)),
        }
    }

    fn pattern_bool(&mut self, id: celox_sv_syntax::ExprId) -> Converted<(Vec<Stmt>, Expr)> {
        let outer = std::mem::take(&mut self.pattern_prelude);
        let mut setup = self.effects(&[id])?;
        self.prepare_pattern_values(id)?;
        let value = self.native_chandle_value(id)?;
        setup.extend(std::mem::replace(&mut self.pattern_prelude, outer));
        let value = statements::simplify_condition(&self.tree, self.session, value, &self.dims);
        Ok((setup, value))
    }

    /// Evaluate `clauses` from left to right (12.6.2): a sequential
    /// conjunction, so a clause's scrutinee, effects and bindings run
    /// only when every clause before it succeeded. The statements set a
    /// hidden verdict variable to each clause's value in turn; the
    /// verdict is the condition. Pattern variables are declared in the
    /// current scope, which the caller keeps for whatever the predicate
    /// guards.
    pub(super) fn eval_predicate_clauses(
        &mut self,
        clauses: &[PredicateClause],
    ) -> Converted<(Vec<Stmt>, Option<Expr>)> {
        if clauses.len() == 1 {
            // One clause needs no flag: its condition is the predicate's.
            return match &clauses[0] {
                PredicateClause::Bool(id) => {
                    let (statements, value) = self.pattern_bool(*id)?;
                    Ok((statements, Some(value)))
                }
                PredicateClause::Match(scrutinee, pattern) => {
                    let (setup, value, layout) = self.pattern_scrutinee(*scrutinee)?;
                    let compiled =
                        self.compile_pattern(pattern, &layout, &value, CaseKind::Exact)?;
                    let mut statements = setup;
                    statements.extend(self.declare_pattern_bindings(&compiled.bindings)?);
                    Ok((statements, compiled.condition))
                }
            };
        }
        // Convert in source order, so each clause sees the pattern
        // variables the clauses before it declared, then nest the parts
        // from the last clause in.
        let mut parts = Vec::new();
        for clause in clauses {
            parts.push(match clause {
                PredicateClause::Bool(id) => {
                    let (setup, value) = self.pattern_bool(*id)?;
                    let truth = Expr::Unary {
                        op: UnaryOp::RedOr,
                        expr: Box::new(value),
                    };
                    (setup, Some(truth), Vec::new())
                }
                PredicateClause::Match(scrutinee, pattern) => {
                    let (setup, value, layout) = self.pattern_scrutinee(*scrutinee)?;
                    let compiled =
                        self.compile_pattern(pattern, &layout, &value, CaseKind::Exact)?;
                    let bindings = self.declare_pattern_bindings(&compiled.bindings)?;
                    (setup, compiled.condition, bindings)
                }
            });
        }
        // The verdict keeps a Boolean clause's ambiguous value, which a
        // conditional expression merges on (11.4.11); only a determined
        // true lets the next clause run (12.6.2).
        let verdict = self.hidden(Type::new(TypeKind::Logic)).1;
        let mut block = Vec::new();
        for (setup, condition, bindings) in parts.into_iter().rev() {
            let mut then_body = bindings;
            then_body.extend(block);
            block = setup;
            block.push(Stmt::Assign {
                lhs: LValue::Ident(verdict.clone()),
                rhs: condition.unwrap_or_else(|| Expr::Literal("1'b1".into())),
                nonblocking: false,
            });
            block.push(Stmt::If {
                condition: Expr::Binary {
                    left: Box::new(Expr::Ident(verdict.clone())),
                    op: BinaryOp::EqCase,
                    right: Box::new(Expr::Literal("1'b1".into())),
                },
                then_body,
                else_body: Vec::new(),
            });
        }
        Ok((block, Some(Expr::Ident(verdict))))
    }
}

impl<'a, 'b, 'c> BodyBuilder<'a, 'b, 'c> {
    /// Prepare a conditional predicate's bindings and control flow. The
    /// resulting mux stays in the containing expression so the ordinary
    /// lowering applies its context to both arms (IEEE 1800-2023 11.8).
    pub(super) fn pattern_value(&mut self, id: celox_sv_syntax::ExprId) -> Converted<Option<Expr>> {
        let native = self.syntax().declarations();
        let mut root = id;
        while let ExprKind::Group(inner) = native.expression(root).kind {
            root = inner;
        }
        if let Some(result) = self.pattern_results.get(&root) {
            return Ok(Some(result.clone()));
        }
        let (clauses, arms) = match &native.expression(root).kind {
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => {
                let mut clauses = Vec::new();
                self.predicate_clauses(*condition, &mut clauses);
                if clauses.len() == 1 && matches!(clauses[0], PredicateClause::Bool(_)) {
                    return Ok(None);
                }
                (clauses, Some((*then_expression, *else_expression)))
            }
            ExprKind::PatternMatch {
                expression,
                pattern,
            } => (
                vec![PredicateClause::Match(*expression, pattern.clone())],
                None,
            ),
            _ => return Ok(None),
        };
        self.push_scope();
        let (setup, condition) = self.eval_predicate_clauses(&clauses)?;
        // The setup runs only where an enclosing conditional selects
        // the arm this value is in.
        let guarded = self.guarded(setup);
        self.pattern_prelude.extend(guarded);
        let condition = condition.unwrap_or_else(|| Expr::Literal("1'b1".into()));
        let value = match arms {
            Some((then_expression, else_expression)) => {
                // A pattern value in an arm runs its setup only where
                // this conditional selects that arm.
                // An ambiguous condition merges both arms (11.4.11):
                // the then setup runs unless the condition is
                // definitely false, the else setup unless definitely
                // true.
                let then_value =
                    self.pattern_conditional_arm(then_expression, &condition, "1'b0")?;
                self.pop_scope();
                let else_value =
                    self.pattern_conditional_arm(else_expression, &condition, "1'b1")?;
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_value),
                    else_expr: Box::new(else_value),
                }
            }
            None => {
                self.pop_scope();
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(Expr::Literal("1'b1".into())),
                    else_expr: Box::new(Expr::Literal("1'b0".into())),
                }
            }
        };
        self.pattern_results.insert(root, value.clone());
        Arc::make_mut(&mut self.dims.effect_values).insert(root, value.clone());
        Ok(Some(value))
    }

    fn pattern_conditional_arm(
        &mut self,
        id: celox_sv_syntax::ExprId,
        condition: &Expr,
        skip: &str,
    ) -> Converted<Expr> {
        self.pattern_guards.push(Expr::Binary {
            left: Box::new(condition.clone()),
            op: BinaryOp::NeCase,
            right: Box::new(Expr::Literal(skip.into())),
        });
        let outer = std::mem::take(&mut self.pattern_prelude);
        let effects = self.effects(&[id])?;
        let effects = self.guarded(effects);
        let value = self.native_value(id);
        self.pattern_guards.pop();
        let prelude = std::mem::replace(&mut self.pattern_prelude, outer);
        self.pattern_prelude.extend(effects);
        self.pattern_prelude.extend(prelude);
        value
    }

    /// `statements` wrapped to run only where every enclosing
    /// conditional guard selects them.
    fn guarded(&self, statements: Vec<Stmt>) -> Vec<Stmt> {
        if statements.is_empty() || self.pattern_guards.is_empty() {
            return statements;
        }
        let condition = self
            .pattern_guards
            .iter()
            .cloned()
            .reduce(|left, right| Expr::Binary {
                left: Box::new(left),
                op: BinaryOp::LogicAnd,
                right: Box::new(right),
            })
            .expect("a nonempty guard list");
        vec![Stmt::If {
            condition,
            then_body: statements,
            else_body: Vec::new(),
        }]
    }

    /// Prepare every pattern predicate in the expression tree of `root`
    /// for conversion (see [`Self::pattern_value`]), so a nested one is
    /// substituted wherever the conversion reaches it. A pattern in an
    /// arm of an ordinary conditional keeps that conditional's lazy
    /// evaluation: its setup is guarded by the arm's selection.
    pub(super) fn prepare_pattern_values(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Converted<()> {
        self.prepare_pattern_node(root)
    }

    fn prepare_pattern_node(&mut self, id: celox_sv_syntax::ExprId) -> Converted<()> {
        let native = self.syntax().declarations();
        if self.dims.effect_values.contains_key(&id) {
            return Ok(());
        }
        let kind = &native.expression(id).kind;
        match kind {
            ExprKind::PatternMatch { .. } => {
                self.pattern_value(id)?;
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => {
                let (condition, then_expression, else_expression) =
                    (*condition, *then_expression, *else_expression);
                if self.predicate_is_patterned(condition) {
                    self.pattern_value(id)?;
                    return Ok(());
                }
                self.prepare_pattern_node(condition)?;
                if self.condition_has_pattern(then_expression)
                    || self.condition_has_pattern(else_expression)
                {
                    let decided = self.save_pattern_guard(condition)?;
                    self.prepare_pattern_arm(then_expression, &decided, "1'b0")?;
                    self.prepare_pattern_arm(else_expression, &decided, "1'b1")?;
                }
            }
            ExprKind::Binary {
                left,
                operator,
                right,
            } if matches!(operator.as_str(), "&&" | "||" | "->") => {
                let (left, right) = (*left, *right);
                self.prepare_pattern_node(left)?;
                if self.condition_has_pattern(right) {
                    let skip = if operator == "||" { "1'b1" } else { "1'b0" };
                    let decided = self.save_pattern_guard(left)?;
                    self.prepare_pattern_arm(right, &decided, skip)?;
                }
            }
            kind => {
                if let Some(operand) = super::effects::unevaluated_argument(native, kind)
                    && self.condition_has_pattern(operand)
                {
                    // Type queries still need the scoped variable bindings
                    // to bind their operand. Discard all executable setup.
                    let outer = std::mem::take(&mut self.pattern_prelude);
                    let prepared = self.prepare_pattern_node(operand);
                    self.pattern_prelude = outer;
                    prepared?;
                }
                for operand in super::effects::evaluated_operands(native, kind) {
                    self.prepare_pattern_node(operand)?;
                }
            }
        }
        Ok(())
    }

    // Save the four-state truth value once for both the prelude guard and the
    // final expression. The condition may call a function or be a wide value.
    fn save_pattern_guard(&mut self, id: celox_sv_syntax::ExprId) -> Converted<String> {
        if let Some(decided) = self.pattern_truth.get(&id) {
            return Ok(decided.clone());
        }
        let mut setup = Vec::new();
        let decided = self.truth(id, &mut setup)?;
        self.pattern_truth.insert(id, decided.clone());
        let setup = self.guarded(setup);
        self.pattern_prelude.extend(setup);
        Ok(decided)
    }

    fn prepare_pattern_arm(
        &mut self,
        id: celox_sv_syntax::ExprId,
        decided: &str,
        skip: &str,
    ) -> Converted<()> {
        self.pattern_guards.push(Expr::Binary {
            left: Box::new(Expr::Ident(decided.into())),
            op: BinaryOp::NeCase,
            right: Box::new(Expr::Literal(skip.into())),
        });
        let result = self.prepare_pattern_node(id);
        self.pattern_guards.pop();
        result
    }

    /// A `wait` whose condition is a pattern predicate over pure
    /// scrutinees (IEEE 1800-2023 9.4.3, 12.6): the wait tests the
    /// compiled conditions, which read the scrutinees directly, so a
    /// change re-evaluates them; the bindings run when it holds. Any
    /// other shape returns `None`.
    pub(super) fn pattern_wait(
        &mut self,
        condition_id: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.syntax().declarations();
        let mut clauses = Vec::new();
        self.predicate_clauses(condition_id, &mut clauses);
        if !clauses
            .iter()
            .any(|clause| matches!(clause, PredicateClause::Match(..)))
        {
            return Ok(None);
        }
        for clause in &clauses {
            let id = match clause {
                PredicateClause::Bool(id) => *id,
                PredicateClause::Match(scrutinee, _) => *scrutinee,
            };
            if self.condition_has_pattern(id) || super::effects::has_updates(native, &[id]) {
                return Ok(None);
            }
        }
        // The fast path may discover a call after binding an earlier
        // clause. Roll back those bindings before lowering the fallback.
        let local_start = self.state.locals.len();
        self.push_scope();
        let result = self.direct_pattern_wait(&clauses);
        if matches!(&result, Ok(Some(_))) {
            let scope = self.names.scopes.pop().expect("a pattern wait scope");
            self.names
                .scopes
                .last_mut()
                .expect("an enclosing wait scope")
                .entries
                .extend(scope.entries);
        } else {
            self.pop_scope();
            self.state.locals.truncate(local_start);
        }
        result
    }

    fn direct_pattern_wait(&mut self, clauses: &[PredicateClause]) -> Converted<Option<Vec<Stmt>>> {
        let mut conditions = Vec::new();
        let mut bindings = Vec::new();
        // A clause that reads a variable an earlier clause binds tests
        // the binding's value, so every re-evaluation of the wait
        // condition recomputes it from the scrutinees (9.4.3).
        let mut substitutions: Vec<(String, Expr)> = Vec::new();
        let mut seen = HashSet::default();
        for clause in clauses {
            match clause {
                PredicateClause::Bool(id) => {
                    let value = expression_binding::bind_chandle_value(
                        *id,
                        self.syntax().declarations(),
                        &self.tree,
                        self.session,
                        &self.dims,
                        None,
                    )?;
                    let mut value =
                        statements::simplify_condition(&self.tree, self.session, value, &self.dims);
                    self.rename_expr(&mut value);
                    if inlining::calls_subroutine(&value) {
                        return Ok(None);
                    }
                    substitute_pattern_bindings(&mut value, &substitutions);
                    conditions.push(value);
                }
                PredicateClause::Match(scrutinee, pattern) => {
                    let ty = self.scrutinee_type(*scrutinee)?;
                    let layout = self.pattern_layout(&ty)?;
                    let mut value = self.native_value(*scrutinee)?;
                    if inlining::calls_subroutine(&value) {
                        return Ok(None);
                    }
                    substitute_pattern_bindings(&mut value, &substitutions);
                    let compiled =
                        self.compile_pattern(pattern, &layout, &value, CaseKind::Exact)?;
                    if let Some(condition) = compiled.condition {
                        conditions.push(condition);
                    }
                    for binding in &compiled.bindings {
                        if !seen.insert(binding.name.clone()) {
                            return Err(unsupported(format!(
                                "pattern identifier `{}` is bound twice",
                                binding.name
                            )));
                        }
                        let unique = self.declare(&binding.name, binding.ty.clone());
                        let value = Expr::Resize {
                            expr: Box::new(binding.value.clone()),
                            width: binding.width,
                            signed: binding.signed,
                        };
                        substitutions.push((unique.clone(), value.clone()));
                        bindings.push(Stmt::Assign {
                            lhs: LValue::Ident(unique),
                            rhs: value,
                            nonblocking: false,
                        });
                    }
                }
            }
        }
        let condition = conditions
            .into_iter()
            .reduce(|left, right| Expr::Binary {
                left: Box::new(left),
                op: BinaryOp::LogicAnd,
                right: Box::new(right),
            })
            .unwrap_or_else(|| Expr::Literal("1'b1".into()));
        if events::reads_triggered(&condition) {
            return Err(unsupported(
                "`triggered` in a wait condition with other operands",
            ));
        }
        let mut statements = vec![Stmt::Wait(condition)];
        statements.extend(bindings);
        Ok(Some(statements))
    }

    /// Whether expression `id` contains a pattern predicate: a
    /// `matches`, or a conditional whose predicate has one.
    pub(super) fn condition_has_pattern(&self, id: celox_sv_syntax::ExprId) -> bool {
        let native = self.syntax().declarations();
        match &native.expression(id).kind {
            ExprKind::PatternMatch { .. } => true,
            ExprKind::Binary { operator, .. } if operator == "&&&" => true,
            kind => super::effects::operands(kind)
                .iter()
                .any(|operand| self.condition_has_pattern(*operand)),
        }
    }

    /// Whether an update inside expression `id` is already bound by an
    /// earlier effects pass over a containing expression, so the
    /// scrutinee's own effects must not execute it again.
    pub(super) fn scrutinee_effects_bound(&self, id: celox_sv_syntax::ExprId) -> bool {
        if self.dims.effect_values.contains_key(&id) {
            return true;
        }
        let native = self.syntax().declarations();
        let mut constants = Vec::new();
        let mut operands = Vec::new();
        match &native.expression(id).kind {
            ExprKind::PatternMatch {
                expression,
                pattern,
            } => {
                operands.push(*expression);
                pattern.constants(&mut constants);
            }
            kind => operands.extend(super::effects::operands(kind)),
        }
        operands.extend(constants);
        operands
            .iter()
            .any(|operand| self.scrutinee_effects_bound(*operand))
    }
}

/// Replace each pattern variable in `expr` by the value its binding
/// assigns, so a wait condition recomputes it from the scrutinees.
fn substitute_pattern_bindings(expr: &mut Expr, substitutions: &[(String, Expr)]) {
    if let Expr::Ident(name) = expr
        && let Some((_, value)) = substitutions.iter().find(|(bound, _)| bound == name)
    {
        *expr = value.clone();
        return;
    }
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => {}
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            substitute_pattern_bindings(expr, substitutions)
        }
        Expr::Concat(parts) => {
            for part in parts {
                substitute_pattern_bindings(part, substitutions);
            }
        }
        Expr::RepeatConcat { parts, .. } => {
            for part in parts {
                substitute_pattern_bindings(part, substitutions);
            }
        }
        Expr::Binary { left, right, .. } => {
            substitute_pattern_bindings(left, substitutions);
            substitute_pattern_bindings(right, substitutions);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            substitute_pattern_bindings(condition, substitutions);
            substitute_pattern_bindings(then_expr, substitutions);
            substitute_pattern_bindings(else_expr, substitutions);
        }
        Expr::Call { args, .. } => {
            for arg in args.iter_mut().flatten() {
                substitute_pattern_bindings(arg, substitutions);
            }
        }
        Expr::Inside { expr, items } => {
            substitute_pattern_bindings(expr, substitutions);
            for item in items {
                match item {
                    InsideItem::Value(value) => substitute_pattern_bindings(value, substitutions),
                    InsideItem::Range { low, high } => {
                        substitute_pattern_bindings(low, substitutions);
                        substitute_pattern_bindings(high, substitutions);
                    }
                }
            }
        }
    }
}

/// The variables `statements` read but do not write, in first-read
/// order: what a wait on their computation waits on.
pub(super) fn pattern_read_names(statements: &[Stmt], condition: &Expr) -> Vec<String> {
    fn expr_names(expr: &Expr, names: &mut Vec<String>) {
        let mut push = |name: &String| {
            if !names.contains(name) {
                names.push(name.clone());
            }
        };
        match expr {
            Expr::Ident(name) => push(name),
            Expr::Literal(_) => {}
            Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
                expr_names(expr, names)
            }
            Expr::Concat(parts) => {
                for part in parts {
                    expr_names(part, names);
                }
            }
            Expr::RepeatConcat { parts, .. } => {
                for part in parts {
                    expr_names(part, names);
                }
            }
            Expr::Binary { left, right, .. } => {
                expr_names(left, names);
                expr_names(right, names);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expr_names(condition, names);
                expr_names(then_expr, names);
                expr_names(else_expr, names);
            }
            Expr::Call { args, .. } => {
                for arg in args.iter().flatten() {
                    expr_names(arg, names);
                }
            }
            Expr::Inside { expr, items } => {
                expr_names(expr, names);
                for item in items {
                    match item {
                        InsideItem::Value(value) => expr_names(value, names),
                        InsideItem::Range { low, high } => {
                            expr_names(low, names);
                            expr_names(high, names);
                        }
                    }
                }
            }
        }
    }
    let mut written = Vec::new();
    let mut read = Vec::new();
    for statement in statements {
        statement.walk(&mut |statement| match statement {
            Stmt::Assign { lhs, rhs, .. } => {
                if let LValue::Ident(name) = lhs
                    && !written.contains(name)
                {
                    written.push(name.clone());
                }
                expr_names(rhs, &mut read);
            }
            Stmt::If { condition, .. } | Stmt::Wait(condition) | Stmt::Eval(condition) => {
                expr_names(condition, &mut read)
            }
            Stmt::Local {
                init: Some(init), ..
            } => expr_names(init, &mut read),
            _ => {}
        });
    }
    expr_names(condition, &mut read);
    read.retain(|name| !written.contains(name));
    read
}

/// One clause of a conditional predicate (IEEE 1800-2023 12.6.2).
pub(super) enum PredicateClause {
    Bool(celox_sv_syntax::ExprId),
    Match(celox_sv_syntax::ExprId, Pattern),
}
