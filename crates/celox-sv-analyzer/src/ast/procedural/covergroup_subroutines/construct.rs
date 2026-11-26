//! `N::$new` of a covergroup type (IEEE 1800-2023 19.3, 19.5, 19.7), and
//! with it the descriptor of the type.
//!
//! It evaluates what the definition reads when an instance is constructed,
//! the values of its options, the operands of its bins that are no
//! constants and the arrays that are the values of bins, makes the
//! instance, hands it the arguments its samples read and what it evaluated,
//! answers the `with` filters of its bins for each of their values, and
//! builds its bins.

use super::*;
use crate::ast::covergroups::{ITEM, Level, OptionType};
use celox_sv_syntax::{BinsArray, CoverageItem, CoverageOption, CoverpointItem};
use system_functions::{ContainerKind, ContainerOp};

/// What `N::$new` evaluates and hands to the heap before it builds the
/// instance, as the options and bins of the definition are compiled.
struct Construction<'x> {
    site: &'x Site<'x>,
    /// The variable of the handle of the instance.
    instance: String,
    inputs: Vec<heap::InputDescriptor>,
    /// The evaluation of the inputs into variables, in authored order.
    evaluate: Vec<Stmt>,
    /// The calls that hand the variables over.
    hand_over: Vec<Stmt>,
    options: Vec<heap::OptionInit>,
    /// The element types of the arrays that are the values of bins
    /// (19.5.1.2), by set index.
    sets: Vec<heap::ValueType>,
    /// The `with` filters a construction answers (19.5.1.1), by filter
    /// index, and the loops that answer them, which follow every call of
    /// `hand_over`: the heap takes what was handed over as what the bins
    /// of the instance depend on when the first filter is asked.
    filters: Vec<heap::FilterDescriptor>,
    answer: Vec<Stmt>,
    /// What the `with` expressions read that is no constant, which is
    /// handed over as an input: an instance whose inputs are those of an
    /// earlier one takes its bins without asking its filters again.
    keyed: Vec<Expr>,
    /// Whether building an instance may warn of what the analysis could
    /// not warn of: a bin operand, an element of an array or
    /// `option.auto_bin_max` that is known only then (19.5.7), or a
    /// wildcard value that is matched as a pattern (19.5.4).
    warns: bool,
}

/// A bin declaration as `N::$new` compiles it.
struct Declared<'x> {
    /// The coverpoint of the bin, with the bins declared before it.
    point: &'x heap::PointDescriptor,
    /// The type of the variable of the coverpoint, which `item` has in a
    /// `with` expression (19.5.1.1).
    item: &'x Type,
    bits: BinBits,
    name: String,
    /// What messages call the bin.
    place: String,
}

