//! `N::$new` of a covergroup type (IEEE 1800-2023 19.3, 19.5, 19.7), and
//! with it the descriptor of the type.
//!
//! It evaluates what the definition reads when an instance is constructed,
//! the values of its options and the operands of its bins that are no
//! constants, makes the instance, hands it the arguments its samples read
//! and what it evaluated, and builds its bins.

use super::*;
use crate::ast::covergroups::{Level, OptionType};
use celox_sv_syntax::{
    Bins, BinsValue, CoverageItem, CoverageOption, CoverageRange, CoverageRangeKind, CoverpointItem,
};

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
    /// Whether building an instance may warn of what the analysis could
    /// not warn of: a bin operand or `option.auto_bin_max` that is known
    /// only then (19.5.7).
    warns: bool,
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
        let index = self.inputs.len() as u32;
        self.inputs.push(heap::InputDescriptor::Integral(ty));
        builder.rename_expr(&mut value);
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
        let mut value = builder.covergroup_value(value, Some(&ty))?;
        builder.rename_expr(&mut value);
        let index = self.inputs.len() as u32;
        self.inputs.push(heap::InputDescriptor::String);
        let local = builder.declare(&format!("$input{index}"), ty);
        self.evaluate.push(assign(&local, value));
        self.hand_over.push(Stmt::Eval(coverage(
            CoverageOp::InputString { index },
            vec![self.cg(), Expr::Ident(local)],
        )));
        Ok(heap::OptionValue::StringInput(index))
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
    /// whose call is a constant. `place` names the bin.
    fn check_operand(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        root: ExprId,
        place: &str,
    ) -> Converted<()> {
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
        for (name, id) in covergroups::variable_names(native, root) {
            let at = native.expression(id).tokens;
            if constant_calls
                .iter()
                .any(|call| call.start <= at.start && at.end <= call.end)
                || matches!(name.as_str(), "this" | "super")
                || model.new_formals.iter().any(|formal| formal.name == name)
            {
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
        let native = self.site.tree.declarations();
        self.check_operand(builder, root, place)?;
        let ty =
            expression_types::type_of(&builder.tree, builder.session, root, native, &builder.dims)?;
        let packed = match builder.session.types.data(ty) {
            semantic_types::TyData::Packed(packed) => packed.clone(),
            _ => {
                return Err(unsupported(
                    "covergroup bin value that is not integral (IEEE 1800-2023 19.5.1)",
                ));
            }
        };
        let ty = heap::ValueType {
            width: u32::try_from(packed.bits())
                .ok()
                .filter(|width| *width != 0)
                .ok_or_else(|| unsupported("covergroup bin value of this many bits"))?,
            signed: packed.signed,
        };
        let value = builder.covergroup_value(root, None)?;
        // A function a bin calls only reads its arguments (19.5).
        let mut calls = Vec::new();
        subroutine_calls(&value, &mut calls);
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
        Ok(match builder.covergroup_constant(&value) {
            Some(literal) => heap::Operand::Constant(constant(literal, Some(ty))),
            None => {
                self.warns = true;
                heap::Operand::Input(self.integral_input(
                    builder,
                    value,
                    ty,
                    packed_type(ty.width, ty.signed, true),
                ))
            }
        })
    }

    /// Element `range` of the range list of a bin (19.5.1). `place` names
    /// the bin.
    fn range(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        range: &CoverageRange,
        place: &str,
    ) -> Converted<heap::RangeDescriptor> {
        Ok(match &range.kind {
            CoverageRangeKind::Value(value) => {
                heap::RangeDescriptor::Value(self.operand(builder, *value, place)?)
            }
            CoverageRangeKind::Range { low, high } => {
                let mut bound = |bound: &Option<ExprId>| {
                    Ok(match bound {
                        Some(bound) => heap::Bound::Operand(self.operand(builder, *bound, place)?),
                        // `$`.
                        None => heap::Bound::Open,
                    })
                };
                heap::RangeDescriptor::Range {
                    low: bound(low)?,
                    high: bound(high)?,
                }
            }
            CoverageRangeKind::Tolerance { .. } => {
                return Err(unsupported(
                    "covergroup bin range with a tolerance (IEEE 1800-2023 19.5.1, 11.4.13)",
                ));
            }
        })
    }

    /// Bin declaration `bins` of coverpoint `point` (19.5.1): `bins name =
    /// { values and ranges }`.
    fn bins(
        &mut self,
        builder: &mut BodyBuilder<'_, '_, '_>,
        bins: &Bins,
        point: &heap::PointDescriptor,
    ) -> Converted<heap::BinDescriptor> {
        let site = self.site;
        let model = site.model;
        let name = instances::normalize_identifier(&bins.name.spelling);
        let place = format!(
            "bin `{name}` of coverpoint `{}` of covergroup `{}`",
            point.name, model.display
        );
        let BinsValue::Ranges { ranges, with: None } = &bins.value else {
            return Err(unsupported("covergroup bins (IEEE 1800-2023 19.5.1)"));
        };
        let mut values = Vec::new();
        for range in ranges {
            let value = self.range(builder, range, &place)?;
            // What the standard has a tool warn of (19.5.7) is known here
            // for constants: the value, or a part of the range, takes no
            // part in the bin. One warning for an element of the list: the
            // warnings of a source position are told apart by their kind
            // alone.
            match resolved_constants(&point.ty, &value) {
                Some(resolved) if !resolved.warnings.is_empty() => {
                    warnings::append(
                        &mut builder.session.diagnostics,
                        [warnings::AnalyzerWarning::CovergroupBinValue {
                            covergroup: model.display.clone(),
                            item: point.name.clone(),
                            bin: name.clone(),
                            detail: resolved.warnings.join("; "),
                            site: source_span(range.tokens, site.tree),
                        }],
                    );
                    // The descriptor has the values that take part in
                    // place of the element: a construction, which warns
                    // of what only it knows, does not warn of the element
                    // again.
                    values.extend(
                        resolved
                            .items
                            .into_iter()
                            .map(|item| taking_part(&point.ty, item)),
                    );
                }
                _ => values.push(value),
            }
        }
        Ok(heap::BinDescriptor::new(
            name,
            heap::BinValues::ranges(values),
        ))
    }
}

/// Whether `name` is declared as a constant in the scope of `dims`: a
/// parameter, or a constant of a type that does not fold to a number.
fn declared_constant(dims: &PackedDimensions, name: &str) -> bool {
    dims.const_env.contains_key(name)
        || dims.parameter_values.contains_key(name)
        || dims.constant_types.contains_key(name)
}

/// Element `range` of the range list of a bin resolved against the type
/// `ty` of its coverpoint (19.5.7), when its operands are constants: the
/// values that take part, and what the standard has a tool warn of. With
/// an operand that only a construction knows, the construction resolves
/// it and warns.
fn resolved_constants(
    ty: &heap::PointType,
    range: &heap::RangeDescriptor,
) -> Option<heap::Resolved> {
    let constant = |operand: &heap::Operand| match operand {
        heap::Operand::Constant(constant) => Some(constant.clone()),
        heap::Operand::Input(_) => None,
    };
    match range {
        heap::RangeDescriptor::Value(value) => {
            constant(value).map(|value| heap::resolve(ty, heap::RangeValue::Value(&value)))
        }
        heap::RangeDescriptor::Range { low, high } => {
            let bound = |bound: &heap::Bound| match bound {
                heap::Bound::Open => Some(None),
                heap::Bound::Operand(value) => constant(value).map(Some),
            };
            bound(low).zip(bound(high)).map(|(low, high)| {
                heap::resolve(
                    ty,
                    heap::RangeValue::Range {
                        low: low.as_ref(),
                        high: high.as_ref(),
                    },
                )
            })
        }
    }
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
    let build = coverage(CoverageOp::Build, vec![cg.clone()]);
    if !warns {
        return vec![Stmt::Eval(build)];
    }
    let count = builder.declare("$warnings", classes::unsigned_type(32));
    vec![
        assign(&count, build),
        Stmt::If {
            condition: Expr::Binary {
                left: Box::new(Expr::Ident(count)),
                op: BinaryOp::Ne,
                right: Box::new(Expr::Literal("32'd0".into())),
            },
            then_body: vec![Stmt::SystemTask {
                task: SystemTask::Severity(system_functions::Severity::Warning),
                args: vec![
                    SystemTaskArg::Str("%s".into()),
                    SystemTaskArg::Expr(coverage(CoverageOp::Message, vec![cg.clone()])),
                ],
                site: source_span(site.declaration().header, site.tree),
                assertion: None,
                home: None,
            }],
            else_body: Vec::new(),
        },
    ]
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
    let Sampled { stored, mut points } = sampled;
    let instance = builder.declare(HANDLE, handle.clone());
    let mut construction = Construction {
        site,
        instance: instance.clone(),
        inputs: Vec::new(),
        evaluate: Vec::new(),
        hand_over: Vec::new(),
        options: Vec::new(),
        warns: false,
    };
    let mut point = 0;
    for item in &site.declaration().items {
        match item {
            CoverageItem::Option(option) => {
                construction.option(builder, 0, option, Level::Covergroup)?;
            }
            CoverageItem::Coverpoint(coverpoint) => {
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
                            let bin = construction.bins(builder, bins, &points[point])?;
                            points[point].bins.push(bin);
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
    body.extend(build(site, builder, &cg, warns));
    let mut descriptor = heap::TypeDescriptor::new(&model.display);
    descriptor.embedded = model.owner.is_some();
    descriptor.inputs = inputs;
    descriptor.formals = formals;
    descriptor.points = points;
    descriptor.options = options;
    Ok((instance, body, descriptor))
}