impl Construction<'_> {
    fn cg(&self) -> Expr {
        Expr::Ident(self.instance.clone())
    }

    /// Integral input `value` of `ty`, evaluated into a variable of `local`.
    fn integral_input(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        mut value: Expr,
        ty: heap::ValueType,
        local: Type,
    ) -> u32 {
        builder.rename_expr(&mut value);
        self.renamed_input(builder, value, ty, local)
    }

    /// [`Self::integral_input`] of a value that names the variables of the
    /// body as the body names them.
    fn renamed_input(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        value: Expr,
        ty: heap::ValueType,
        local: Type,
    ) -> u32 {
        let index = self.inputs.len() as u32;
        self.inputs.push(heap::InputDescriptor::Integral(ty));
        let local = builder.declare(&format!("$input{index}"), local);
        self.evaluate.push(assign(&local, value));
        self.hand_over.push(Stmt::Eval(coverage(
            CoverageOp::Input {
                index,
                width: ty.width,
                signed: ty.signed,
            },
            vec![self.cg(), Expr::Ident(local)],
        )));
        index
    }

    /// The value of the string option that `value` assigns: its text where
    /// it is a string literal, else a string input.
    fn text(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        value: ExprId,
    ) -> Converted<heap::OptionValue> {
        let native = self.site.tree.declarations();
        let ty = randomization::string_type();
        let literal = match &native
            .expression(expression_binding::ungroup(value, native))
            .kind
        {
            ExprKind::Literal(text) => expression_binding::string_literal_bytes(text)
                .and_then(|bytes| String::from_utf8(bytes).ok()),
            _ => None,
        };
        if let Some(text) = literal {
            return Ok(heap::OptionValue::String(text));
        }
        let value = builder.covergroup_value(value, Some(&ty))?;
        Ok(heap::OptionValue::StringInput(
            self.string_input(builder, value),
        ))
    }

    /// String input `value`, evaluated into a variable.
    fn string_input(&mut self, builder: &mut BodyBuilder<'_, '_, '_>, mut value: Expr) -> u32 {
        builder.rename_expr(&mut value);
        let index = self.inputs.len() as u32;
        self.inputs.push(heap::InputDescriptor::String);
        let local = builder.declare(&format!("$input{index}"), randomization::string_type());
        self.evaluate.push(assign(&local, value));
        self.hand_over.push(Stmt::Eval(coverage(
            CoverageOp::InputString { index },
            vec![self.cg(), Expr::Ident(local)],
        )));
        index
    }

    /// `option.member = value` of item `item` of the definition (19.7): an
    /// ordinary expression of the scope of `new`, assigned to a variable of
    /// the type of the member (19.10) as a declaration assigns its
    /// initializer.
    fn option(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        item: u32,
        option: &CoverageOption,
        level: Level,
    ) -> Converted<()> {
        let model = self.site.model;
        let (id, kind) = covergroups::option_member(option, level, &model.display)??;
        let value = match kind {
            OptionType::Text => self.text(builder, option.value)?,
            OptionType::Int | OptionType::Bit => {
                let (ty, value_type) = if kind == OptionType::Int {
                    (
                        int_type(),
                        heap::ValueType {
                            width: 32,
                            signed: true,
                        },
                    )
                } else {
                    (
                        packed_type(1, false, false),
                        heap::ValueType {
                            width: 1,
                            signed: false,
                        },
                    )
                };
                let value = builder.covergroup_value(option.value, Some(&ty))?;
                let folded = builder.covergroup_constant(&value);
                // The heap warns where it makes one automatic bin for a
                // maximum below 1.
                if id == heap::OptionId::AutoBinMax
                    && folded.as_ref().is_none_or(|literal| {
                        crate::typecheck::integral_literal_as_i128(literal, literal.signed)
                            .is_none_or(|value| value <= 0)
                    })
                {
                    self.warns = true;
                }
                heap::OptionValue::Operand(match folded {
                    Some(literal) => heap::Operand::Constant(constant(literal, None)),
                    None => {
                        heap::Operand::Input(self.integral_input(builder, value, value_type, ty))
                    }
                })
            }
        };
        self.options.push(heap::OptionInit {
            item,
            option: id,
            value,
        });
        Ok(())
    }

    /// Check what operand `root` of a bin names (19.5): as variables only
    /// constants, arguments of the covergroup and, in an embedded
    /// covergroup, constants of its class; of system functions only those
    /// whose call is a constant. In a `with` expression, which `item`
    /// tells, `item` is the value the filter is asked about (19.5.1.1).
    /// `place` names the bin. The result is what the operand reads: the
    /// names that are not constants of the scope, each with the members
    /// selected of it.
    fn check_operand(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        root: ExprId,
        place: &str,
        item: bool,
    ) -> Converted<Vec<ExprId>> {
        let model = self.site.model;
        let native = self.site.tree.declarations();
        // The system function calls that are constants: what they are
        // called with is not looked at, as a variable in `$bits(v)` is not
        // read.
        let mut constant_calls = Vec::new();
        // What the calls call, which names no variable, and the member
        // selects that are read: the walk of the names below does not see
        // the member of one.
        let mut callees = Vec::new();
        let mut members = Vec::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let kind = &native.expression(id).kind;
            // A system function, called with or without parentheses.
            let system = match kind {
                ExprKind::Call { expression, .. } => match &native.expression(*expression).kind {
                    ExprKind::Name(name) => Some(name),
                    _ => None,
                },
                ExprKind::Name(name) => Some(name),
                _ => None,
            }
            .filter(|name| name.spelling.starts_with('$'));
            if let Some(name) = system {
                let folds = builder
                    .covergroup_value(id, None)
                    .ok()
                    .is_some_and(|value| {
                        matches!(value, Expr::Literal(_))
                            || builder.covergroup_constant(&value).is_some()
                    });
                if !folds {
                    return Err(invalid(format!(
                        "the call of `{}` in {place} is not a constant system function call (IEEE 1800-2023 19.5)",
                        name.spelling
                    )));
                }
                constant_calls.push(native.expression(id).tokens);
                continue;
            }
            match kind {
                ExprKind::Call { expression, .. } => callees.push(*expression),
                ExprKind::Member { .. } if !callees.contains(&id) => members.push(id),
                _ => {}
            }
            kind.map_expressions(|child| {
                pending.push(child);
                child
            });
        }
        for id in &members {
            self.check_member(builder, *id, place)?;
        }
        // `id` with the members that are selected of it and read.
        let selected = |mut whole: ExprId| {
            while let Some(select) = members.iter().copied().find(|select| {
                matches!(
                    &native.expression(*select).kind,
                    ExprKind::Member { expression, separator, .. }
                        if separator == "."
                            && expression_binding::ungroup(*expression, native) == whole
                )
            }) {
                whole = select;
            }
            whole
        };
        let mut reads = Vec::new();
        for (name, id) in covergroups::variable_names(native, root) {
            let at = native.expression(id).tokens;
            if constant_calls
                .iter()
                .any(|call| call.start <= at.start && at.end <= call.end)
                || item && name == ITEM
            {
                continue;
            }
            if matches!(name.as_str(), "this" | "super")
                || model.new_formals.iter().any(|formal| formal.name == name)
            {
                reads.push(selected(id));
                continue;
            }
            // A constant of the class of an embedded covergroup (19.4,
            // 8.19), or a static member of it. A member of unpacked
            // structure type is not told apart from a constant either:
            // its members are properties of their own.
            if let Some(class) = self.site.class
                && (builder.dims.class_static_members.contains(&name)
                    || builder.dims.get(&name).is_none()
                        && (class
                            .property(&name)
                            .is_some_and(|property| property.constant)
                            || class.aggregate(&name).is_some()))
            {
                reads.push(selected(id));
                continue;
            }
            if declared_constant(&builder.dims, &name) {
                continue;
            }
            // Whatever else names a constant binds to its value. A name
            // that is no value by itself, such as that of an instance, is
            // read with the members selected of it (`u.v`). What does not
            // bind as a value at all, such as the name of a type, is no
            // variable.
            let mut whole = id;
            let mut value = builder.covergroup_value(whole, None);
            while value.is_err()
                && let Some(select) = members.iter().copied().find(|select| {
                    matches!(
                        &native.expression(*select).kind,
                        ExprKind::Member { expression, separator, .. }
                            if separator == "."
                                && expression_binding::ungroup(*expression, native) == whole
                    )
                })
            {
                whole = select;
                value = builder.covergroup_value(whole, None);
            }
            let Ok(value) = value else {
                continue;
            };
            if matches!(value, Expr::Literal(_)) || builder.covergroup_constant(&value).is_some() {
                continue;
            }
            return Err(invalid(format!(
                "`{name}` in {place} is neither a constant nor an argument of the covergroup (IEEE 1800-2023 19.5)"
            )));
        }
        Ok(reads)
    }

    /// Check the functions that `value` of a bin calls: one only reads
    /// its arguments (19.5). `place` names the bin.
    fn check_calls(
        &self,
        builder: &BodyBuilder<'_, '_, '_>,
        value: &Expr,
        place: &str,
    ) -> Converted<()> {
        let mut calls = Vec::new();
        subroutine_calls(value, &mut calls);
        let writes = |callee: &String| {
            builder
                .dims
                .subroutine_directions
                .get(callee)
                .is_some_and(|directions| {
                    directions.iter().any(|direction| {
                        matches!(
                            direction,
                            ParamDirection::Output | ParamDirection::Inout | ParamDirection::Ref
                        )
                    })
                })
        };
        if calls.iter().any(writes) {
            return Err(invalid(format!(
                "the function called in {place} has an output, inout or ref argument (IEEE 1800-2023 19.5)"
            )));
        }
        Ok(())
    }

    /// Check member select `id` in an operand of a bin, which no call
    /// calls (19.5): `scope::name` names a constant, and a member of the
    /// object of an embedded covergroup, `this.name` or `super.name`, a
    /// constant of its class. A static property of a class is not told
    /// apart from a constant. The member of anything else is read through
    /// a name, which is checked as the names of the operand are. `place`
    /// names the bin.
    fn check_member(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        id: ExprId,
        place: &str,
    ) -> Converted<()> {
        let native = self.site.tree.declarations();
        let ExprKind::Member {
            expression,
            separator,
            name,
        } = &native.expression(id).kind
        else {
            return Ok(());
        };
        if expression_binding::objects::static_property(
            &builder.tree,
            builder.session,
            id,
            native,
            &builder.dims,
        )?
        .is_some()
        {
            return Ok(());
        }
        let member = instances::normalize_identifier(&name.spelling);
        let scope = match &native
            .expression(expression_binding::ungroup(*expression, native))
            .kind
        {
            ExprKind::Name(scope) => Some(scope.spelling.as_str()),
            _ => None,
        };
        let variable = match (separator.as_str(), scope) {
            // A constant of a package, as the name of one of the scope is.
            ("::", Some(scope))
                if declared_constant(
                    &builder.dims,
                    &scope::qualified_name(
                        &builder.tree,
                        &instances::normalize_identifier(scope),
                        &member,
                    ),
                ) =>
            {
                false
            }
            // A function may be called without parentheses.
            ("::", _) => builder
                .covergroup_value(id, None)
                .ok()
                .is_some_and(|value| {
                    !matches!(
                        value,
                        Expr::Literal(_)
                            | Expr::Call {
                                callee: Callee::Subroutine(_),
                                ..
                            }
                    ) && builder.covergroup_constant(&value).is_none()
                }),
            (".", Some("this" | "super")) => self.site.class.is_some_and(|class| {
                class
                    .property(&member)
                    .is_some_and(|property| !property.constant)
            }),
            _ => false,
        };
        if !variable {
            return Ok(());
        }
        let scope = scope.map_or_else(String::new, |scope| format!("{scope}{separator}"));
        Err(invalid(format!(
            "`{scope}{member}` in {place} is neither a constant nor an argument of the covergroup (IEEE 1800-2023 19.5)"
        )))
    }

    /// Operand `root` of a bin of a coverpoint (19.5.1): a constant where
    /// it folds, else an input that `new` evaluates, a call of a function
    /// among it. `place` names the bin.
    fn operand(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        root: ExprId,
        place: &str,
    ) -> Converted<heap::Operand> {
        self.check_operand(builder, root, place, false)?;
        let ty = builder.covergroup_operand_type(root)?;
        let value = builder.covergroup_value(root, None)?;
        self.check_calls(builder, &value, place)?;
        Ok(match builder.covergroup_constant(&value) {
            Some(literal) => heap::Operand::Constant(constant(literal, Some(ty))),
            None => heap::Operand::Input(self.integral_input(
                builder,
                value,
                ty,
                packed_type(ty.width, ty.signed, true),
            )),
        })
    }

    /// The bounds of a tolerance range of a bin, `[center +/- tolerance]`
    /// or `[center +%- tolerance]` if `relative` (11.4.13): constants where
    /// both operands fold, else inputs that `new` computes. `place` names
    /// the bin.
    fn tolerance(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        center: ExprId,
        tolerance: ExprId,
        relative: bool,
        place: &str,
    ) -> Converted<(heap::Operand, heap::Operand)> {
        let integral = |error: AnalyzerError| match error {
            AnalyzerError::Unsupported(_) => unsupported(
                "covergroup bin range with a tolerance that is not integral (IEEE 1800-2023 19.5.1, 11.4.13)",
            ),
            error => error,
        };
        self.check_operand(builder, center, place, false)?;
        self.check_operand(builder, tolerance, place, false)?;
        let ty = builder.covergroup_operand_type(center).map_err(integral)?;
        let tolerance_type = builder
            .covergroup_operand_type(tolerance)
            .map_err(integral)?;
        let mut center_value = builder.covergroup_value(center, None)?;
        let mut amount = builder.covergroup_value(tolerance, None)?;
        self.check_calls(builder, &center_value, place)?;
        self.check_calls(builder, &amount, place)?;
        if let Some((low, high)) =
            builder.covergroup_tolerance_constants(center, tolerance, relative)
        {
            return Ok((heap::Operand::Constant(low), heap::Operand::Constant(high)));
        }
        // `new` computes the bounds in variables, whose types say how
        // each step extends and compares: the center and the tolerance in
        // the type of the center, which the bounds are of (11.4.13).
        builder.rename_expr(&mut center_value);
        builder.rename_expr(&mut amount);
        let local = packed_type(ty.width, ty.signed, true);
        let index = self.inputs.len();
        let mut variable = |this: &mut Self, name: &str, ty: Type, value: Expr| {
            let variable = builder.declare(&format!("${name}{index}"), ty);
            this.evaluate.push(assign(&variable, value));
            Expr::Ident(variable)
        };
        let binary = |left: &Expr, op: BinaryOp, right: &Expr| Expr::Binary {
            left: Box::new(left.clone()),
            op,
            right: Box::new(right.clone()),
        };
        let center_value = variable(self, "center", local.clone(), center_value);
        let amount = if relative {
            // `A * B / 100.0` cut to an integer: each operand, extended by
            // its own signedness as an assignment extends it, in a signed
            // type that holds the product, which is divided towards zero.
            let wide = packed_type((ty.width + tolerance_type.width + 2).max(16), true, true);
            let center_value = variable(self, "wide_center", wide.clone(), center_value.clone());
            let amount = variable(self, "wide_tolerance", wide.clone(), amount);
            let hundred = variable(self, "hundred", wide, Expr::Literal("100".into()));
            binary(
                &binary(&center_value, BinaryOp::Mul, &amount),
                BinaryOp::Div,
                &hundred,
            )
        } else {
            amount
        };
        let amount = variable(self, "tolerance", local.clone(), amount);
        let below = variable(
            self,
            "below",
            local.clone(),
            binary(&center_value, BinaryOp::Sub, &amount),
        );
        let above = variable(
            self,
            "above",
            local.clone(),
            binary(&center_value, BinaryOp::Add, &amount),
        );
        // "the bounds are always considered as [low_bound:high_bound],
        // swapping the two resulting values as necessary".
        let ordered = binary(&below, BinaryOp::Le, &above);
        let choose = |then_expr: &Expr, else_expr: &Expr| Expr::Mux {
            condition: Box::new(ordered.clone()),
            then_expr: Box::new(then_expr.clone()),
            else_expr: Box::new(else_expr.clone()),
        };
        self.warns = true;
        let low = self.renamed_input(builder, choose(&below, &above), ty, local.clone());
        let high = self.renamed_input(builder, choose(&above, &below), ty, local);
        Ok((heap::Operand::Input(low), heap::Operand::Input(high)))
    }

    /// Element `range` of the range list of a bin (19.5.1). `place` names
    /// the bin.
    fn range(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        range: &CoverageRange,
        place: &str,
    ) -> Converted<heap::RangeDescriptor> {
        // The construction resolves a value it alone knows, and warns of
        // one that takes no part (19.5.7).
        let mut operand = |this: &mut Self, root: ExprId| {
            let operand = this.operand(builder, root, place)?;
            this.warns |= matches!(operand, heap::Operand::Input(_));
            Ok::<_, AnalyzerError>(operand)
        };
        Ok(match &range.kind {
            CoverageRangeKind::Value(value) => heap::RangeDescriptor::Value(operand(self, *value)?),
            CoverageRangeKind::Range { low, high } => {
                let mut bound = |bound: &Option<ExprId>| {
                    Ok::<_, AnalyzerError>(match bound {
                        Some(bound) => heap::Bound::Operand(operand(self, *bound)?),
                        // `$`.
                        None => heap::Bound::Open,
                    })
                };
                heap::RangeDescriptor::Range {
                    low: bound(low)?,
                    high: bound(high)?,
                }
            }
            CoverageRangeKind::Tolerance {
                center,
                tolerance,
                relative,
            } => {
                let (low, high) = self.tolerance(builder, *center, *tolerance, *relative, place)?;
                heap::RangeDescriptor::Range {
                    low: heap::Bound::Operand(low),
                    high: heap::Bound::Operand(high),
                }
            }
        })
    }

    /// Add element `value` of the values of bin `name` of `point`, written
    /// at `tokens`, to `values`. What the standard has a tool warn of
    /// (19.5.7) is known here for constants: the value, or a part of the
    /// range, takes no part in the bin. The text of it joins `details`.
    /// `wildcard` is whether the declaration of the bin is `wildcard`.
    fn resolved(
        &mut self,
        point: &heap::PointDescriptor,
        wildcard: bool,
        value: heap::RangeDescriptor,
        values: &mut Vec<heap::RangeDescriptor>,
        details: &mut Vec<String>,
    ) {
        let Some(constants) = range_value(&value) else {
            values.push(value);
            return;
        };
        let warnings = heap::resolve_warnings(&point.ty, wildcard, constants);
        if warnings.is_empty() {
            values.push(value);
            return;
        }
        details.extend(warnings);
        // An x or z bit of a value of a wildcard bin stands for 0 and for
        // 1 (19.5.4): the value stays as it is written, and a construction
        // that warns of what only it knows warns of it again.
        let unknown = |operand: &heap::Operand| matches!(operand, heap::Operand::Constant(constant) if constant.mask.bits() != 0);
        let patterned = wildcard
            && match &value {
                heap::RangeDescriptor::Value(value) => unknown(value),
                heap::RangeDescriptor::Range { low, high } => [low, high].into_iter().any(
                    |bound| matches!(bound, heap::Bound::Operand(operand) if unknown(operand)),
                ),
            };
        if patterned {
            values.push(value);
            return;
        }
        // The descriptor has the values that take part in place of the
        // element: a construction, which warns of what only it knows, does
        // not warn of the element again.
        values.extend(
            heap::resolve(&point.ty, constants)
                .items
                .into_iter()
                .map(|item| taking_part(&point.ty, item)),
        );
    }

    /// Warn of what `details` say of the values of bin `name` of `point`,
    /// written at `tokens`, that take no part in it (19.5.7).
    fn warn(
        &self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        point: &heap::PointDescriptor,
        name: &str,
        details: Vec<String>,
        tokens: celox_sv_syntax::TokenRange,
    ) {
        if details.is_empty() {
            return;
        }
        warnings::append(
            &mut builder.session.diagnostics,
            [warnings::AnalyzerWarning::CovergroupBinValue {
                covergroup: self.site.model.display.clone(),
                item: point.name.clone(),
                bin: name.to_owned(),
                detail: details.join("; "),
                site: source_span(tokens, self.site.tree),
            }],
        );
    }

    /// The size of bin array `name[size]` (19.5.1): a positive number,
    /// which a construction checks where only it knows the value. `place`
    /// names the bin.
    fn size(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        size: ExprId,
        place: &str,
    ) -> Converted<heap::Operand> {
        // "a single positive integral expression".
        let native = self.site.tree.declarations();
        let integral =
            expression_types::type_of(&builder.tree, builder.session, size, native, &builder.dims)
                .is_ok_and(|ty| {
                    matches!(
                        builder.session.types.data(ty),
                        semantic_types::TyData::Packed(_)
                    )
                });
        if !integral {
            return Err(invalid(format!(
                "the number of bins of {place} is not an integral expression (IEEE 1800-2023 19.5.1)"
            )));
        }
        let operand = self.operand(builder, size, place)?;
        if let heap::Operand::Constant(constant) = &operand {
            let positive = constant.mask.bits() == 0
                && constant.value.bits() != 0
                && !(constant.ty.signed
                    && constant.ty.width != 0
                    && constant.value.bit(u64::from(constant.ty.width) - 1));
            if !positive {
                return Err(invalid(format!(
                    "the number of bins of {place} is not positive (IEEE 1800-2023 19.5.1)"
                )));
            }
        }
        Ok(operand)
    }

    /// The `with` expression `with` of a bin of `point` (19.5.1.1), bound
    /// with `item` as a variable of type `item_type`: the variable, the
    /// condition that keeps a value, and whether the values a construction
    /// hands over decide what the filter keeps. `place` names the bin.
    /// With `keyed`, what the expression reads beside `item` and constants
    /// is handed to the construction as inputs for that.
    fn with_expression(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        with: ExprId,
        item_type: &Type,
        place: &str,
        keyed: bool,
    ) -> Converted<(String, Expr, bool)> {
        let native = self.site.tree.declarations();
        builder.push_scope();
        let item = builder.declare(ITEM, item_type.clone());
        let bound = (|| {
            let reads = self.check_operand(builder, with, place, true)?;
            // The result is "assignment compatible with an integral type"
            // (A.2.11, footnote 31).
            let ty = expression_types::type_of(
                &builder.tree,
                builder.session,
                with,
                native,
                &builder.dims,
            )?;
            if !matches!(
                builder.session.types.data(ty),
                semantic_types::TyData::Packed(_)
                    | semantic_types::TyData::Real
                    | semantic_types::TyData::Shortreal
            ) {
                return Err(invalid(format!(
                    "the `with` expression of {place} is not of a type that is assignment compatible with an integral type (IEEE 1800-2023 19.5.1.1)"
                )));
            }
            let mut condition = builder.covergroup_value(with, None)?;
            self.check_calls(builder, &condition, place)?;
            builder.rename_expr(&mut condition);
            // What an object holds is not handed over: a filter that reads
            // one, as a method of it does, is asked by every construction.
            let mut shared = !self.reads_an_object(builder, with);
            if keyed && shared {
                for read in reads {
                    shared &= self.key(builder, read)?;
                }
            }
            Ok((condition, shared))
        })();
        builder.pop_scope();
        let (condition, shared) = bound?;
        Ok((item, condition, shared))
    }

    /// Whether `with` expression `root` reads an object, whose state the
    /// values a construction hands over do not tell: it has a handle among
    /// its operands, such as the object a method is called of or an
    /// argument of a call, or it calls a method of the class of an
    /// embedded covergroup that is not static, which reads the object
    /// that holds the covergroup (19.5 lets a function of a bin read
    /// "global and instance constants").
    fn reads_an_object(&self, builder: &mut BodyBuilder<'_, '_, '_>, root: ExprId) -> bool {
        let native = self.site.tree.declarations();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let kind = &native.expression(id).kind;
            if let ExprKind::Name(name) = kind
                && let Some(class) = self.site.class
                && class
                    .method(&instances::normalize_identifier(&name.spelling))
                    .is_some_and(|method| !method.qualifiers.is_static)
            {
                return true;
            }
            let handle = expression_types::type_of(
                &builder.tree,
                builder.session,
                id,
                native,
                &builder.dims,
            )
            .is_ok_and(|ty| {
                matches!(
                    builder.session.types.data(ty),
                    semantic_types::TyData::Class(_)
                        | semantic_types::TyData::Chandle
                        | semantic_types::TyData::VirtualInterface { .. }
                )
            });
            if handle {
                return true;
            }
            kind.map_expressions(|child| {
                pending.push(child);
                child
            });
        }
        false
    }

    /// Hand the value of `read`, which a `with` expression reads, to the
    /// construction as an input, unless it is a constant or handed over
    /// already: what the filter keeps depends on it. The result is whether
    /// the value is one a construction hands over, an integral value or a
    /// string.
    fn key(&mut self, builder: &mut BodyBuilder<'_, '_, '_>, read: ExprId) -> Converted<bool> {
        let native = self.site.tree.declarations();
        let Ok(value) = builder.covergroup_value(read, None) else {
            return Ok(true);
        };
        if matches!(value, Expr::Literal(_)) || builder.covergroup_constant(&value).is_some() {
            return Ok(true);
        }
        let mut renamed = value.clone();
        builder.rename_expr(&mut renamed);
        if self.keyed.contains(&renamed) {
            return Ok(true);
        }
        let ty =
            expression_types::type_of(&builder.tree, builder.session, read, native, &builder.dims)?;
        match builder.session.types.data(ty) {
            semantic_types::TyData::Packed(_) => {
                let ty = builder.covergroup_operand_type(read)?;
                self.integral_input(builder, value, ty, packed_type(ty.width, ty.signed, true));
            }
            semantic_types::TyData::String => {
                let value = expression_binding::string_operand(value, &builder.dims);
                self.string_input(builder, value);
            }
            _ => return Ok(false),
        }
        self.keyed.push(renamed);
        Ok(true)
    }

    /// The `with` filter `with` of bin declaration `bins`, `declared`
    /// (19.5.1.1). A construction asks it about each value of the bin; for
    /// a bin whose sample evaluates it instead, which its predicate bits
    /// tell, the expression is only checked here.
    fn filter(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        bins: &Bins,
        with: ExprId,
        declared: &Declared<'_>,
    ) -> Converted<heap::BinFilter> {
        let Declared {
            point,
            item: item_type,
            place,
            ..
        } = declared;
        if let Some(bit) = declared.bits.filter {
            self.with_expression(builder, with, item_type, place, false)?;
            return Ok(heap::BinFilter::Predicate(bit));
        }
        // A sample evaluates the filter of one `bins` bin of more values
        // than a construction is asked about; no other bin has that many.
        if !candidates(self.site, builder, bins, &point.ty).enumerable() {
            return Err(unsupported(format!(
                "covergroup with filter over more than {} candidate values on a bin array, `ignore_bins` or `illegal_bins`, or over a wildcard value of more than {} ranges of values (IEEE 1800-2023 19.5.1.1)",
                heap::MAX_FILTER_CANDIDATES,
                heap::MAX_WILDCARD_EXPANSION
            )));
        }
        let (item, condition, shared) =
            self.with_expression(builder, with, item_type, place, true)?;
        let filter = self.filters.len() as u32;
        let mut descriptor = heap::FilterDescriptor::new(vec![heap::ValueType {
            width: point.ty.width,
            signed: point.ty.signed,
        }]);
        descriptor.shared = shared;
        self.filters.push(descriptor);
        let bit = packed_type(1, false, false);
        let more = builder.declare(&format!("$more{filter}"), bit.clone());
        let keep = builder.declare(&format!("$keep{filter}"), bit);
        let next = |keep: Expr| coverage(CoverageOp::FilterNext { filter }, vec![self.cg(), keep]);
        self.answer.extend([
            assign(&more, next(Expr::Literal("1'b0".into()))),
            Stmt::Loop {
                kind: LoopKind::While,
                init: Vec::new(),
                condition: Some(Expr::Ident(more.clone())),
                step: Vec::new(),
                body: vec![
                    assign(
                        &item,
                        coverage(
                            CoverageOp::FilterItem {
                                filter,
                                width: point.ty.width,
                                signed: point.ty.signed,
                                four_state: point.ty.four_state,
                            },
                            vec![self.cg()],
                        ),
                    ),
                    // "for which the expression evaluates to true, as
                    // described in 12.4": an x or z keeps no value.
                    Stmt::If {
                        condition,
                        then_body: vec![assign(&keep, Expr::Literal("1'b1".into()))],
                        else_body: vec![assign(&keep, Expr::Literal("1'b0".into()))],
                    },
                    assign(&more, next(Expr::Ident(keep.clone()))),
                ],
            },
        ]);
        Ok(heap::BinFilter::Enumerate(filter))
    }

    /// The array `set` that is the values of bin declaration `bins`,
    /// `declared` (19.5.1.2): a dynamic array, a queue or an array of an
    /// object, whose elements `new` hands over, or an array parameter,
    /// whose elements are constants of the bin.
    fn set(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        bins: &Bins,
        set: ExprId,
        declared: &Declared<'_>,
    ) -> Converted<heap::BinValues> {
        let Declared {
            point, name, place, ..
        } = declared;
        let native = self.site.tree.declarations();
        let not_supported = || {
            unsupported(
                "covergroup bins of a set expression that is neither a dynamic array, a queue or an array of an object with integral elements of at most 64 bits nor an array parameter with integral elements (IEEE 1800-2023 19.5.1.2)",
            )
        };
        self.check_operand(builder, set, place, false)?;
        // "an expression yielding an array of values".
        if expression_types::type_of(&builder.tree, builder.session, set, native, &builder.dims)
            .is_ok_and(|ty| {
                !matches!(
                    builder.session.types.data(ty),
                    semantic_types::TyData::Array { .. }
                )
            })
        {
            return Err(invalid(format!(
                "the values of {place} are no array (IEEE 1800-2023 19.5.1.2)"
            )));
        }
        if let Some(container) =
            containers::native_type(set, native, &builder.tree, builder.session, &builder.dims)?
        {
            if matches!(container.layout.kind, ContainerKind::Associative { .. }) {
                return Err(invalid(format!(
                    "the values of {place} are an associative array (IEEE 1800-2023 19.5.1.2)"
                )));
            }
            if container.layout.handle.is_some()
                || container.record.is_some()
                || container.element.container().is_some()
                || container.layout.width > 64
            {
                return Err(not_supported());
            }
            let layout = container.layout.clone();
            let ty = heap::ValueType {
                width: layout.width,
                signed: layout.signed,
            };
            let mut value = builder.covergroup_value(set, None)?;
            self.check_calls(builder, &value, place)?;
            builder.rename_expr(&mut value);
            let index = self.sets.len() as u32;
            self.sets.push(ty);
            let array = builder.declare(
                &format!("$set{index}"),
                containers::of((*container).clone()),
            );
            let at = builder.declare(&format!("$element{index}"), int_type());
            self.evaluate.push(assign(&array, value));
            let binary = |left: Expr, op: BinaryOp, right: Expr| Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            self.hand_over.push(Stmt::Loop {
                kind: LoopKind::For,
                init: vec![assign(&at, Expr::Literal("0".into()))],
                condition: Some(binary(
                    Expr::Ident(at.clone()),
                    BinaryOp::Lt,
                    containers::call(
                        ContainerOp::Size,
                        layout.clone(),
                        vec![Expr::Ident(array.clone())],
                    ),
                )),
                step: vec![assign(
                    &at,
                    binary(
                        Expr::Ident(at.clone()),
                        BinaryOp::Add,
                        Expr::Literal("1".into()),
                    ),
                )],
                body: vec![Stmt::Eval(coverage(
                    CoverageOp::SetPush {
                        set: index,
                        width: ty.width,
                        signed: ty.signed,
                    },
                    vec![
                        self.cg(),
                        containers::call(
                            ContainerOp::Get,
                            layout,
                            vec![Expr::Ident(array), Expr::Ident(at.clone())],
                        ),
                    ],
                ))],
            });
            // The construction resolves the elements, and warns of those
            // that take no part (19.5.7).
            self.warns = true;
            return Ok(heap::BinValues::Set(index));
        }
        // The value of an array parameter is known here: its elements,
        // which follow each other in it from the left one, are the values
        // of the bin as if it listed them.
        let array = expression_binding::native_name(&builder.tree, set, native)
            .ok_or_else(not_supported)?;
        let shape = builder
            .dims
            .get(&array)
            .cloned()
            .ok_or_else(not_supported)?;
        if shape.unpacked.len() != 1
            || shape.real
            || shape.string
            || shape.chandle
            || shape.container().is_some()
            || shape.structure_declaration().is_some()
            || shape.class_in(&builder.session.types).is_some()
        {
            return Err(not_supported());
        }
        let size = |bound: &ConstExpr| {
            eval_ast_const_expr(&builder.tree.functions, bound, &builder.dims.const_env)
                .and_then(|value| usize::try_from(value).ok())
        };
        let width = shape
            .packed
            .iter()
            .try_fold(1usize, |width, dimension| {
                width.checked_mul(size(&dimension.width)?)
            })
            .filter(|width| *width != 0)
            .ok_or_else(not_supported)?;
        let count = size(&shape.unpacked[0].width).ok_or_else(not_supported)?;
        let ty = heap::ValueType {
            width: u32::try_from(width).map_err(|_| not_supported())?,
            signed: shape.signed,
        };
        let whole = builder
            .dims
            .array_parameter_values
            .get(&array)
            .cloned()
            .and_then(|value| builder.covergroup_constant(&value))
            .filter(|literal| literal.width == count * width)
            .ok_or_else(not_supported)?;
        let window = (BigUint::from(1u8) << width) - 1u8;
        let mut values = Vec::new();
        let mut details = Vec::new();
        for position in 0..count {
            let element = crate::typecheck::IntegralLiteral {
                width,
                signed: shape.signed,
                value: (&whole.value >> (position * width)) & &window,
                mask: (&whole.mask >> (position * width)) & &window,
            };
            let value =
                heap::RangeDescriptor::Value(heap::Operand::Constant(constant(element, Some(ty))));
            let mut found = Vec::new();
            self.resolved(point, bins.wildcard, value, &mut values, &mut found);
            // Many elements may take no part for one reason.
            if details.is_empty() {
                details.extend(found);
            } else if !found.is_empty() {
                details.truncate(1);
                details.push("so do other elements of the array".into());
            }
        }
        self.warn(builder, point, name, details, native.expression(set).tokens);
        Ok(heap::BinValues::ranges(values))
    }

    /// Bin declaration `bins` of coverpoint `point` (19.5.1 to 19.5.6),
    /// whose predicate bits are `bits` and whose values are of the type
    /// `item_type` of the variable of the coverpoint.
    fn bins(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        bins: &Bins,
        point: &heap::PointDescriptor,
        item_type: &Type,
        bits: BinBits,
    ) -> Converted<heap::BinDescriptor> {
        let model = self.site.model;
        let name = instances::normalize_identifier(&bins.name.spelling);
        let place = format!(
            "bin `{name}` of coverpoint `{}` of covergroup `{}`",
            point.name, model.display
        );
        let declared = Declared {
            point,
            item: item_type,
            bits,
            name: name.clone(),
            place: place.clone(),
        };
        let array = match &bins.array {
            None => heap::BinArray::Scalar,
            Some(BinsArray::Unsized) => heap::BinArray::Open,
            Some(BinsArray::Sized(size)) => {
                heap::BinArray::Fixed(self.size(builder, *size, &place)?)
            }
        };
        let values = match &bins.value {
            BinsValue::Ranges { ranges, with } => {
                let mut values = Vec::new();
                for range in ranges {
                    let value = self.range(builder, range, &place)?;
                    // One warning for an element of the list: the warnings
                    // of a source position are told apart by their kind
                    // alone.
                    let mut details = Vec::new();
                    self.resolved(point, bins.wildcard, value, &mut values, &mut details);
                    self.warn(builder, point, &name, details, range.tokens);
                }
                let filter = with
                    .map(|with| self.filter(builder, bins, with, &declared))
                    .transpose()?;
                self.patterns(bins, point, &values, filter)?;
                heap::BinValues::Ranges {
                    ranges: values,
                    filter,
                }
            }
            // "all values of the coverpoint" (19.5.1.1): of an enumeration
            // its named values, as its automatic bins are (19.5.3), in the
            // order of their declaration; else every value of the type.
            BinsValue::CoverpointWith { with, .. } => heap::BinValues::Ranges {
                ranges: if point.ty.members.is_empty() {
                    vec![heap::RangeDescriptor::Range {
                        low: heap::Bound::Open,
                        high: heap::Bound::Open,
                    }]
                } else {
                    point
                        .ty
                        .members
                        .iter()
                        .map(|(_, value)| {
                            heap::RangeDescriptor::Value(heap::Operand::Constant(value.clone()))
                        })
                        .collect()
                },
                filter: Some(self.filter(builder, bins, *with, &declared)?),
            },
            BinsValue::Set(set) => self.set(builder, bins, *set, &declared)?,
            BinsValue::Default => heap::BinValues::Default,
            BinsValue::Transitions(_) | BinsValue::DefaultSequence => {
                return Err(unsupported(
                    "covergroup transition bins (IEEE 1800-2023 19.5.2)",
                ));
            }
        };
        let mut bin = heap::BinDescriptor::new(name, values);
        bin.kind = match bins.kind {
            BinsKind::Bins => heap::BinKind::Bins,
            BinsKind::IgnoreBins => heap::BinKind::Ignore,
            BinsKind::IllegalBins => heap::BinKind::Illegal,
        };
        bin.array = array;
        bin.wildcard = bins.wildcard;
        bin.iff = bits.iff;
        Ok(bin)
    }

    /// What the wildcard values of `values` of bin declaration `bins` of
    /// `point` that are matched as patterns mean for it (19.5.4, see
    /// `heap::MAX_WILDCARD_EXPANSION`): only one bin without a filter that
    /// a construction answers may have one, and a construction warns
    /// where its values, which are not listed, would be taken from other
    /// bins or leave them.
    fn patterns(
        &mut self,
        bins: &Bins,
        point: &heap::PointDescriptor,
        values: &[heap::RangeDescriptor],
        filter: Option<heap::BinFilter>,
    ) -> Converted<()> {
        if !bins.wildcard {
            return Ok(());
        }
        let pattern = values.iter().any(|value| {
            range_value(value)
                .is_some_and(|value| heap::value_count(&point.ty, true, value).is_none())
        });
        if !pattern {
            return Ok(());
        }
        if bins.array.is_some() || matches!(filter, Some(heap::BinFilter::Enumerate(_))) {
            return Err(unsupported(format!(
                "covergroup wildcard value of more than {} ranges of values in a bin array or a bin with a filter (IEEE 1800-2023 19.5.4)",
                heap::MAX_WILDCARD_EXPANSION
            )));
        }
        self.warns = true;
        Ok(())
    }
}

/// Whether `name` is declared as a constant in the scope of `dims`: a
/// parameter, also one of an unpacked array type, or a constant of a type
/// that does not fold to a number.
fn declared_constant(dims: &PackedDimensions, name: &str) -> bool {
    dims.const_env.contains_key(name)
        || dims.parameter_values.contains_key(name)
        || dims.constant_types.contains_key(name)
        || dims.array_parameter_values.contains_key(name)
}

/// The values `item` of a coverpoint of type `ty` as an element of a range
/// list: constants of the type of the coverpoint, which resolve to these
/// values with no warning (19.5.7).
fn taking_part(ty: &heap::PointType, item: heap::ResolvedItem) -> heap::RangeDescriptor {
    use num_bigint::{BigInt, BigUint};
    let constant = |value: BigUint, mask: BigUint| {
        heap::Operand::Constant(heap::Constant {
            ty: heap::ValueType {
                width: ty.width,
                signed: ty.signed,
            },
            value,
            mask,
        })
    };
    // The bits of a value of the type: its two's complement.
    let bits = |value: BigInt| {
        let modulus = BigInt::from(1) << ty.width;
        ((value % &modulus + &modulus) % &modulus)
            .magnitude()
            .clone()
    };
    match item {
        heap::ResolvedItem::Range { low, high } => heap::RangeDescriptor::Range {
            low: heap::Bound::Operand(constant(bits(low), BigUint::ZERO)),
            high: heap::Bound::Operand(constant(bits(high), BigUint::ZERO)),
        },
        heap::ResolvedItem::Exact { value, mask } => {
            heap::RangeDescriptor::Value(constant(value, mask))
        }
    }
}

/// The subroutines `value` calls.
fn subroutine_calls(value: &Expr, calls: &mut Vec<String>) {
    match value {
        Expr::Ident(_) | Expr::Literal(_) => {}
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            subroutine_calls(expr, calls);
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            for part in parts {
                subroutine_calls(part, calls);
            }
        }
        Expr::Binary { left, right, .. } => {
            subroutine_calls(left, calls);
            subroutine_calls(right, calls);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            subroutine_calls(condition, calls);
            subroutine_calls(then_expr, calls);
            subroutine_calls(else_expr, calls);
        }
        Expr::Call { callee, args, .. } => {
            if let Callee::Subroutine(name) = callee {
                calls.push(name.clone());
            }
            for arg in args.iter().flatten() {
                subroutine_calls(arg, calls);
            }
        }
        Expr::Inside { expr, items } => {
            subroutine_calls(expr, calls);
            for item in items {
                for operand in item.exprs() {
                    subroutine_calls(operand, calls);
                }
            }
        }
    }
}

/// The statements that hand instance `cg` the arguments of the covergroup
/// that its samples read, `stored` (19.3), with the types of those the
/// record of the instance keeps, by slot.
fn keep_formals(
    site: &Site<'_>,
    builder: &BodyBuilder<'_, '_, '_>,
    stored: &[(usize, Stored)],
    cg: &Expr,
) -> Converted<(Vec<Stmt>, Vec<heap::ValueType>)> {
    let mut body = Vec::new();
    let mut formals = Vec::new();
    for (index, stored) in stored {
        let formal = &site.model.new_formals[*index];
        let argument = Expr::Ident(
            builder
                .names
                .lookup(&formal.name)
                .ok_or_else(|| unsupported("covergroup argument"))?
                .to_owned(),
        );
        body.push(Stmt::Eval(match stored {
            Stored::Slot {
                slot,
                width,
                signed,
            } => {
                formals.push(heap::ValueType {
                    width: *width,
                    signed: *signed,
                });
                coverage(
                    CoverageOp::FormalSet {
                        slot: *slot,
                        width: *width,
                        signed: *signed,
                    },
                    vec![cg.clone(), argument],
                )
            }
            Stored::Handle(field) => {
                classes::object_call(ObjectOp::Store(*field), vec![cg.clone(), argument])
            }
        }));
    }
    Ok((body, formals))
}

/// The statements that build the bins of instance `cg`. Where the
/// construction may know of a value that takes no part in its bin only now
/// (19.5.7), they issue the warnings of the heap as a `$warning`.
fn build(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    cg: &Expr,
    warns: bool,
) -> Vec<Stmt> {
    if !warns {
        return vec![Stmt::Eval(coverage(CoverageOp::Build, vec![cg.clone()]))];
    }
    reported(
        site,
        builder,
        cg,
        CoverageOp::Build,
        system_functions::Severity::Warning,
        "$warnings",
    )
}

/// The body of `N::$new` of the type of `site`, the variable of its
/// result, and the descriptor of the type, whose coverpoints and stored
/// arguments `sampled` found.
pub(super) fn body(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    handle: &Type,
    sampled: Sampled,
) -> Converted<(String, Vec<Stmt>, heap::TypeDescriptor)> {
    let model = site.model;
    let Sampled {
        stored,
        mut points,
        items,
        bins: bits,
    } = sampled;
    let instance = builder.declare(HANDLE, handle.clone());
    let mut construction = Construction {
        site,
        instance: instance.clone(),
        inputs: Vec::new(),
        evaluate: Vec::new(),
        hand_over: Vec::new(),
        options: Vec::new(),
        sets: Vec::new(),
        filters: Vec::new(),
        answer: Vec::new(),
        keyed: Vec::new(),
        warns: false,
    };
    let mut point = 0;
    for item in &site.declaration().items {
        match item {
            CoverageItem::Option(option) => {
                construction.option(builder, 0, option, Level::Covergroup)?;
            }
            CoverageItem::Coverpoint(coverpoint) => {
                let mut declaration = 0;
                for item in &coverpoint.items {
                    match item {
                        CoverpointItem::Option(option) => {
                            construction.option(
                                builder,
                                point as u32 + 1,
                                option,
                                Level::Coverpoint,
                            )?;
                        }
                        CoverpointItem::Bins(bins) => {
                            let bin = construction.bins(
                                builder,
                                bins,
                                &points[point],
                                &items[point],
                                bits[point][declaration],
                            )?;
                            points[point].bins.push(bin);
                            declaration += 1;
                        }
                    }
                }
                point += 1;
            }
            CoverageItem::Cross(_) => {
                return Err(unsupported("covergroup cross (IEEE 1800-2023 19.6)"));
            }
        }
    }
    let Construction {
        inputs,
        evaluate,
        hand_over,
        options,
        sets,
        filters,
        answer,
        warns,
        ..
    } = construction;
    let cg = Expr::Ident(instance.clone());
    // The string and class handle arguments are properties of the object.
    let handles = stored
        .iter()
        .filter_map(|(_, stored)| match stored {
            Stored::Handle(field) => Some((field.offset, field.handle?)),
            Stored::Slot { .. } => None,
        })
        .collect::<Vec<_>>();
    let size = stored
        .iter()
        .filter_map(|(_, stored)| match stored {
            Stored::Handle(field) => Some(field.offset + field.bytes().next_multiple_of(8)),
            Stored::Slot { .. } => None,
        })
        .max()
        .unwrap_or(0);
    let mut body = evaluate;
    body.push(assign(
        &instance,
        coverage(
            CoverageOp::New {
                class: model.class(),
                size,
                handles,
            },
            vec![Expr::Call {
                callee: Callee::Subroutine(model.type_subroutine()),
                args: Vec::new(),
                arg_order: Vec::new(),
            }],
        ),
    ));
    let (keep, formals) = keep_formals(site, builder, &stored, &cg)?;
    body.extend(keep);
    body.extend(hand_over);
    body.extend(answer);
    body.extend(build(site, builder, &cg, warns));
    let mut descriptor = heap::TypeDescriptor::new(&model.display);
    descriptor.embedded = model.owner.is_some();
    descriptor.inputs = inputs;
    descriptor.formals = formals;
    descriptor.points = points;
    descriptor.options = options;
    descriptor.sets = sets;
    descriptor.filters = filters;
    Ok((instance, body, descriptor))
}
