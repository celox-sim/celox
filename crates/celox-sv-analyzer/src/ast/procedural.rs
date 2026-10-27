//! Statement-structured procedural bodies.
//!
//! `always`, `initial`, and subroutine bodies are converted statement by
//! statement into [`Stmt`] trees. Variables declared inside a body are
//! lexically scoped (IEEE 1800-2023 6.21): each receives a module-unique name
//! (`name@n`) and is collected as a [`LocalVariable`], so a local may shadow a
//! module signal or share its name with a local of another process.

use super::*;
mod assertions;
mod control;
mod final_procedures;
mod foreach;
use foreach::ForeachLoop;
pub(super) mod randomization;
pub(super) mod timed_tasks;
mod timing;
use crate::procedural::{
    CaseItemBase, CaseKind, CaseLabel, EventEdge, EventItemBase, LoopKind, ParamDirection,
    SystemTaskArg,
};

/// One lexical scope: the locals it declares and the bindings they shadow.
struct Scope {
    entries: Vec<ScopeEntry>,
}

struct ScopeEntry {
    source: String,
    unique: Option<String>,
    shadowed_dimensions: Option<VariableDimensions>,
    shadowed_constants: Vec<(String, Option<i128>)>,
    shadowed_literal: Option<String>,
    shadowed_parameter_value: Option<Expr>,
    shadowed_signedness: Option<bool>,
}

/// Per-module state shared by every body converted in that module.
pub(super) struct BodyState<'a> {
    pub locals: &'a mut Vec<LocalVariable>,
    pub counter: &'a mut usize,
    /// Positional argument names of each subroutine, for named arguments.
    pub subroutine_params: &'a HashMap<String, Vec<String>>,
    /// The argument directions of each subroutine.
    pub subroutine_directions: &'a lifetimes::Directions,
    /// Whether the variables of the scope's subroutines and blocks are
    /// automatic by default (IEEE 1800-2023 6.21).
    pub automatic: bool,
    /// The locals declared with a static lifetime: their unique names and
    /// types.
    pub statics: Vec<(String, Type)>,
    /// Function static declarations initialized once at time zero.
    pub static_initializers: Vec<Stmt>,
    /// Constant calls get a fresh frame, even for static declarations (13.4.3).
    pub constant_evaluation: bool,
}

/// Converts the statements of one procedural body.
pub(super) struct BodyBuilder<'s, 't, 'a> {
    tree: &'t AnalysisContext<'t>,
    dims: PackedDimensions,
    scopes: Vec<Scope>,
    state: &'s mut BodyState<'a>,
    type_aliases: HashMap<String, Type>,
    /// The values of the local parameters declared in the body.
    local_constants: HashMap<String, String>,
    /// The kind of body, for the system tasks it may call.
    body: system_functions::Body,
    /// Whether the body's variables are automatic by default.
    automatic: bool,
    task_timing: bool,
    return_shape: Option<VariableDimensions>,
    pub(super) sensitivity_body: Vec<Stmt>,
}

/// The packed and unpacked shape of a declared type, as selects see it.
pub(super) fn dimensions_from_type(r#type: &Type) -> VariableDimensions {
    VariableDimensions {
        container: r#type.container.clone(),
        randomizable: r#type.container.is_none()
            && (!r#type.is_enum || r#type.enum_values.is_some()),
        enum_values: r#type.enum_values.clone(),
        real: matches!(r#type.kind(), TypeKind::Real | TypeKind::Shortreal),
        string: matches!(r#type.kind(), TypeKind::String),
        class: r#type.class().map(str::to_owned),
        packed: dimensions::signal_packed_dimension_widths(r#type.packed_ranges()),
        unpacked: unpacked_dimension_widths(r#type.unpacked_ranges()),
        signed: r#type.is_signed(),
        is_2state: matches!(
            r#type.kind(),
            TypeKind::Bit
                | TypeKind::Real
                | TypeKind::Shortreal
                | TypeKind::String
                | TypeKind::Object
        ),
        members: r#type.members.clone(),
        signed_element_depth: r#type.signed_element_depth,
    }
}

/// `r#type` with the constants of its declaration scope substituted into its
/// ranges, so it keeps its meaning outside that scope.
pub(super) fn scoped_type(mut r#type: Type, const_env: &HashMap<String, i128>) -> Type {
    for range in &mut r#type.packed_ranges {
        range.left = substitute_dimension_constants(range.left.clone(), const_env);
        range.right = substitute_dimension_constants(range.right.clone(), const_env);
    }
    for range in &mut r#type.unpacked_ranges {
        range.left = substitute_dimension_constants(range.left.clone(), const_env);
        range.right = substitute_dimension_constants(range.right.clone(), const_env);
        range.size = range
            .size
            .take()
            .map(|size| substitute_dimension_constants(size, const_env));
    }
    r#type
}

/// The `int` type of a loop variable declared by `foreach`.
pub(super) fn int_type() -> Type {
    Type {
        container: None,
        is_enum: false,
        enum_values: None,
        kind: TypeKind::Bit,
        is_signed: true,
        packed_ranges: vec![PackedRange::new(
            ConstExpr::Literal("31".to_string()),
            ConstExpr::Literal("0".to_string()),
        )],
        unpacked_ranges: Vec::new(),
        members: Vec::new(),
        signed_element_depth: None,
        class: None,
    }
}

impl<'s, 't, 'a> BodyBuilder<'s, 't, 'a> {
    pub(super) fn new(
        tree: &'t AnalysisContext<'t>,
        dims: &PackedDimensions,
        state: &'s mut BodyState<'a>,
        body: system_functions::Body,
    ) -> Self {
        let type_aliases = dims.type_aliases.clone();
        let automatic = state.automatic;
        Self {
            automatic,
            task_timing: false,
            return_shape: None,
            sensitivity_body: Vec::new(),
            tree,
            dims: dims.clone(),
            scopes: vec![Scope {
                entries: Vec::new(),
            }],
            state,
            type_aliases,
            local_constants: HashMap::default(),
            body,
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            entries: Vec::new(),
        });
    }

    fn pop_scope(&mut self) {
        let Some(scope) = self.scopes.pop() else {
            return;
        };
        for entry in scope.entries.into_iter().rev() {
            for (key, value) in entry.shadowed_constants {
                if let Some(value) = value {
                    self.dims.const_env.insert(key, value);
                } else {
                    self.dims.const_env.remove(&key);
                }
            }
            match entry.shadowed_literal {
                Some(value) => {
                    self.local_constants.insert(entry.source.clone(), value);
                }
                None => {
                    self.local_constants.remove(&entry.source);
                }
            }
            match entry.shadowed_parameter_value {
                Some(value) => {
                    self.dims
                        .parameter_values
                        .insert(entry.source.clone(), value);
                }
                None => {
                    self.dims.parameter_values.remove(&entry.source);
                }
            }
            match entry.shadowed_dimensions {
                Some(dimensions) => {
                    self.dims.insert(entry.source.clone(), dimensions);
                }
                None => {
                    self.dims.remove(&entry.source);
                }
            }
            let signedness = &mut self.dims.expression_signedness;
            match entry.shadowed_signedness {
                Some(signed) => {
                    signedness.insert(entry.source, signed);
                }
                None => {
                    signedness.remove(&entry.source);
                }
            }
        }
    }

    /// Declare a local in the innermost scope and return its unique name.
    pub(super) fn declare(&mut self, source: &str, r#type: Type) -> String {
        let unique = format!("{source}@{}", *self.state.counter);
        *self.state.counter += 1;
        let entry = self.take_binding(source, Some(unique.clone()), r#type.packed_ranges.len());
        self.dims
            .insert(source.to_string(), dimensions_from_type(&r#type));
        self.dims
            .expression_signedness
            .insert(source.to_string(), r#type.is_signed());
        self.state.locals.push(LocalVariable {
            name: unique.clone(),
            source_name: source.to_string(),
            r#type: crate::ir::Type::from_ast(
                scoped_type(r#type, &self.dims.const_env),
                &const_functions::Environment {
                    constants: &*self.dims.const_env,
                    functions: &self.tree.state.const_functions,
                },
            ),
        });
        self.scopes
            .last_mut()
            .expect("a body always has a scope")
            .entries
            .push(entry);
        unique
    }

    /// Detach every view of a declaration before publishing the shadowing
    /// binding. Save only keys for this name, without copying a growing scope.
    fn take_binding(
        &mut self,
        source: &str,
        unique: Option<String>,
        packed_count: usize,
    ) -> ScopeEntry {
        let keys = parameters::binding_keys(source, &self.dims.const_env, packed_count);
        ScopeEntry {
            source: source.to_string(),
            unique,
            shadowed_constants: keys
                .into_iter()
                .map(|key| {
                    let value = self.dims.const_env.remove(&key);
                    (key, value)
                })
                .collect(),
            shadowed_literal: self.local_constants.remove(source),
            shadowed_parameter_value: self.dims.parameter_values.remove(source),
            shadowed_dimensions: self.dims.remove(source),
            shadowed_signedness: self.dims.expression_signedness.remove(source),
        }
    }

    /// Hide the module-scope names that the properties of `class` shadow
    /// inside its methods (IEEE 1800-2023 8.5), until the scope ends.
    /// A static property's name is renamed to the module variable that holds
    /// it (IEEE 1800-2023 8.9).
    fn enter_class_scope(&mut self, class: &classes::Class) {
        self.dims.class_scope = Some(class.name.clone());
        for property in &class.properties {
            let entry = self.take_binding(&property.name, None, 0);
            self.scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        for property in &class.statics {
            // A base's local static is not visible in the derived class (8.18).
            // Keep it out of the alias table so binding can diagnose its use.
            if property.local && property.owner != class.name {
                continue;
            }
            let entry = self.take_binding(
                &property.name,
                Some(property.variable.clone()),
                property.r#type.packed_ranges.len(),
            );
            self.dims.insert(
                property.name.clone(),
                dimensions_from_type(&property.r#type),
            );
            self.dims
                .expression_signedness
                .insert(property.name.clone(), property.r#type.is_signed());
            self.scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
    }

    fn lookup(&self, source: &str) -> Option<&str> {
        for scope in self.scopes.iter().rev() {
            if let Some(entry) = scope
                .entries
                .iter()
                .rev()
                .find(|entry| entry.source == source)
            {
                // A parameter in an inner block also hides an outer variable.
                return entry.unique.as_deref();
            }
        }
        None
    }

    fn rename_name(&self, name: &mut String) {
        if let Some(unique) = self.lookup(name) {
            *name = unique.to_string();
        }
    }

    /// The literal value of a local parameter that `name` refers to.
    fn local_constant(&self, name: &str) -> Option<&String> {
        if self.lookup(name).is_some() {
            return None;
        }
        self.local_constants.get(name)
    }

    fn rename_const(&self, expr: &mut ConstExpr) {
        match expr {
            ConstExpr::Ident(name) => match self.local_constant(name) {
                Some(value) => *expr = ConstExpr::Literal(value.clone()),
                None => self.rename_name(name),
            },
            ConstExpr::Literal(_) => {}
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                self.rename_const(expr);
                self.rename_const(msb);
                self.rename_const(lsb);
            }
            ConstExpr::Select { expr, bit } => {
                self.rename_const(expr);
                self.rename_const(bit);
            }
            ConstExpr::Function { args, .. } => {
                args.iter_mut().for_each(|arg| self.rename_const(arg));
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                self.rename_const(expr)
            }
            ConstExpr::Binary { left, right, .. } => {
                self.rename_const(left);
                self.rename_const(right);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.rename_const(condition);
                self.rename_const(then_expr);
                self.rename_const(else_expr);
            }
        }
    }

    fn rename_expr(&self, expr: &mut Expr) {
        match expr {
            Expr::Ident(name) => match self.local_constant(name) {
                Some(value) => *expr = Expr::Literal(value.clone()),
                None => self.rename_name(name),
            },
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                self.rename_expr(expr);
                self.rename_const(msb);
                self.rename_const(lsb);
            }
            Expr::Concat(parts) => parts.iter_mut().for_each(|part| self.rename_expr(part)),
            Expr::RepeatConcat { count, parts } => {
                self.rename_const(count);
                parts.iter_mut().for_each(|part| self.rename_expr(part));
            }
            Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => self.rename_expr(expr),
            Expr::Binary { left, right, .. } => {
                self.rename_expr(left);
                self.rename_expr(right);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.rename_expr(condition);
                self.rename_expr(then_expr);
                self.rename_expr(else_expr);
            }
            Expr::Call { args, .. } => args
                .iter_mut()
                .flatten()
                .for_each(|arg| self.rename_expr(arg)),
            Expr::Inside { expr, items } => {
                self.rename_expr(expr);
                for item in items {
                    match item {
                        InsideItem::Value(value) => self.rename_expr(value),
                        InsideItem::Range { low, high } => {
                            self.rename_expr(low);
                            self.rename_expr(high);
                        }
                    }
                }
            }
        }
    }

    fn rename_lvalue(&self, lvalue: &mut LValue) {
        match lvalue {
            LValue::Ident(name) => self.rename_name(name),
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                self.rename_name(name);
                self.rename_const(msb);
                self.rename_const(lsb);
                if let Some(width) = array_slice_width {
                    self.rename_const(width);
                }
            }
        }
    }

    fn native_value(&self, id: celox_sv_syntax::ExprId) -> Result<Expr, AnalyzerError> {
        let mut value =
            expression_binding::value(id, self.tree.declarations(), self.tree, &self.dims)?;
        self.rename_expr(&mut value);
        Ok(value)
    }

    fn native_lvalue(
        &self,
        id: celox_sv_syntax::ExprId,
    ) -> Result<(LValue, LValue), AnalyzerError> {
        let lowered = expression_binding::variable_lvalue(
            id,
            self.tree.declarations(),
            self.tree,
            &self.dims,
        )?;
        let mut renamed = lowered.clone();
        self.rename_lvalue(&mut renamed);
        Ok((lowered, renamed))
    }

    /// The parts of a concatenated assignment target, most significant first.
    fn concat_lvalue(&self, root: celox_sv_syntax::ExprId) -> Converted<Option<Vec<LValue>>> {
        let native = self.tree.declarations();
        let celox_sv_syntax::ExprKind::Concatenation(parts) = &native.expression(root).kind else {
            return Ok(None);
        };
        let mut pending = parts.iter().rev().copied().collect::<Vec<_>>();
        let mut parts = Vec::new();
        while let Some(id) = pending.pop() {
            if let celox_sv_syntax::ExprKind::Concatenation(items) = &native.expression(id).kind {
                pending.extend(items.iter().rev().copied());
            } else {
                if expression_binding::member_context(id, native, self.tree, &self.dims)?
                    .is_some_and(|(guards, _)| !guards.is_empty())
                {
                    return Err(unsupported(
                        "concatenation assignment to a tagged union member",
                    ));
                }
                self.reject_tagged_integral_target(id)?;
                let mut target =
                    expression_binding::variable_lvalue(id, native, self.tree, &self.dims)?;
                self.rename_lvalue(&mut target);
                parts.push(target);
            }
        }
        Ok(Some(parts))
    }

    fn assignment_value(
        &self,
        op: &str,
        nonblocking: bool,
        native: (celox_sv_syntax::ExprId, celox_sv_syntax::ExprId),
    ) -> Result<Stmt, AnalyzerError> {
        let (left, right) = native;
        if let Some(mut stmt) =
            containers::set(left, right, op, nonblocking, self.tree, &self.dims)?
        {
            if let Stmt::Eval(expr) = &mut stmt {
                self.rename_expr(expr);
            }
            return Ok(stmt);
        }
        if let Some(parts) = self.concat_lvalue(left)? {
            if op != "=" {
                return Err(unsupported("compound assignment to a concatenation"));
            }
            return Ok(Stmt::AssignConcat {
                parts,
                rhs: self.native_value(right)?,
                nonblocking,
            });
        }
        if op == "="
            && let Some(target) = array_compatibility::native_unpacked_shape(
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            array_compatibility::check_native_unpacked_assignment(
                &self.tree.state,
                right,
                self.tree.declarations(),
                &target,
                || "assignment".into(),
                &self.dims,
            )?;
        }
        if let Some(stmt) = self.string_character_assignment(op, nonblocking, left, right)? {
            return Ok(stmt);
        }
        if let Some(stmt) = self.property_assignment(op, nonblocking, left, right)? {
            return Ok(stmt);
        }
        if op != "=" {
            self.reject_tagged_integral_target(left)?;
        }
        let (written, lhs) = self.native_lvalue(left)?;
        if op != "="
            && expression_binding::native_string(
                self.tree,
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            return Err(unsupported(format!("operator `{op}` on a string")));
        }
        let member_context = expression_binding::member_context(
            left,
            self.tree.declarations(),
            self.tree,
            &self.dims,
        )?;
        let mut rhs = if op == "="
            && let Some((_, ty)) = &member_context
        {
            let shape = procedural::dimensions_from_type(ty);
            expression_binding::bind_value(
                right,
                self.tree.declarations(),
                self.tree,
                &self.dims,
                Some(&shape),
            )?
        } else if op == "=" {
            expression_binding::expression(
                right,
                self.tree.declarations(),
                &written,
                self.tree,
                &self.dims,
            )?
        } else {
            let value =
                expression_binding::value(right, self.tree.declarations(), self.tree, &self.dims)?;
            assignment_op_expr(&self.tree.state, &written, op, value, &self.dims)?
        };
        // Preserve assignment sizing before checked accesses introduce a temporary
        // (IEEE 1800-2023 11.8.2), using the selected destination width.
        // A two-state member of a four-state packed struct also drops unknown bits.
        if matches!(
            written,
            LValue::Select {
                is_2state: true,
                ..
            }
        ) || member_context
            .as_ref()
            .is_some_and(|(guards, _)| !guards.is_empty())
        {
            rhs = coerce_procedural_assignment_rhs(&self.tree.state, rhs, &written, &self.dims);
        }
        if let Some((guards, _)) = member_context {
            rhs = packed_structs::checked_value(guards, rhs, &self.tree.state, &self.dims)?;
        }
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking,
        })
    }

    /// `str[i] = c`, which replaces one character like `str.putc(i, c)`
    /// (IEEE 1800-2023 6.16.2).
    fn string_character_assignment(
        &self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let celox_sv_syntax::ExprKind::Select {
            expression,
            dimension,
        } = &native
            .expression(expression_binding::ungroup(left, native))
            .kind
        else {
            return Ok(None);
        };
        if !expression_binding::native_string(self.tree, *expression, native, &self.dims) {
            return Ok(None);
        }
        let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
            return Err(unsupported("part-select of a string"));
        };
        if op != "=" {
            return Err(unsupported(format!(
                "operator `{op}` on a string character"
            )));
        }
        let index = expression_binding::value(index, native, self.tree, &self.dims)?;
        let character = expression_binding::value(right, native, self.tree, &self.dims)?;
        self.string_update(
            *expression,
            StringOp::Putc,
            vec![index, character],
            nonblocking,
        )
        .map(Some)
    }

    /// An assignment to a property of a class object: `h.p = v`, or `p = v`
    /// inside a method of its class (IEEE 1800-2023 8.5).
    fn property_assignment(
        &self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let Some((property, receiver)) =
            expression_binding::object_property(self.tree, left, native, &self.dims)?
        else {
            return Ok(None);
        };
        if nonblocking {
            return Err(unsupported("nonblocking assignment to a class property"));
        }
        let handle = expression_binding::object_receiver(self.tree, receiver, native, &self.dims)?;
        let target = dimensions_from_type(&property.r#type);
        let bind = |right| {
            expression_binding::bind_value(right, native, self.tree, &self.dims, Some(&target))
        };
        let mut value = match property.r#type.class() {
            Some(class) if op == "=" => {
                match expression_binding::object_constructor(
                    self.tree, right, native, &self.dims, class,
                )? {
                    Some(call) => call,
                    None => {
                        expression_binding::check_handle_assignment(
                            self.tree,
                            right,
                            native,
                            &self.dims,
                            class,
                            || format!("assignment to property `{}`", property.name),
                        )?;
                        bind(right)?
                    }
                }
            }
            _ if op == "=" => bind(right)?,
            _ => expression_binding::value(right, native, self.tree, &self.dims)?,
        };
        if property.field.handle == Some(system_functions::HandleProperty::String) {
            if op != "=" {
                return Err(unsupported(format!("operator `{op}` on a string")));
            }
            value = expression_binding::string_operand(value, &self.dims);
        }
        self.property_store(&property, handle, op, value).map(Some)
    }

    /// `h.p op value`: a store of `value` into property `property` of the
    /// object `handle`, combined with its old value by a compound `op`.
    fn property_store(
        &self,
        property: &classes::Property,
        handle: Expr,
        op: &str,
        mut value: Expr,
    ) -> Result<Stmt, AnalyzerError> {
        if op != "=" {
            let operator = match op.strip_suffix('=') {
                Some("+") => BinaryOp::Add,
                Some("-") => BinaryOp::Sub,
                Some("*") => BinaryOp::Mul,
                Some("/") => BinaryOp::Div,
                Some("%") => BinaryOp::Mod,
                Some("&") => BinaryOp::BitAnd,
                Some("|") => BinaryOp::BitOr,
                Some("^") => BinaryOp::BitXor,
                Some("<<") | Some("<<<") => BinaryOp::Shl,
                Some(">>") => BinaryOp::Shr,
                Some(">>>") => BinaryOp::Sar,
                _ => return Err(unsupported(format!("assignment operator `{op}`"))),
            };
            value = Expr::Binary {
                left: Box::new(classes::object_call(
                    system_functions::ObjectOp::Load(property.field),
                    vec![handle.clone()],
                )),
                op: operator,
                right: Box::new(value),
            };
        }
        let mut store = classes::object_call(
            system_functions::ObjectOp::Store(property.field),
            vec![handle, value],
        );
        self.rename_expr(&mut store);
        Ok(Stmt::Eval(store))
    }

    /// A call of a class method as a statement, `h.m(args)`, `h.m`, or an
    /// unqualified `m(args)` inside a method of its class (IEEE 1800-2023
    /// 8.10).
    fn method_call_statement(
        &self,
        root: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some(mut call) =
            expression_binding::object_rand_mode_call(self.tree, root, native, &self.dims, true)?
        {
            self.rename_expr(&mut call);
            return Ok(Some(match call {
                Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order,
                } => Stmt::Call {
                    name,
                    args,
                    arg_order,
                },
                call => Stmt::Eval(call),
            }));
        }
        let call = match native.expression(root).kind {
            celox_sv_syntax::ExprKind::Call { expression, .. } => {
                if expression_binding::object_method(self.tree, expression, native, &self.dims)?
                    .is_none()
                {
                    return Ok(None);
                }
                expression_binding::value(root, native, self.tree, &self.dims)?
            }
            _ => {
                let Some((name, formals, receiver)) =
                    expression_binding::object_method(self.tree, root, native, &self.dims)?
                else {
                    return Ok(None);
                };
                let mut args = match receiver {
                    expression_binding::ObjectReceiver::Handle(receiver) => vec![Some(
                        expression_binding::value(receiver, native, self.tree, &self.dims)?,
                    )],
                    expression_binding::ObjectReceiver::This => {
                        vec![Some(expression_binding::object_this())]
                    }
                    expression_binding::ObjectReceiver::Static => Vec::new(),
                };
                if name.ends_with("::randomize") {
                    args.push(Some(Expr::Literal("1'b0".into())));
                }
                if name.ends_with("::srandom") {
                    return Err(unsupported("srandom requires a seed argument"));
                }
                args.resize(
                    formals + usize::from(receiver != expression_binding::ObjectReceiver::Static),
                    None,
                );
                Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order: Vec::new(),
                }
            }
        };
        let mut call = call;
        self.rename_expr(&mut call);
        let Expr::Call {
            callee: Callee::Subroutine(name),
            args,
            arg_order,
        } = call
        else {
            return Err(unsupported("method call statement"));
        };
        Ok(Some(Stmt::Call {
            name,
            args,
            arg_order,
        }))
    }

    /// `string = op(string, args)`: a method that updates the string it is
    /// called on.
    fn string_update(
        &self,
        string: celox_sv_syntax::ExprId,
        op: StringOp,
        args: Vec<Expr>,
        nonblocking: bool,
    ) -> Result<Stmt, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some((property, receiver)) =
            expression_binding::object_property(self.tree, string, native, &self.dims)?
        {
            if nonblocking {
                return Err(unsupported("nonblocking assignment to a class property"));
            }
            let handle =
                expression_binding::object_receiver(self.tree, receiver, native, &self.dims)?;
            let receiver = (op == StringOp::Putc).then(|| {
                classes::object_call(
                    system_functions::ObjectOp::Load(property.field),
                    vec![handle.clone()],
                )
            });
            let value =
                expression_binding::string_call(op, receiver.into_iter().chain(args).collect());
            let mut store = classes::object_call(
                system_functions::ObjectOp::Store(property.field),
                vec![handle, value],
            );
            self.rename_expr(&mut store);
            return Ok(Stmt::Eval(store));
        }
        let (written, lhs) = self.native_lvalue(string)?;
        let LValue::Ident(name) = &written else {
            return Err(unsupported(format!(
                "string method `{op}` on an array element"
            )));
        };
        let receiver = (op == StringOp::Putc).then(|| Expr::Ident(name.clone()));
        let mut rhs =
            expression_binding::string_call(op, receiver.into_iter().chain(args).collect());
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking,
        })
    }

    fn reject_tagged_integral_target(&self, root: celox_sv_syntax::ExprId) -> Converted<()> {
        if dimensions::operand_shape(root, self.tree, &self.dims)?.is_some_and(|ty| {
            ty.members
                .first()
                .is_some_and(packed_structs::PackedMember::is_tagged)
        }) {
            return Err(unsupported("integral update of a tagged union"));
        }
        Ok(())
    }

    fn inc_or_dec(&self, root: celox_sv_syntax::ExprId) -> Result<Stmt, AnalyzerError> {
        let celox_sv_syntax::ExprKind::Unary {
            operator,
            expression,
            ..
        } = &self.tree.declarations().expression(root).kind
        else {
            return Err(unsupported("increment or decrement expression"));
        };
        let op = match operator.as_str() {
            "++" => "+=",
            "--" => "-=",
            _ => return Err(unsupported("increment or decrement operator")),
        };
        let native = self.tree.declarations();
        if let Some((property, receiver)) =
            expression_binding::object_property(self.tree, *expression, native, &self.dims)?
        {
            if property.field.handle.is_some() {
                return Err(unsupported(format!("operator `{operator}` on a handle")));
            }
            let handle =
                expression_binding::object_receiver(self.tree, receiver, native, &self.dims)?;
            return self.property_store(&property, handle, op, Expr::Literal("1".into()));
        }
        self.reject_tagged_integral_target(*expression)?;
        let (written, lhs) = self.native_lvalue(*expression)?;
        let mut rhs = assignment_op_expr(
            &self.tree.state,
            &written,
            op,
            Expr::Literal("1".into()),
            &self.dims,
        )?;
        if let Some((guards, _)) =
            expression_binding::member_context(*expression, native, self.tree, &self.dims)?
        {
            rhs = packed_structs::checked_value(guards, rhs, &self.tree.state, &self.dims)?;
        }
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking: false,
        })
    }

    fn native_statement_leaf(
        &mut self,
        statement: &celox_sv_syntax::StatementKind,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        use celox_sv_syntax::{ExprKind, StatementKind};
        match statement {
            StatementKind::Assignment {
                left,
                operator,
                right,
                nonblocking,
                timing,
                ..
            } => {
                if timing.is_some() {
                    return Err(unsupported("intra-assignment timing control"));
                }
                if (operator == "<=") != *nonblocking {
                    return Err(AnalyzerError::Parse(
                        "native assignment operator mode disagrees".into(),
                    ));
                }
                Ok(vec![self.assignment_value(
                    if *nonblocking { "=" } else { operator },
                    *nonblocking,
                    (*left, *right),
                )?])
            }
            StatementKind::Expression(id) => match &self.tree.declarations().expression(*id).kind {
                ExprKind::Unary { operator, .. } if matches!(operator.as_str(), "++" | "--") => {
                    Ok(vec![self.inc_or_dec(*id)?])
                }
                _ => self.subroutine_call_statement(*id),
            },
            StatementKind::Assertion { .. } => Err(unsupported("concurrent assertion")),
            StatementKind::WaitFork => {
                self.allow_timing("wait fork statement")?;
                Ok(vec![Stmt::WaitFork])
            }
            StatementKind::WaitOrder { .. } => Err(unsupported("wait order statement")),
            StatementKind::EventTrigger(_) => Err(unsupported("event trigger")),
            StatementKind::ProceduralAssign(_) | StatementKind::ProceduralRelease(_) => {
                Err(unsupported("procedural continuous assignment"))
            }
            StatementKind::Disable(tokens) => {
                let disables_fork = self
                    .tree
                    .declarations()
                    .tokens
                    .get(tokens.start..tokens.end)
                    .into_iter()
                    .flatten()
                    .filter(|token| !token.is_trivia())
                    .find(|token| token.text != "disable")
                    .is_some_and(|token| token.text == "fork");
                if disables_fork {
                    Ok(vec![Stmt::DisableFork])
                } else {
                    Err(unsupported("disable statement"))
                }
            }
            StatementKind::Parallel { .. } => Err(unsupported("fork-join block")),
            _ => Err(unsupported("procedural statement")),
        }
    }

    /// Timing controls suspend the process that executes them: an
    /// `initial` or `always` process. An `always_comb` or `always_ff` body
    /// and a function may not suspend. A task suspends its caller.
    fn allow_timing(&self, construct: &'static str) -> Result<(), AnalyzerError> {
        match self.body {
            system_functions::Body::Initial => Ok(()),
            system_functions::Body::Final => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "a final procedure",
            }),
            system_functions::Body::Always => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "an always_comb or always_ff body",
            }),
            system_functions::Body::Subroutine if self.task_timing => Ok(()),
            // IEEE 1800-2023 13.4: a function shall not contain a timing
            // control.
            system_functions::Body::Subroutine => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "a function",
            }),
        }
    }

    /// Declare native local variables and return their initializations.
    pub(super) fn block_item_declaration(
        &mut self,
        native: &celox_sv_syntax::LocalDeclaration,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = match native {
            celox_sv_syntax::LocalDeclaration::Parameters(id) => return self.local_parameters(*id),
            celox_sv_syntax::LocalDeclaration::Variables(value)
            | celox_sv_syntax::LocalDeclaration::Constructed(value) => value,
            celox_sv_syntax::LocalDeclaration::TypeAlias(_) => {
                return Err(unsupported("local type declaration"));
            }
            celox_sv_syntax::LocalDeclaration::Unconverted(_) => {
                return Err(unsupported("let declaration inside a procedural block"));
            }
            _ => return Err(unsupported("local declaration")),
        };
        let signals = type_binding::local_signals(
            native,
            self.tree,
            &self.dims.const_env,
            &self.type_aliases,
            None,
        )?;
        let mut stmts = Vec::new();
        for (signal, assignment) in
            signals
                .into_iter()
                .zip(native.variables.iter().filter(|variable| {
                    type_binding::ordinary_variable(
                        variable,
                        native.data_type,
                        self.tree.declarations(),
                    )
                }))
        {
            // A declaration may not repeat a name of its own scope, such as
            // a subroutine argument (IEEE 1800-2023 6.21).
            let scope = self.scopes.last().expect("a body always has a scope");
            if scope
                .entries
                .iter()
                .any(|entry| entry.source == signal.name())
            {
                return Err(unsupported(format!(
                    "duplicate declaration of `{}` in one scope",
                    signal.name()
                )));
            }
            let init = match assignment.value {
                Some(root) => {
                    let target = LValue::Ident(signal.name().to_string());
                    let mut scoped = self.dims.clone();
                    scoped.insert(
                        signal.name().to_string(),
                        dimensions_from_type(signal.r#type()),
                    );
                    let mut init = expression_binding::expression(
                        root,
                        self.tree.declarations(),
                        &target,
                        self.tree,
                        &scoped,
                    )?;
                    self.rename_expr(&mut init);
                    Some(init)
                }
                None => None,
            };
            let name = self.declare(signal.name(), signal.r#type().clone());
            if signal.is_const {
                let mut shape = self.dims.get(signal.name()).unwrap().clone();
                shape.randomizable = false;
                self.dims.insert(signal.name().to_string(), shape);
            }
            let automatic = match native.lifetime.as_deref() {
                Some("automatic") => true,
                Some("static") => false,
                _ => self.automatic,
            };
            if !automatic {
                let r#type = scoped_type(signal.r#type().clone(), &self.dims.const_env);
                self.state.statics.push((name.clone(), r#type));
            }
            stmts.push(Stmt::Local { name, init });
        }
        Ok(stmts)
    }

    /// Make the parameters of a declaration in the body known as constants.
    fn local_parameters(
        &mut self,
        id: celox_sv_syntax::ParameterDeclarationId,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut parameters = Vec::new();
        let authored = self
            .tree
            .declarations()
            .parameter_declaration(id)
            .parameters
            .iter()
            .collect::<Vec<_>>();
        parameters::parameters_from_native(
            &authored,
            self.tree,
            &mut parameters,
            true,
            &self.dims.const_env,
            &self.type_aliases,
            &HashMap::default(),
        )?;
        let mut types = parameter_types_from_const_env(&self.dims.const_env);
        for parameter in &parameters {
            let name = parameter.name();
            let entry = self.take_binding(name, None, parameter.packed_ranges.len());
            types.remove(name);
            if !parameters::bind_parameter(
                &self.tree.state,
                &mut self.dims.const_env,
                &mut types,
                &mut self.dims.parameter_values,
                parameter,
            ) {
                return Err(unsupported("local parameter whose value is not constant"));
            }
            let literal = if let Some(value) = self.dims.const_env.get(name).copied() {
                match types.get(name) {
                    Some(ty) => format_typed_parameter_literal(value, ty.width, ty.signed),
                    None => value.to_string(),
                }
            } else if let Some(Expr::Literal(value)) = self.dims.parameter_values.get(name) {
                value.clone()
            } else {
                return Err(unsupported("local parameter whose value is not constant"));
            };
            self.local_constants
                .insert(name.to_string(), literal.clone());
            self.dims
                .parameter_values
                .insert(name.to_string(), Expr::Literal(literal));
            if let Some(dimensions) =
                parameter_packed_dimensions(std::slice::from_ref(parameter)).remove(name)
            {
                self.dims.insert(name.to_string(), dimensions);
            }
            if let Some(ty) = types.get(name) {
                self.dims
                    .expression_signedness
                    .insert(name.to_string(), ty.signed);
            }
            self.scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        Ok(Vec::new())
    }

    fn subroutine_call_statement(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some((mut value, _)) = array_reductions::bind(root, native, self.tree, &self.dims)? {
            self.rename_expr(&mut value);
            return Ok(vec![Stmt::Eval(value)]);
        }
        if let Some(mut value) = containers::value(root, native, self.tree, &self.dims, true)? {
            self.rename_expr(&mut value);
            return Ok(vec![Stmt::Eval(value)]);
        }
        if let celox_sv_syntax::ExprKind::Call {
            expression,
            arguments,
        } = &native
            .expression(expression_binding::ungroup(root, native))
            .kind
            && let Some((op, string)) =
                expression_binding::string_method(self.tree, *expression, native, &self.dims)
        {
            let (_, count) = StringOp::method(&op.to_string()).expect("a method name");
            let args = arguments
                .iter()
                .map(|arg| arg.value.filter(|_| arg.name.is_none()))
                .collect::<Option<Vec<_>>>()
                .filter(|args| args.len() == count)
                .ok_or_else(|| unsupported(format!("arguments of string method `{op}`")))?
                .into_iter()
                .map(|arg| expression_binding::value(arg, native, self.tree, &self.dims))
                .collect::<Result<Vec<_>, _>>()?;
            // A method that returns a value has no effect as a statement.
            return if op.updates_receiver() {
                Ok(vec![self.string_update(string, op, args, false)?])
            } else {
                Ok(Vec::new())
            };
        }
        let discarded = matches!(
            self.tree.declarations().expression(root).kind,
            celox_sv_syntax::ExprKind::Cast { .. }
        );
        {
            let query_root =
                expression_binding::call_statement_root(root, discarded, self.tree.declarations())?;
            if discarded
                && let Some((mut value, _)) =
                    array_reductions::bind(query_root, native, self.tree, &self.dims)?
            {
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            if discarded
                && let Some(mut value) =
                    containers::value(query_root, native, self.tree, &self.dims, true)?
            {
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            if matches!(
                native.expression(query_root).kind,
                celox_sv_syntax::ExprKind::RandomizeWith { .. }
            ) {
                let mut call = self.native_value(query_root)?;
                self.rename_expr(&mut call);
                return Ok(vec![Stmt::Eval(call)]);
            }
            if let Some(call) = self.method_call_statement(query_root)? {
                return Ok(vec![call]);
            }
            if let celox_sv_syntax::ExprKind::Call { expression, .. } =
                self.tree.declarations().expression(query_root).kind
                && matches!(
                    expression_binding::native_name(
                        &self.tree.state,
                        expression,
                        self.tree.declarations()
                    )
                    .as_deref(),
                    Some("randomize" | "std::randomize")
                )
            {
                let mut call = self.native_value(query_root)?;
                self.rename_expr(&mut call);
                return Ok(vec![Stmt::Eval(call)]);
            }
            if timescales::is_query(query_root, self.tree.declarations()) {
                let value = timescales::query(query_root, self.tree)?;
                return Ok(vec![Stmt::Eval(Expr::Literal(
                    parameters::format_typed_parameter_literal(i128::from(value), 32, true),
                ))]);
            }
            let signatures = if self.dims.subroutine_param_names.is_empty() {
                self.state.subroutine_params
            } else {
                &self.dims.subroutine_param_names
            };
            if let Some(mut call) = expression_binding::call_statement(
                root,
                discarded,
                self.tree.declarations(),
                self.tree,
                &self.dims,
                signatures,
            )? {
                self.rename_expr(&mut call);
                let Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order,
                } = call
                else {
                    unreachable!("ordinary native statement call")
                };
                return Ok(vec![Stmt::Call {
                    name,
                    args,
                    arg_order,
                }]);
            }
        }
        let root =
            expression_binding::call_statement_root(root, discarded, self.tree.declarations())?;
        let declarations = self.tree.declarations();
        let (callee, arguments) = match &declarations.expression(root).kind {
            celox_sv_syntax::ExprKind::Call {
                expression,
                arguments,
            } => (*expression, arguments.as_slice()),
            celox_sv_syntax::ExprKind::Name(_) => (root, &[][..]),
            _ => return Err(unsupported("system task or function name")),
        };
        let celox_sv_syntax::ExprKind::Name(name) = &declarations.expression(callee).kind else {
            return Err(unsupported("system task or function name"));
        };
        let presence = arguments
            .iter()
            .map(|arg| {
                if arg.value.is_some() {
                    system_functions::Arg::Given
                } else {
                    system_functions::Arg::Omitted
                }
            })
            .collect::<Vec<_>>();
        let args = (!arguments.iter().any(|arg| arg.name.is_some())).then_some(presence);
        let name = name.spelling.as_str();
        // `$cast` as a task reports a run-time error when the cast fails
        // (IEEE 1800-2023 6.24.2).
        if name == "$cast"
            && !discarded
            && let Some(cast) =
                expression_binding::object_cast(self.tree, arguments, declarations, &self.dims)?
        {
            let error = Stmt::SystemTask {
                task: SystemTask::Severity(system_functions::Severity::Error),
                site: source_span(declarations.expression(root).tokens, self.tree),
                assertion: None,
                args: vec![SystemTaskArg::Str(
                    "$cast of a class handle to an incompatible class".into(),
                )],
            };
            if let expression_binding::ObjectCast::Fails = cast {
                return Ok(vec![error]);
            }
            let mut succeeded = self.native_value(root)?;
            self.rename_expr(&mut succeeded);
            return Ok(vec![Stmt::If {
                condition: succeeded,
                then_body: Vec::new(),
                else_body: vec![error],
            }]);
        }
        let task = match system_functions::check_call(
            name,
            args.as_deref(),
            system_functions::CallSite::Statement(self.body),
        )? {
            system_functions::SystemCall::Function(_) => {
                // A system function called as a statement is
                // converted and checked like the same call in an
                // expression, then its value is discarded. `$bits`
                // and `$size` become constants and do not evaluate
                // their operands (IEEE 1800-2023 20.6.2).
                let mut value = expression_binding::bind_value(
                    root,
                    declarations,
                    self.tree,
                    &self.dims,
                    None,
                )?;
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            system_functions::SystemCall::Task(SystemTask::Printtimescale) => {
                return Ok(vec![Stmt::SystemTask {
                    task: SystemTask::Display(system_functions::Radix::Decimal),
                    site: source_span(declarations.expression(root).tokens, self.tree),
                    assertion: None,
                    args: vec![SystemTaskArg::Str(timescales::print_message(
                        root,
                        self.tree,
                        system_functions::CallSite::Statement(self.body),
                    )?)],
                }]);
            }
            system_functions::SystemCall::Task(task) => task,
        };
        let args = arguments
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                let Some(id) = arg.value else {
                    return Ok(SystemTaskArg::Empty);
                };
                if matches!(task, SystemTask::Vcd(system_functions::VcdTask::Vars)) && index > 0 {
                    let text = dump_selection_name(
                        id,
                        self.tree,
                        &self.dims,
                        &|value| self.rename_const(value),
                        &|name| {
                            self.scopes
                                .iter()
                                .any(|scope| scope.entries.iter().any(|entry| entry.source == name))
                        },
                    )?;
                    return Ok(SystemTaskArg::Str(text));
                }
                // Filenames use the normal packed-string expression binder,
                // including octal/hex escapes and multiline literal delimiters.
                if !matches!(task, SystemTask::Vcd(system_functions::VcdTask::File))
                    && let celox_sv_syntax::ExprKind::Literal(text) =
                        &declarations.expression(id).kind
                    && let Some(text) = text
                        .strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))
                {
                    return Ok(SystemTaskArg::Str(text.to_string()));
                }
                let mut value =
                    expression_binding::bind_value(id, declarations, self.tree, &self.dims, None)?;
                self.rename_expr(&mut value);
                Ok(SystemTaskArg::Expr(value))
            })
            .collect::<Result<Vec<_>, AnalyzerError>>()?;
        let mut args = args;
        // A string literal that %t converts is bound as a value: the binder
        // handles every literal form, triple quotes and escapes included.
        let mut bind = |index: usize| -> Result<Option<Expr>, AnalyzerError> {
            let Some(id) = arguments.get(index).and_then(|arg| arg.value) else {
                return Ok(None);
            };
            let mut value =
                expression_binding::bind_value(id, declarations, self.tree, &self.dims, None)?;
            self.rename_expr(&mut value);
            Ok(Some(value))
        };
        scale_time_conversions(
            task,
            &mut args,
            timescales::time_divisor(root, self.tree)?,
            &mut bind,
        )?;
        // Keep Veryl's assertion calls as calls: their message arguments
        // execute even on success, unlike an SV assertion's fail action.
        Ok(vec![Stmt::SystemTask {
            task,
            args,
            site: source_span(declarations.expression(root).tokens, self.tree),
            assertion: None,
        }])
    }

    fn fold_conditional(
        &mut self,
        branches: Vec<(Expr, Vec<Stmt>)>,
        mut else_body: Vec<Stmt>,
    ) -> Vec<Stmt> {
        for (condition, then_body) in branches.into_iter().rev() {
            // A generate index or parameter can make a procedural branch
            // unreachable. Remove its writes before checking process drivers.
            let constant = if inlining::calls_subroutine(&condition) {
                // Even a constant return value does not make the run-time
                // call's display, output or module-variable effects optional.
                None
            } else {
                expr_to_const(&self.tree.state, condition.clone()).and_then(|condition| {
                    eval_ast_const_expr(&self.tree.state, &condition, &self.dims.const_env)
                })
            };
            if let Some(value) = constant {
                // Implicit sensitivity is syntactic even for unreachable
                // branches. Preserve reads separately from executable writes.
                self.sensitivity_body.push(Stmt::Eval(condition.clone()));
                if value != 0 {
                    self.sensitivity_body.extend(else_body);
                    else_body = then_body;
                } else {
                    self.sensitivity_body.extend(then_body);
                }
                continue;
            }
            else_body = vec![Stmt::If {
                condition,
                then_body,
                else_body,
            }];
        }
        else_body
    }

    fn cond_predicate(
        &self,
        predicate: &celox_sv_syntax::Predicate,
    ) -> Result<Expr, AnalyzerError> {
        let [item] = predicate.items.as_slice() else {
            return Err(unsupported("`&&&` in a condition"));
        };
        if item.pattern.is_some() {
            return Err(unsupported("pattern matching in a condition"));
        }
        let condition = expression_binding::value(
            item.expression,
            self.tree.declarations(),
            self.tree,
            &self.dims,
        )?;
        let mut condition = statements::simplify_condition(&self.tree.state, condition, &self.dims);
        self.rename_expr(&mut condition);
        Ok(condition)
    }

    fn for_header(
        &mut self,
        header: &celox_sv_syntax::ForHeader,
    ) -> Result<(Vec<Stmt>, Option<Expr>, Vec<Stmt>), AnalyzerError> {
        use celox_sv_syntax::{ForInitialization, ForStep};
        let mut init = Vec::new();
        match &header.initialization {
            None => {}
            Some(ForInitialization::Declarations(groups)) => {
                for group in groups {
                    let ty = type_binding::procedural_type(
                        group.data_type,
                        self.tree.declarations(),
                        self.tree,
                        &self.dims.const_env,
                        &self.type_aliases,
                    )?
                    .ok_or_else(|| unsupported("for-loop variable type"))?;
                    for variable in &group.variables {
                        let source = instances::normalize_identifier(&variable.name.spelling);
                        let target = LValue::Ident(source.clone());
                        let mut scoped = self.dims.clone();
                        scoped.insert(source.clone(), dimensions_from_type(&ty));
                        let mut value = expression_binding::expression(
                            variable.value,
                            self.tree.declarations(),
                            &target,
                            self.tree,
                            &scoped,
                        )?;
                        self.rename_expr(&mut value);
                        let name = self.declare(&source, ty.clone());
                        init.push(Stmt::Local {
                            name,
                            init: Some(value),
                        });
                    }
                }
            }
            Some(ForInitialization::Assignments(values)) => {
                for value in values {
                    init.push(self.assignment_value("=", false, (value.left, value.right))?);
                }
            }
        }
        let condition = header
            .condition
            .map(|id| self.native_value(id))
            .transpose()?;
        let mut steps = Vec::new();
        for step in &header.steps {
            steps.push(match step {
                ForStep::Assignment { left, operator, right, .. } => self.assignment_value(operator, false, (*left, *right))?,
                ForStep::Expression(id) => {
                    if !matches!(&self.tree.declarations().expression(*id).kind,
                        celox_sv_syntax::ExprKind::Unary { operator, .. } if matches!(operator.as_str(), "++" | "--")) {
                        return Err(unsupported("subroutine call as a for-loop step"));
                    }
                    self.inc_or_dec(*id)?
                }
            });
        }
        Ok((init, condition, steps))
    }

    fn foreach_bound_indices(
        &mut self,
        array: String,
        variables: Vec<Option<String>>,
    ) -> Result<Vec<ForeachLoop>, AnalyzerError> {
        let dimensions = self
            .dims
            .get(&array)
            .cloned()
            .ok_or_else(|| unsupported(format!("foreach over `{array}`")))?;
        let bounds: Vec<(ConstExpr, ConstExpr)> = dimensions
            .unpacked
            .iter()
            .map(|dimension| (dimension.left.clone(), dimension.right.clone()))
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| (dimension.left.clone(), dimension.right.clone())),
            )
            .collect();
        let mut loops = Vec::new();
        for (position, variable) in variables.into_iter().enumerate() {
            let Some(variable) = variable else {
                continue;
            };
            let (left, right) = bounds
                .get(position)
                .ok_or_else(|| unsupported("foreach index beyond the array dimensions"))?;
            let left = eval_ast_const_expr(&self.tree.state, left, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let right = eval_ast_const_expr(&self.tree.state, right, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let name = self.declare(&variable, int_type());
            loops.push(ForeachLoop::Static(name, left, right));
        }
        Ok(loops)
    }
    fn fold_foreach(loops: Vec<ForeachLoop>, mut body: Vec<Stmt>) -> Vec<Stmt> {
        for bound in loops.into_iter().rev() {
            let (name, left, right) = match bound {
                ForeachLoop::Static(name, left, right) => (name, left, right),
                ForeachLoop::Container {
                    array,
                    index,
                    layout,
                    status,
                } => {
                    let index_expr = Expr::Ident(index.clone());
                    let (init, condition, step) = if let Some(status) = status {
                        let call = |op| {
                            containers::call(op, layout, vec![array.clone(), index_expr.clone()])
                        };
                        (
                            vec![
                                Stmt::Local {
                                    name: index,
                                    init: None,
                                },
                                Stmt::Local {
                                    name: status.clone(),
                                    init: Some(call(ContainerOp::First)),
                                },
                            ],
                            Expr::Ident(status.clone()),
                            vec![Stmt::Assign {
                                lhs: LValue::Ident(status),
                                rhs: call(ContainerOp::Next),
                                nonblocking: false,
                            }],
                        )
                    } else {
                        (
                            vec![Stmt::Local {
                                name: index.clone(),
                                init: Some(Expr::Literal("0".into())),
                            }],
                            Expr::Binary {
                                left: Box::new(index_expr.clone()),
                                op: BinaryOp::Lt,
                                right: Box::new(containers::call(
                                    ContainerOp::Size,
                                    layout,
                                    vec![array],
                                )),
                            },
                            vec![Stmt::Assign {
                                lhs: LValue::Ident(index),
                                rhs: Expr::Binary {
                                    left: Box::new(index_expr),
                                    op: BinaryOp::Add,
                                    right: Box::new(Expr::Literal("1".into())),
                                },
                                nonblocking: false,
                            }],
                        )
                    };
                    body = vec![Stmt::Loop {
                        kind: LoopKind::For,
                        init,
                        condition: Some(condition),
                        step,
                        body,
                    }];
                    continue;
                }
            };
            let index = Expr::Ident(name.clone());
            let (compare, step) = if left <= right {
                (BinaryOp::Le, BinaryOp::Add)
            } else {
                (BinaryOp::Ge, BinaryOp::Sub)
            };
            body = vec![Stmt::Loop {
                kind: LoopKind::For,
                init: vec![Stmt::Local {
                    name: name.clone(),
                    init: Some(Expr::Literal(left.to_string())),
                }],
                condition: Some(Expr::Binary {
                    left: Box::new(index.clone()),
                    op: compare,
                    right: Box::new(Expr::Literal(right.to_string())),
                }),
                step: vec![Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs: Expr::Binary {
                        left: Box::new(index),
                        op: step,
                        right: Box::new(Expr::Literal("1".to_string())),
                    },
                    nonblocking: false,
                }],
                body,
            }];
        }
        body
    }
}

/// The default failure action of a native immediate assertion.
/// The default fail action of an `assert` or `assume`: a call to `$error`
/// at the assertion (IEEE 1800-2023 20.10).
fn assertion_failure(keyword: &str, site: Option<crate::procedural::SourceSpan>) -> Vec<Stmt> {
    vec![Stmt::SystemTask {
        task: SystemTask::Severity(system_functions::Severity::Error),
        args: vec![SystemTaskArg::Str(format!("{keyword} failed"))],
        site,
        assertion: None,
    }]
}

/// Check the action of a deferred assertion: one subroutine call (IEEE
/// 1800-2023 16.4). Only display and severity tasks are supported, since
/// a user subroutine would run in the Reactive region and could wake
/// processes there.
fn deferred_action(body: &[Stmt]) -> Result<(), AnalyzerError> {
    match body {
        [] => Ok(()),
        [
            Stmt::SystemTask {
                task:
                    SystemTask::Display(_)
                    | SystemTask::Write(_)
                    | SystemTask::Severity(_)
                    | SystemTask::Fatal,
                ..
            },
        ] => Ok(()),
        _ => Err(unsupported(
            "deferred assertion action other than one display or severity task call",
        )),
    }
}

/// Attribute the severity tasks of an assertion's action block to it, and
/// for a deferred assertion its display tasks, which wait in the same
/// queue. Tasks in subroutines the block calls are not attributed, and the
/// tasks of a nested assertion keep its own attribution.
fn attribute_reports(
    body: &mut [Stmt],
    assertion: &celox_design::AssertionSite,
    outcome: celox_design::AssertionOutcome,
) {
    for stmt in body {
        stmt.walk_mut(&mut |stmt| {
            let deferred = assertion.deferral != celox_design::AssertionDeferral::None;
            if let Stmt::SystemTask {
                task,
                assertion: attributed @ None,
                ..
            } = stmt
                && (matches!(task, SystemTask::Severity(_) | SystemTask::Fatal)
                    || deferred && matches!(task, SystemTask::Display(_) | SystemTask::Write(_)))
            {
                *attributed = Some(celox_design::AssertionSite {
                    outcome,
                    ..assertion.clone()
                });
            }
        });
    }
}

/// The declared arguments of a task or function: `(name, direction, type,
/// default)`. An argument without a direction takes the previous one's, and
/// one without a type the previous one's type (IEEE 1800-2023 13.3).
pub(super) fn subroutine_param_declarations(
    native: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<
    Vec<(
        String,
        ParamDirection,
        Type,
        Option<celox_sv_syntax::ExprId>,
    )>,
> {
    type_binding::subroutine_formals(
        native,
        tree,
        &PackedDimensions::new(HashMap::default(), env, aliases),
    )
}

/// The argument names of every function and task in the active generate
/// items, in declaration order, keyed by their (qualified) names.
pub(super) fn subroutine_argument_names(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<
    (
        HashMap<String, Vec<String>>,
        HashMap<String, Vec<VariableDimensions>>,
        lifetimes::Directions,
    ),
    AnalyzerError,
> {
    let mut names = HashMap::default();
    let mut shapes = HashMap::default();
    let mut directions = HashMap::default();
    for item in generate::subroutine_items(tree, const_env, type_aliases)? {
        let _interface_scope = item.interface_scope();
        for native_id in subroutine_owners::item(item.id, tree)? {
            let native = tree.declarations().subroutine(native_id);
            let _constant_functions = item.enter();
            let params = subroutine_param_declarations(
                native,
                tree,
                &item.env,
                &item.type_aliases(type_aliases),
            )?;
            let qualified = item.name(&instances::normalize_identifier(&native.name.spelling));
            let param_shapes: Vec<VariableDimensions> = params
                .iter()
                .map(|(_, _, r#type, _)| {
                    dimensions_from_type(&scoped_type(r#type.clone(), &item.env))
                })
                .collect();
            let param_directions: Vec<ParamDirection> =
                params.iter().map(|(_, direction, ..)| *direction).collect();
            directions.insert(qualified.clone(), param_directions);
            let params: Vec<String> = params.into_iter().map(|(name, ..)| name).collect();
            names.insert(qualified.clone(), params);
            shapes.insert(qualified, param_shapes);
        }
        for class in classes::item_classes(item.id, tree) {
            let _constant_functions = item.enter();
            let aliases = item.type_aliases(type_aliases);
            // A specialization's methods see its parameters (8.25).
            let (env, aliases) = classes::specializations::scope(tree, &class, &item.env, &aliases);
            let handle = classes::handle_type(tree, &class)
                .ok_or_else(|| unsupported(format!("class `{class}`")))?;
            let mut constructor = false;
            let this = (
                classes::THIS.to_owned(),
                ParamDirection::Input,
                handle.clone(),
                None,
            );
            let mut methods = classes::declared_methods(tree, &class)?
                .into_iter()
                .map(|(native_id, is_static)| {
                    let name = instances::normalize_identifier(
                        &tree.declarations().subroutine(native_id).name.spelling,
                    );
                    (
                        classes::Class::subroutine_name(&class, &name),
                        native_id,
                        is_static,
                    )
                })
                .collect::<Vec<_>>();
            // A class that cannot be laid out reports why where it is
            // lowered.
            let layout = classes::class(tree, &class, &env, &aliases).ok();
            if let Some(layout) = &layout {
                methods.extend(layout.dispatched_methods().map(|method| {
                    (
                        classes::Class::dispatcher_name(&class, &method.name),
                        method.native,
                        false,
                    )
                }));
            }
            for (name, native_id, is_static) in methods {
                let native = tree.declarations().subroutine(native_id);
                let mut params = subroutine_param_declarations(native, tree, &env, &aliases)?;
                constructor |= native.kind == celox_sv_syntax::SubroutineKind::Constructor;
                if !is_static {
                    params.insert(0, this.clone());
                }
                let qualified = item.name(&name);
                directions.insert(
                    qualified.clone(),
                    params.iter().map(|(_, direction, ..)| *direction).collect(),
                );
                shapes.insert(
                    qualified.clone(),
                    params
                        .iter()
                        .map(|(_, _, r#type, _)| {
                            dimensions_from_type(&scoped_type(r#type.clone(), &env))
                        })
                        .collect(),
                );
                names.insert(
                    qualified,
                    params.into_iter().map(|(name, ..)| name).collect(),
                );
            }
            for method in ["randomize", "srandom", "rand_mode"] {
                let qualified = item.name(&classes::Class::subroutine_name(&class, method));
                let mut param_names = vec![classes::THIS.to_owned()];
                let mut param_shapes = vec![dimensions_from_type(&handle)];
                let (argument, ty) = match method {
                    "randomize" => (
                        "$selection",
                        layout.as_ref().map_or_else(
                            || classes::unsigned_type(1),
                            |layout| layout.selection_type(),
                        ),
                    ),
                    "srandom" => ("seed", int_type()),
                    _ => ("on_off", classes::unsigned_type(1)),
                };
                param_names.push(argument.into());
                param_shapes.push(dimensions_from_type(&ty));
                directions.insert(
                    qualified.clone(),
                    vec![ParamDirection::Input; param_names.len()],
                );
                shapes.insert(qualified.clone(), param_shapes);
                names.insert(qualified, param_names);
            }
            let handle = dimensions_from_type(&scoped_type(handle, &env));
            if !constructor && !layout.is_some_and(|layout| layout.is_interface) {
                let qualified = item.name(&classes::Class::subroutine_name(&class, "new"));
                directions.insert(qualified.clone(), vec![ParamDirection::Input]);
                shapes.insert(qualified.clone(), vec![handle.clone()]);
                names.insert(qualified, vec![classes::THIS.to_owned()]);
            }
            let qualified = item.name(&classes::Class::cast_name(&class));
            directions.insert(
                qualified.clone(),
                vec![ParamDirection::Inout, ParamDirection::Input],
            );
            shapes.insert(qualified.clone(), vec![handle.clone(), handle]);
            names.insert(
                qualified,
                vec![CAST_DESTINATION.to_owned(), CAST_SOURCE.to_owned()],
            );
        }
    }
    Ok((names, shapes, directions))
}

/// Every function and task of the active generate items, with statement
/// bodies.
pub(super) fn subroutines_from_scope(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    subroutines_from_scope_with(
        tree,
        const_env,
        packed_dimensions,
        parameter_literals,
        state,
        None,
    )
}

/// The subroutines of a module. With `rejected`, a subroutine that cannot be
/// lowered is left out and the reason recorded under its name, instead of
/// failing.
pub(super) fn subroutines_from_scope_with(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    rejected: Option<&mut HashMap<String, AnalyzerError>>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    subroutines_with_symbolic_parameters(
        tree,
        const_env,
        packed_dimensions,
        parameter_literals,
        state,
        rejected,
        &HashSet::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn subroutines_with_symbolic_parameters(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    mut rejected: Option<&mut HashMap<String, AnalyzerError>>,
    symbolic_parameters: &HashSet<String>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    let module_type_aliases = &packed_dimensions.type_aliases;
    let mut subroutines = Vec::new();
    let active = generate::subroutine_items(tree, const_env, module_type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for item in &active {
        let _interface_scope = item.interface_scope();
        if item.is_parameter_declaration(tree.declarations()) {
            continue;
        }
        for native_id in subroutine_owners::item(item.id, tree)? {
            let (item_dimensions, literals) = views.get(item);
            let item_dimensions = if symbolic_parameters.is_empty() {
                std::borrow::Cow::Borrowed(item_dimensions)
            } else {
                let mut dimensions = item_dimensions.clone();
                for parameter in symbolic_parameters {
                    dimensions.const_env.remove(parameter);
                    dimensions.parameter_values.remove(parameter);
                }
                std::borrow::Cow::Owned(dimensions)
            };
            let item_dimensions = &*item_dimensions;

            let declaration_env = if symbolic_parameters.is_empty() {
                std::borrow::Cow::Borrowed(&*item.env)
            } else {
                std::borrow::Cow::Owned(
                    item.env
                        .iter()
                        .filter(|(name, _)| !symbolic_parameters.contains(*name))
                        .map(|(name, value)| (name.clone(), *value))
                        .collect::<HashMap<_, _>>(),
                )
            };

            let _constant_functions = item.enter();
            let type_aliases = item.type_aliases(module_type_aliases);
            let name = instances::normalize_identifier(
                &tree.declarations().subroutine(native_id).name.spelling,
            );
            let lowered = lower_subroutine(
                native_id,
                tree,
                &declaration_env,
                &type_aliases,
                item_dimensions,
                literals,
                state,
                item,
                None,
                None,
            );
            let subroutine = match (lowered, rejected.as_deref_mut()) {
                (Ok(subroutine), _) => subroutine,
                (Err(error), Some(rejected)) => {
                    rejected.insert(name, error);
                    continue;
                }
                (Err(error), None) => return Err(error),
            };
            if subroutines
                .iter()
                .any(|other: &Subroutine| other.name == subroutine.name)
            {
                return Err(unsupported(format!(
                    "duplicate function declaration `{}`",
                    subroutine.name
                )));
            }
            subroutines.push(subroutine);
        }
        for class in classes::item_classes(item.id, tree) {
            let (item_dimensions, literals) = views.get(item);
            let _constant_functions = item.enter();
            let type_aliases = item.type_aliases(module_type_aliases);
            match (
                lower_class(
                    &class,
                    tree,
                    &item.env,
                    &type_aliases,
                    item_dimensions,
                    literals,
                    state,
                    item,
                ),
                rejected.as_deref_mut(),
            ) {
                (Ok(methods), _) => subroutines.extend(methods),
                (Err(error), Some(rejected)) => {
                    rejected.insert(class, error);
                }
                (Err(error), None) => return Err(error),
            }
        }
    }
    // A class of the scope, or of another package, that overrides a virtual
    // method of a class that another scope declares is reached through that
    // method's dispatcher, which the declaring scope lowered for the classes
    // it saw: the scope lowers the dispatcher again, over every class it
    // sees, when the dispatcher misses one (IEEE 1800-2023 8.20).
    let imported = scope::imported(&tree.state);
    for class in &imported.classes {
        for method in class.dispatched_methods() {
            let name = classes::Class::dispatcher_name(&class.name, &method.name);
            if subroutines.iter().any(|known| known.name == name) {
                continue;
            }
            let Some(dispatcher) = imported.subroutines.iter().find(|known| known.name == name)
            else {
                continue;
            };
            let targets = classes::dispatch_targets(
                tree,
                &class.name,
                &method.name,
                const_env,
                module_type_aliases,
            )?;
            let covered = case_ids(&dispatcher.body);
            if targets
                .iter()
                .all(|(_, ids)| ids.iter().all(|id| covered.contains(id)))
            {
                continue;
            }
            let mut dispatcher = dispatcher.clone();
            dispatcher.body = vec![dispatch_body(
                &targets,
                &dispatcher.params,
                dispatcher.return_var.as_deref(),
            )];
            subroutines.push(dispatcher);
        }
        // So does a `$cast` to a variable of the class (8.16).
        let name = classes::Class::cast_name(&class.name);
        if subroutines.iter().any(|known| known.name == name) {
            continue;
        }
        let Some(cast) = imported.subroutines.iter().find(|known| known.name == name) else {
            continue;
        };
        let targets = classes::cast_targets(tree, &class.name, const_env, module_type_aliases)?;
        let covered = case_ids(&cast.body);
        if targets.iter().all(|id| covered.contains(id)) {
            continue;
        }
        let mut cast = cast.clone();
        if let Some(Stmt::Case { items, .. }) = cast.body.first_mut()
            && let Some(item) = items.first_mut()
        {
            item.labels = std::iter::once(0)
                .chain(targets)
                .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                .collect();
            subroutines.push(cast);
        }
    }
    Ok(subroutines)
}

/// The class identifiers that the case on the class of an object in `body`,
/// the body of a dispatcher or a `$cast` helper, selects.
fn case_ids(body: &[Stmt]) -> HashSet<u32> {
    let Some(Stmt::Case { items, .. }) = body.first() else {
        return HashSet::default();
    };
    items
        .iter()
        .flat_map(|item| &item.labels)
        .filter_map(|label| match label {
            CaseLabel::Value(Expr::Literal(literal)) => literal.strip_prefix("32'd")?.parse().ok(),
            _ => None,
        })
        .collect()
}

/// Shared native subroutine lowering for execution and expression-form plans.
/// Native ownership/control is identical in both consumers; typed headers remain
/// borrowed operands until their independent type kernels are migrated.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_subroutine(
    native_id: celox_sv_syntax::SubroutineId,
    tree: &AnalysisContext<'_>,
    declaration_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    item_dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    item: &generate::Item,
    class: Option<&classes::Class>,
    dispatch: Option<&[(String, Vec<u32>)]>,
) -> Converted<Subroutine> {
    let native = tree.declarations().subroutine(native_id);
    let name = instances::normalize_identifier(&native.name.spelling);
    let constructor = native.kind == celox_sv_syntax::SubroutineKind::Constructor;
    let is_static = dispatch.is_none()
        && class.is_some_and(|class| {
            class
                .methods
                .iter()
                // An inherited method of a class of another source may
                // have the same identifier.
                .any(|method| {
                    method.native == native_id
                        && method.owner == class.name
                        && method.qualifiers.is_static
                })
        });
    let handle = class
        .map(|class| {
            classes::handle_type(tree, &class.name)
                .ok_or_else(|| unsupported(format!("class `{}`", class.name)))
        })
        .transpose()?;
    let params = subroutine_param_declarations(native, tree, declaration_env, type_aliases)?;
    let mut formal_names = HashSet::default();
    if let Some((name, ..)) = params
        .iter()
        .find(|(name, ..)| !formal_names.insert(name.as_str()))
    {
        return Err(unsupported(format!("duplicate function argument `{name}`")));
    }
    let return_type = match native.return_type {
        _ if constructor => handle.clone(),
        Some(id)
            if matches!(
                tree.declarations().data_type(id).kind,
                celox_sv_syntax::DataTypeKind::Implicit
            ) =>
        {
            Some(type_binding::implicit_subroutine_type(
                id,
                tree,
                declaration_env,
                type_aliases,
            )?)
        }
        Some(id) => type_binding::procedural_type(
            id,
            tree.declarations(),
            tree,
            declaration_env,
            type_aliases,
        )?,
        None => None,
    };
    let mut builder = BodyBuilder::new(
        tree,
        item_dimensions,
        state,
        system_functions::Body::Subroutine,
    );
    builder.task_timing = native.kind == celox_sv_syntax::SubroutineKind::Task;
    let first_local = builder.state.locals.len();
    builder.automatic = match native.lifetime.as_deref() {
        Some("automatic") => true,
        Some("static") => false,
        Some(_) => {
            return Err(AnalyzerError::Parse(
                "unknown native subroutine lifetime".into(),
            ));
        }
        // Methods are automatic (IEEE 1800-2023 8.6).
        None => builder.automatic || class.is_some(),
    };
    let first_static = builder.state.statics.len();
    builder.push_scope();
    if let Some(class) = class {
        builder.enter_class_scope(class);
    }
    // Defaults bind in the enclosing declaration scope, before
    // any formal can shadow those names (IEEE 1800-2023 13.5.3).
    let defaults = params
        .iter()
        .map(|(_, _, ty, default)| {
            default
                .map(|root| {
                    let shape = ty.tagged_identity().map(|_| dimensions_from_type(ty));
                    let mut value = expression_binding::bind_value(
                        root,
                        tree.declarations(),
                        tree,
                        &builder.dims,
                        shape.as_ref(),
                    )?;
                    builder.rename_expr(&mut value);
                    Ok(value)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lowered_params = Vec::new();
    let mut formal_types = HashMap::default();
    // A method that is not static receives the handle of its object first,
    // and a constructor returns the handle of the object it initializes.
    let this = handle.clone().filter(|_| !is_static).map(|handle| {
        (
            classes::THIS.to_owned(),
            ParamDirection::Input,
            handle,
            None,
        )
    });
    let defaults = this
        .iter()
        .map(|_| None)
        .chain(defaults)
        .collect::<Vec<_>>();
    for ((source, direction, r#type, _), default) in this.into_iter().chain(params).zip(defaults) {
        let name = builder.declare(&source, r#type.clone());
        formal_types.insert(name.clone(), scoped_type(r#type.clone(), declaration_env));
        lowered_params.push(SubroutineParam {
            name,
            source_name: source,
            direction,
            r#type: crate::ir::Type::from_ast(
                scoped_type(r#type, declaration_env),
                &const_functions::Environment {
                    constants: &*item.env,
                    functions: &tree.state.const_functions,
                },
            ),
            default,
        });
    }
    let return_var = return_type
        .as_ref()
        .map(|r#type| builder.declare(&name, r#type.clone()));
    if let (Some(name), Some(ty)) = (&return_var, &return_type) {
        formal_types.insert(name.clone(), scoped_type(ty.clone(), declaration_env));
    }
    builder.return_shape = return_type
        .as_ref()
        .filter(|ty| ty.tagged_identity().is_some())
        .map(dimensions_from_type);
    let parameter_locals_end = builder.state.locals.len();
    let mut items = native.items.clone();
    let mut prologue = Vec::new();
    if let (Some(class), Some(result), Some(this)) = (class, &return_var, lowered_params.first())
        && constructor
    {
        // An explicit `super.new` call is made before the property
        // initializers, with the base call the prologue makes.
        let arguments = match (
            super_new(tree.declarations(), &items),
            classes::base_arguments(tree, &class.name),
        ) {
            (Some(_), Some(_)) => {
                return Err(unsupported(
                    "`super.new` in a class whose `extends` passes constructor arguments",
                ));
            }
            (Some(_), None) if class.base.is_none() => {
                return Err(unsupported("`super.new` in a class without a base"));
            }
            (Some((index, arguments)), None) => {
                items.remove(index);
                arguments
            }
            (None, arguments) => arguments.unwrap_or_default(),
        };
        prologue = builder.constructor_prologue(class, &this.name, result, arguments)?;
    }
    let mut body = prologue;
    match dispatch {
        Some(targets) => body.push(dispatch_body(
            targets,
            &lowered_params,
            return_var.as_deref(),
        )),
        None => body.extend(builder.native_body(&items, &native.authored_statements)?),
    }
    builder.pop_scope();
    let static_types: HashMap<String, Type> = builder
        .state
        .statics
        .split_off(first_static)
        .into_iter()
        .collect();
    let statics = static_types.keys().cloned().collect::<HashSet<_>>();
    // A constructor's `return;` returns the object (IEEE 1800-2023 8.7).
    if return_var.is_some() && !constructor {
        let mut expressionless = false;
        for stmt in &body {
            stmt.walk(&mut |stmt| expressionless |= matches!(stmt, Stmt::Return(None)));
        }
        if expressionless {
            return Err(unsupported("expressionless function return"));
        }
    }
    let mut subroutine = Subroutine {
        name: item.name(&match class {
            Some(class) if dispatch.is_some() => {
                classes::Class::dispatcher_name(&class.name, &name)
            }
            Some(class) => classes::Class::subroutine_name(&class.name, &name),
            None => name,
        }),
        is_task: native.kind == celox_sv_syntax::SubroutineKind::Task,
        automatic_locals: builder.state.locals[first_local..]
            .iter()
            .enumerate()
            .filter(|(index, local)| {
                if first_local + index < parameter_locals_end {
                    builder.automatic
                } else {
                    !statics.contains(&local.name)
                }
            })
            .map(|(_, local)| local.name.clone())
            .collect(),
        persistent_locals: Vec::new(),
        return_type: return_type.map(|r#type| {
            crate::ir::Type::from_ast(
                scoped_type(r#type, declaration_env),
                &const_functions::Environment {
                    constants: &*item.env,
                    functions: &tree.state.const_functions,
                },
            )
        }),
        params: lowered_params,
        return_var,
        body,
        sensitivity_body: std::mem::take(&mut builder.sensitivity_body),
    };
    for stmt in subroutine
        .body
        .iter_mut()
        .chain(&mut subroutine.sensitivity_body)
    {
        qualify_stmt(item, stmt);
        substitute_stmt_constants(&tree.state, stmt, declaration_env, literals);
    }
    for param in &mut subroutine.params {
        if let Some(default) = &mut param.default {
            item.expr(default);
            *default = substitute_expr_constants_with_parameter_literals(
                &tree.state,
                default.clone(),
                &item.env,
                literals,
            );
        }
    }
    if subroutine.is_task {
        lifetimes::check_subroutine_statics(
            &tree.state,
            &subroutine,
            &builder.dims.const_env,
            builder.state.locals,
            &statics,
            builder.state.subroutine_directions,
        )?;
        if !builder.automatic {
            lifetimes::check_static_result(
                &tree.state,
                &builder.dims.const_env,
                builder.state.locals,
                &subroutine,
                builder.state.subroutine_directions,
            )?;
        }
    } else if !builder.state.constant_evaluation {
        let retain_result = !builder.automatic
            && lifetimes::check_static_result(
                &tree.state,
                &builder.dims.const_env,
                builder.state.locals,
                &subroutine,
                builder.state.subroutine_directions,
            )
            .is_err();
        // Keep scratch arrays/bitwise result proofs on the existing temporary path.
        let retained_types = static_types
            .iter()
            .filter(|(name, _)| {
                lifetimes::check_subroutine_statics(
                    &tree.state,
                    &subroutine,
                    &builder.dims.const_env,
                    builder.state.locals,
                    &std::iter::once((*name).clone()).collect(),
                    builder.state.subroutine_directions,
                )
                .is_err()
            })
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let (mut kept, mut initializers) = lifetimes::keep_static_locals(
            &tree.state,
            &retained_types,
            builder.state.subroutine_directions,
            std::iter::once(&mut subroutine.body),
        )?;
        if retain_result {
            kept.extend(subroutine.return_var.clone());
        }
        for param in &subroutine.params {
            if param.direction == ParamDirection::Output
                && !subroutine.automatic_locals.contains(&param.name)
                && lifetimes::output_retains_value(
                    &subroutine,
                    &param.name,
                    builder.state.subroutine_directions,
                )
            {
                kept.push(param.name.clone());
            }
        }
        for initializer in &mut initializers {
            qualify_stmt(item, initializer);
            substitute_stmt_constants(&tree.state, initializer, declaration_env, literals);
        }
        for name in &kept {
            let ty = static_types
                .get(name)
                .or_else(|| formal_types.get(name))
                .expect("function storage has a local type");
            builder.state.statics.push((name.clone(), ty.clone()));
        }
        builder.state.static_initializers.extend(initializers);
        subroutine.persistent_locals = kept;
    }
    Ok(subroutine)
}

/// The body of a dispatcher of a virtual method: a call of the override in
/// `targets` for the class of `this`, the first of `params`, passing the
/// formals on (IEEE 1800-2023 8.20). Every class an object can have is in
/// `targets`, so any other class is that of `null`, whose dereference is an
/// error (8.4).
fn dispatch_body(
    targets: &[(String, Vec<u32>)],
    params: &[SubroutineParam],
    result: Option<&str>,
) -> Stmt {
    let this = Expr::Ident(params[0].name.clone());
    let args = params
        .iter()
        .map(|param| Some(Expr::Ident(param.name.clone())))
        .collect::<Vec<_>>();
    let items = targets
        .iter()
        .map(|(target, classes)| {
            let call = match result {
                Some(result) => Stmt::Assign {
                    lhs: LValue::Ident(result.to_owned()),
                    rhs: Expr::Call {
                        callee: Callee::Subroutine(target.clone()),
                        args: args.clone(),
                        arg_order: Vec::new(),
                    },
                    nonblocking: false,
                },
                None => Stmt::Call {
                    name: target.clone(),
                    args: args.clone(),
                    arg_order: Vec::new(),
                },
            };
            CaseItemBase {
                labels: classes
                    .iter()
                    .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                    .collect(),
                body: vec![call],
            }
        })
        .collect();
    let null_dereference = classes::object_call(
        system_functions::ObjectOp::Load(system_functions::ObjectField {
            offset: 0,
            width: 8,
            signed: false,
            handle: None,
        }),
        vec![this.clone()],
    );
    Stmt::Case {
        kind: CaseKind::Exact,
        selector: classes::object_call(system_functions::ObjectOp::ClassOf, vec![this]),
        items,
        default: Some(vec![Stmt::Eval(null_dereference)]),
    }
}

/// The `super.new(args)` call that starts constructor body `items`, as its
/// index in `items` and its arguments (IEEE 1800-2023 8.15, 8.17).
fn super_new<'a>(
    native: &'a celox_sv_syntax::Declarations,
    items: &[celox_sv_syntax::StatementId],
) -> Option<(usize, &'a [celox_sv_syntax::Argument])> {
    use celox_sv_syntax::{ExprKind, StatementKind};
    let (index, statement) = items.iter().enumerate().find(|(_, id)| {
        !matches!(
            native.statement(**id).kind,
            StatementKind::Declaration(_) | StatementKind::Empty
        )
    })?;
    let StatementKind::Expression(root) = native.statement(*statement).kind else {
        return None;
    };
    let root = expression_binding::ungroup(root, native);
    let (callee, arguments) = match &native.expression(root).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (root, &[][..]),
    };
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return None;
    };
    (separator == "."
        && name.spelling == "new"
        && matches!(&native.expression(*expression).kind,
            ExprKind::Name(name) if name.spelling == "super"))
    .then_some((index, arguments))
}

impl BodyBuilder<'_, '_, '_> {
    /// The statements a constructor of `class` runs before its body, on the
    /// object `this` its caller allocated: return `this` as `result`, call
    /// the constructor of the base with `arguments`, and initialize the
    /// properties the class declares (IEEE 1800-2023 8.7, 8.8, 8.17).
    fn constructor_prologue(
        &mut self,
        class: &classes::Class,
        this: &str,
        result: &str,
        arguments: &[celox_sv_syntax::Argument],
    ) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let mut body = vec![Stmt::Assign {
            lhs: LValue::Ident(result.to_owned()),
            rhs: Expr::Ident(this.to_owned()),
            nonblocking: false,
        }];
        if let Some(base) = &class.base {
            let base = classes::class(
                self.tree,
                base,
                &self.dims.const_env,
                &self.dims.type_aliases,
            )?;
            if let Some(method) = base.method("new") {
                expression_binding::check_member_access(
                    self.tree,
                    &self.dims,
                    &method.owner,
                    &method.name,
                    method.qualifiers.local,
                    method.qualifiers.protected,
                )?;
            }
            let mut args = vec![Some(Expr::Ident(this.to_owned()))];
            let (actuals, order) = base.constructor_arguments(arguments)?;
            for actual in actuals {
                args.push(
                    actual
                        .map(|value| {
                            let mut value =
                                expression_binding::value(value, native, self.tree, &self.dims)?;
                            self.rename_expr(&mut value);
                            Ok::<_, AnalyzerError>(value)
                        })
                        .transpose()?,
                );
            }
            body.push(Stmt::Call {
                name: classes::Class::subroutine_name(&base.name, "new"),
                args,
                arg_order: std::iter::once(0)
                    .chain(order.into_iter().map(|index| index + 1))
                    .collect(),
            });
        }
        for property in class.own_properties() {
            let Some(initializer) = property.initializer else {
                continue;
            };
            let mut value = match property.r#type.class() {
                Some(class) => expression_binding::object_constructor(
                    self.tree,
                    initializer,
                    native,
                    &self.dims,
                    class,
                )?,
                None => None,
            };
            let mut value = match value.take() {
                Some(value) => value,
                None => expression_binding::bind_value(
                    initializer,
                    native,
                    self.tree,
                    &self.dims,
                    Some(&dimensions_from_type(&property.r#type)),
                )?,
            };
            if property.field.handle == Some(system_functions::HandleProperty::String) {
                value = expression_binding::string_operand(value, &self.dims);
            }
            self.rename_expr(&mut value);
            body.push(Stmt::Eval(classes::object_call(
                system_functions::ObjectOp::Store(property.field),
                vec![Expr::Ident(this.to_owned()), value],
            )));
        }
        Ok(body)
    }
}

/// The methods of class `name` as subroutines, with an implicit constructor
/// when the class declares none (IEEE 1800-2023 8.7).
#[allow(clippy::too_many_arguments)]
fn lower_class(
    name: &str,
    tree: &AnalysisContext<'_>,
    declaration_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    item_dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    item: &generate::Item,
) -> Converted<Vec<Subroutine>> {
    // A specialization's methods see its parameters (IEEE 1800-2023 8.25).
    let (declaration_env, type_aliases) =
        classes::specializations::scope(tree, name, declaration_env, type_aliases);
    let (item_dimensions, literals) =
        classes::specializations::body_scope(tree, name, item_dimensions, literals);
    let (declaration_env, type_aliases, item_dimensions, literals) =
        (&declaration_env, &type_aliases, &item_dimensions, &literals);
    let class = match classes::class(tree, name, declaration_env, type_aliases) {
        Ok(class) => class,
        // An interface class that cannot be laid out has no subroutines; a
        // use of its handles reports why (IEEE 1800-2023 8.26).
        Err(AnalyzerError::Unsupported(_)) if classes::is_interface(tree, name) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let mut subroutines = Vec::new();
    let mut constructor = false;
    for (id, _) in classes::declared_methods(tree, name)? {
        constructor |=
            tree.declarations().subroutine(id).kind == celox_sv_syntax::SubroutineKind::Constructor;
        subroutines.push(lower_subroutine(
            id,
            tree,
            declaration_env,
            type_aliases,
            item_dimensions,
            literals,
            state,
            item,
            Some(&class),
            None,
        )?);
    }
    for method in class.dispatched_methods() {
        let targets =
            classes::dispatch_targets(tree, name, &method.name, declaration_env, type_aliases)?;
        subroutines.push(lower_subroutine(
            method.native,
            tree,
            declaration_env,
            type_aliases,
            item_dimensions,
            literals,
            state,
            item,
            Some(&class),
            Some(&targets),
        )?);
    }
    if !constructor && !class.is_interface {
        let handle = classes::handle_type(tree, name)
            .ok_or_else(|| unsupported(format!("class `{name}`")))?;
        let ir_type = crate::ir::Type::from_ast(
            scoped_type(handle.clone(), declaration_env),
            &const_functions::Environment {
                constants: &*item.env,
                functions: &tree.state.const_functions,
            },
        );
        let mut builder = BodyBuilder::new(
            tree,
            item_dimensions,
            state,
            system_functions::Body::Subroutine,
        );
        builder.automatic = true;
        let first_local = builder.state.locals.len();
        builder.push_scope();
        builder.enter_class_scope(&class);
        let this = builder.declare(classes::THIS, handle.clone());
        let result = builder.declare("new", handle);
        let body = builder.constructor_prologue(
            &class,
            &this,
            &result,
            classes::base_arguments(tree, name).unwrap_or_default(),
        )?;
        builder.pop_scope();
        let mut subroutine = Subroutine {
            name: item.name(&classes::Class::subroutine_name(name, "new")),
            is_task: false,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: Some(ir_type.clone()),
            params: vec![SubroutineParam {
                name: this,
                source_name: classes::THIS.to_owned(),
                direction: ParamDirection::Input,
                r#type: ir_type,
                default: None,
            }],
            return_var: Some(result),
            body,
            sensitivity_body: Vec::new(),
        };
        for stmt in &mut subroutine.body {
            qualify_stmt(item, stmt);
            substitute_stmt_constants(&tree.state, stmt, declaration_env, literals);
        }
        subroutines.push(subroutine);
    }
    subroutines.push(lower_cast(
        name,
        tree,
        declaration_env,
        type_aliases,
        item_dimensions,
        state,
        item,
    )?);
    subroutines.extend(randomization::lower(
        &class,
        tree,
        item_dimensions,
        state,
        item,
    )?);
    Ok(subroutines)
}

/// The formals of [`classes::Class::cast_name`].
pub(super) const CAST_DESTINATION: &str = "destination";
pub(super) const CAST_SOURCE: &str = "source";

/// The subroutine `$cast` to a variable of class `name` calls: it assigns
/// the source handle to the destination and returns 1 when the source is
/// `null` or its object's class derives from or implements the class, and
/// otherwise returns 0 (IEEE 1800-2023 8.16).
#[allow(clippy::too_many_arguments)]
fn lower_cast(
    name: &str,
    tree: &AnalysisContext<'_>,
    declaration_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    item_dimensions: &PackedDimensions,
    state: &mut BodyState<'_>,
    item: &generate::Item,
) -> Converted<Subroutine> {
    let environment = const_functions::Environment {
        constants: &*item.env,
        functions: &tree.state.const_functions,
    };
    let handle =
        classes::handle_type(tree, name).ok_or_else(|| unsupported(format!("class `{name}`")))?;
    let ir_handle =
        crate::ir::Type::from_ast(scoped_type(handle.clone(), declaration_env), &environment);
    let ir_int = crate::ir::Type::from_ast(int_type(), &environment);
    let mut labels = vec![0];
    labels.extend(classes::cast_targets(
        tree,
        name,
        declaration_env,
        type_aliases,
    )?);
    let mut builder = BodyBuilder::new(
        tree,
        item_dimensions,
        state,
        system_functions::Body::Subroutine,
    );
    builder.automatic = true;
    let first_local = builder.state.locals.len();
    builder.push_scope();
    let destination = builder.declare(CAST_DESTINATION, handle.clone());
    let source = builder.declare(CAST_SOURCE, handle);
    let result = builder.declare("$cast", int_type());
    builder.pop_scope();
    let assign = |lhs: &str, rhs: Expr| Stmt::Assign {
        lhs: LValue::Ident(lhs.to_owned()),
        rhs,
        nonblocking: false,
    };
    let mut subroutine = Subroutine {
        name: item.name(&classes::Class::cast_name(name)),
        is_task: false,
        automatic_locals: builder.state.locals[first_local..]
            .iter()
            .map(|local| local.name.clone())
            .collect(),
        persistent_locals: Vec::new(),
        return_type: Some(ir_int),
        params: vec![
            SubroutineParam {
                name: destination.clone(),
                source_name: CAST_DESTINATION.to_owned(),
                direction: ParamDirection::Inout,
                r#type: ir_handle.clone(),
                default: None,
            },
            SubroutineParam {
                name: source.clone(),
                source_name: CAST_SOURCE.to_owned(),
                direction: ParamDirection::Input,
                r#type: ir_handle,
                default: None,
            },
        ],
        return_var: Some(result.clone()),
        body: vec![Stmt::Case {
            kind: CaseKind::Exact,
            selector: classes::object_call(
                system_functions::ObjectOp::ClassOf,
                vec![Expr::Ident(source.clone())],
            ),
            items: vec![CaseItemBase {
                labels: labels
                    .iter()
                    .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                    .collect(),
                body: vec![
                    assign(&destination, Expr::Ident(source)),
                    assign(&result, Expr::Literal("1".into())),
                ],
            }],
            default: Some(vec![assign(&result, Expr::Literal("0".into()))]),
        }],
        sensitivity_body: Vec::new(),
    };
    for stmt in &mut subroutine.body {
        qualify_stmt(item, stmt);
    }
    Ok(subroutine)
}

/// Qualify the module-scope names of a statement for its generate scope.
pub(super) fn qualify_stmt(item: &generate::Item, stmt: &mut Stmt) {
    stmt.visit_mut(
        &mut |expr| item.expr(expr),
        &mut |lvalue| item.lvalue(lvalue),
        &mut |name| {
            if !name.contains('@') {
                *name = item.name(name);
            }
        },
    );
}

/// Replace parameters and genvars by their values.
pub(super) fn substitute_stmt_constants(
    state: &AnalysisState,
    stmt: &mut Stmt,
    const_env: &HashMap<String, i128>,
    literals: &HashMap<String, Expr>,
) {
    stmt.visit_mut(
        &mut |expr| {
            *expr = substitute_expr_constants_with_parameter_literals(
                state,
                expr.clone(),
                const_env,
                literals,
            );
        },
        &mut |lvalue| {
            *lvalue = substitute_lvalue_constants(lvalue.clone(), const_env);
        },
        &mut |_| {},
    );
}

/// Every `initial` process of the active generate items.
pub(super) fn initial_processes_from_scope(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
) -> Result<Vec<InitialProcess>, AnalyzerError> {
    let type_aliases = packed_dimensions.type_aliases.clone();
    // Variable declaration initializers run before every `initial` and
    // `always` procedure (IEEE 1800-2023 10.5).
    let mut initializers = Vec::new();
    let mut processes = Vec::new();
    let active = generate::items(tree, const_env, &type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for (creation_order, item) in active.iter().enumerate() {
        let _interface_scope = item.interface_scope();
        if item.is_parameter_declaration(tree.declarations()) {
            continue;
        }
        let _constant_functions = item.enter();
        let mut assignments = Vec::new();
        if let celox_sv_syntax::BodyItemKind::Data(id) = tree.declarations().body_item(item.id).kind
        {
            let declaration = match tree.declarations().data_declaration(id) {
                celox_sv_syntax::DataDeclaration::Variables(value)
                | celox_sv_syntax::DataDeclaration::Constructed(value) => value,
                celox_sv_syntax::DataDeclaration::Unconverted(_) => {
                    return Err(unsupported("opaque declaration initializer"));
                }
            };
            for variable in declaration.variables.iter().filter(|variable| {
                type_binding::ordinary_variable(
                    variable,
                    declaration.data_type,
                    tree.declarations(),
                )
            }) {
                if let Some(value) = variable.value {
                    assignments.push((
                        instances::normalize_identifier(&variable.name.spelling),
                        value,
                        None,
                    ));
                }
            }
        }
        // Static class properties are initialized like variables, in the
        // scope of the class that declares them (IEEE 1800-2023 8.9, 8.25).
        for property in classes::item_statics(item.id, tree, &item.env, &type_aliases) {
            if let Some(value) = property.initializer {
                assignments.push((property.variable, value, Some(property.owner)));
            }
        }
        if !assignments.is_empty() {
            let (item_dimensions, literals) = views.get(item);
            let mut body = Vec::new();
            for (name, native, owner) in assignments {
                let scoped = owner.map(|owner| {
                    classes::specializations::body_scope(tree, &owner, item_dimensions, literals)
                });
                let (item_dimensions, literals) = match &scoped {
                    Some((dimensions, literals)) => (dimensions, literals),
                    None => (item_dimensions, &**literals),
                };
                if let Some(target) = selected_unpacked_shape(&name, 0, item_dimensions) {
                    let id = native;
                    array_compatibility::check_native_unpacked_assignment(
                        &tree.state,
                        id,
                        tree.declarations(),
                        &target,
                        || format!("initializer of `{name}`"),
                        item_dimensions,
                    )?;
                }
                let lhs = LValue::Ident(name);
                let rhs = expression_binding::expression(
                    native,
                    tree.declarations(),
                    &lhs,
                    tree,
                    item_dimensions,
                )?;
                let mut stmt = Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                };
                // The parameters of the class are its own constants.
                if scoped.is_some() {
                    substitute_stmt_constants(
                        &tree.state,
                        &mut stmt,
                        &item_dimensions.const_env,
                        literals,
                    );
                }
                body.push(stmt);
            }
            let (_, literals) = views.get(item);
            for stmt in &mut body {
                substitute_stmt_constants(&tree.state, stmt, &item.env, literals);
                qualify_stmt(item, stmt);
                substitute_stmt_constants(&tree.state, stmt, const_env, parameter_literals);
            }
            if !body.is_empty() {
                initializers.push(InitialProcess {
                    creation_order,
                    site: process_site(
                        "declaration initializer",
                        tree.declarations().body_item(item.id).tokens,
                        tree,
                    ),
                    condition: None,
                    body,
                    kind: celox_design::ProcessKind::Initializer,
                    validation_body: Vec::new(),
                    synthetic: false,
                    repeats: false,
                });
            }
            continue;
        }
        let Some(procedure) = item.procedure(tree.declarations())? else {
            continue;
        };
        let construct = match procedure.kind {
            celox_sv_syntax::ProcedureKind::Initial => "initial",
            celox_sv_syntax::ProcedureKind::Final => "final",
            _ => "always",
        };
        let repeats = match procedure.kind {
            celox_sv_syntax::ProcedureKind::Initial | celox_sv_syntax::ProcedureKind::Final => {
                false
            }
            celox_sv_syntax::ProcedureKind::Always
                if validation::always_kind(procedure, tree.declarations())?
                    == validation::AlwaysKind::Process
                    || ff_process::always_falls_back_to_process(procedure, tree)? =>
            {
                true
            }
            _ => continue,
        };
        let (item_dimensions, literals) = views.get(item);
        let is_final = procedure.kind == celox_sv_syntax::ProcedureKind::Final;
        let mut builder = BodyBuilder::new(
            tree,
            item_dimensions,
            state,
            if is_final {
                system_functions::Body::Final
            } else {
                system_functions::Body::Initial
            },
        );
        let body = builder.native_body(&[procedure.body], &procedure.authored_statements)?;
        let mut validation_body = if is_final {
            std::mem::take(&mut builder.sensitivity_body)
        } else {
            Vec::new()
        };
        let mut body = if repeats {
            vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }]
        } else {
            body
        };
        for stmt in body.iter_mut().chain(&mut validation_body) {
            substitute_stmt_constants(&tree.state, stmt, &item.env, literals);
            qualify_stmt(item, stmt);
            substitute_stmt_constants(&tree.state, stmt, const_env, parameter_literals);
        }
        processes.push(InitialProcess {
            creation_order,
            site: process_site(
                construct,
                tree.declarations().body_item(item.id).tokens,
                tree,
            ),
            condition: None,
            body,
            kind: if is_final {
                celox_design::ProcessKind::Final
            } else {
                celox_design::ProcessKind::Regular
            },
            validation_body,
            synthetic: false,
            repeats,
        });
    }
    initializers.extend(processes);
    Ok(initializers)
}

/// The names of the variables written by assignment statements.
pub(super) fn written_names<'s>(stmts: impl IntoIterator<Item = &'s Stmt>) -> HashSet<String> {
    let mut names = HashSet::default();
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Assign { lhs, .. } => {
                names.insert(lhs.name().to_string());
            }
            Stmt::AssignConcat { parts, .. } => {
                names.extend(parts.iter().map(|part| part.name().to_string()));
            }
            _ => {}
        });
    }
    names
}

/// Where the first token of a native token range was invoked.
pub(super) fn process_site(
    construct: &'static str,
    tokens: celox_sv_syntax::TokenRange,
    tree: &SyntaxTree,
) -> Option<crate::procedural::ProcessSite> {
    let native = tree.declarations();
    let token = native
        .tokens
        .get(tokens.start..tokens.end)?
        .iter()
        .find(|token| !token.is_trivia())?;
    let span = native.sources.invocation(token.origin);
    let file = native.sources.file(span.file);
    let (line, column) = file.line_column(span.start);
    Some(crate::procedural::ProcessSite {
        construct,
        path: file.path.clone(),
        line,
        column,
    })
}

/// `%t` reports a time value in the units of `$timeformat`, by default the
/// smallest time precision of the design (IEEE 1800-2023 20.4.3, Table 20-3),
/// in which the simulation counts its time. The value is in the time unit of
/// the calling scope, of which `ticks_per_unit` simulation time units make
/// one, so each value a `%t` of the template converts is scaled by it.
fn scale_time_conversions(
    task: SystemTask,
    args: &mut [SystemTaskArg<Expr>],
    ticks_per_unit: u64,
    bind: &mut impl FnMut(usize) -> Result<Option<Expr>, AnalyzerError>,
) -> Result<(), AnalyzerError> {
    if ticks_per_unit == 1 {
        return Ok(());
    }
    let template = match task {
        SystemTask::Display(_) | SystemTask::Write(_) | SystemTask::Severity(_) => 0,
        SystemTask::Fatal => usize::from(matches!(args.first(), Some(SystemTaskArg::Expr(_)))),
        SystemTask::Fdisplay(_)
        | SystemTask::Fwrite(_)
        | SystemTask::Assert
        | SystemTask::AssertContinue => 1,
        _ => return Ok(()),
    };
    let Some(SystemTaskArg::Str(text)) = args.get(template) else {
        return Ok(());
    };
    // The conversions of the template, in the order they take arguments
    // (21.2.1.2): flags, a width and a precision may precede each.
    let mut conversions = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars
            .next_if(|ch| matches!(ch, '-' | '+' | ' ' | '#' | '0'))
            .is_some()
        {}
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        if let Some(spec) = chars.next()
            && "bBoOhHxXdDiIcCsStTeEfFgG".contains(spec)
        {
            conversions.push(spec);
        }
    }
    for (offset, spec) in conversions.into_iter().enumerate() {
        if !matches!(spec, 't' | 'T') {
            continue;
        }
        let index = template + 1 + offset;
        // A string literal converts as its packed bytes (5.9). The factor is
        // signed, so the product keeps the signedness of the value (11.8.1).
        let value = match args.get(index) {
            Some(SystemTaskArg::Expr(value)) => value.clone(),
            Some(SystemTaskArg::Str(_)) => match bind(index)? {
                Some(value) => value,
                None => continue,
            },
            Some(SystemTaskArg::Empty) | None => continue,
        };
        args[index] = SystemTaskArg::Expr(Expr::Binary {
            left: Box::new(value),
            op: BinaryOp::Mul,
            right: Box::new(Expr::Literal(format!("64'sd{ticks_per_unit}"))),
        });
    }
    Ok(())
}

/// Original invocation span of a native token range, when it belongs to one file.
pub(super) fn source_span(
    tokens: celox_sv_syntax::TokenRange,
    tree: &SyntaxTree,
) -> Option<crate::procedural::SourceSpan> {
    let native = tree.declarations();
    let mut result: Option<crate::procedural::SourceSpan> = None;
    for token in native
        .tokens
        .get(tokens.start..tokens.end)?
        .iter()
        .filter(|token| !token.is_trivia())
    {
        let span = native.sources.invocation(token.origin);
        let path = native.sources.file(span.file).path.clone();
        if let Some(current) = &mut result {
            if current.path != path {
                return None;
            }
            current.start = current.start.min(span.start);
            current.end = current.end.max(span.end);
        } else {
            let (logical_path, line, _) = native.sources.logical_location(span);
            result = Some(crate::procedural::SourceSpan {
                logical_path: logical_path.to_owned(),
                line,
                path,
                start: span.start,
                end: span.end,
            });
        }
    }
    result
}

fn dump_selection_name(
    id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    rename: &dyn Fn(&mut ConstExpr),
    is_local: &dyn Fn(&str) -> bool,
) -> Result<String, AnalyzerError> {
    // Preserve identifier/index boundaries in host metadata. A normalized
    // escaped identifier can itself contain '.', '::', or '[0]'. NUL cannot
    // occur in an authored identifier, so it separates these components.
    fn append(
        id: celox_sv_syntax::ExprId,
        tree: &AnalysisContext<'_>,
        dimensions: &PackedDimensions,
        rename: &dyn Fn(&mut ConstExpr),
        kind: &mut &'static str,
        fields: &mut Vec<String>,
    ) -> Result<(), AnalyzerError> {
        use celox_sv_syntax::{DimensionKind, ExprKind};
        match &tree.declarations().expression(id).kind {
            ExprKind::Name(name) if name.spelling == "$root" => {
                *kind = "absolute";
            }
            ExprKind::Name(name) => {
                fields.extend([
                    "name".into(),
                    instances::normalize_identifier(&name.spelling),
                ]);
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." || separator == "::" => {
                append(*expression, tree, dimensions, rename, kind, fields)?;
                if separator == "::" {
                    *kind = "package";
                }
                fields.extend([
                    "name".into(),
                    instances::normalize_identifier(&name.spelling),
                ]);
            }
            ExprKind::Select {
                expression,
                dimension,
            } => {
                let DimensionKind::Index(index) = &dimension.kind else {
                    return Err(unsupported(
                        "$dumpvars hierarchy selection requires a single constant index",
                    ));
                };
                append(*expression, tree, dimensions, rename, kind, fields)?;
                let mut value = constant_binding::bind(*index, tree, dimensions)?
                    .ok_or_else(|| unsupported("$dumpvars hierarchy index must be constant"))?;
                rename(&mut value);
                let index = eval_ast_const_expr(&tree.state, &value, &dimensions.const_env)
                    .ok_or_else(|| {
                        unsupported("$dumpvars hierarchy index must be a known constant integer")
                    })?;
                fields.extend(["index".into(), index.to_string()]);
            }
            _ => {
                return Err(unsupported(
                    "$dumpvars selection must be a module or variable name",
                ));
            }
        }
        Ok(())
    }
    let mut kind = "relative";
    let mut fields = Vec::new();
    append(id, tree, dimensions, rename, &mut kind, &mut fields)?;
    // Imports bind declarations, not a module-local spelling. Nested procedural
    // declarations shadow these aliases just as they do for value expressions.
    if kind == "relative" && fields.len() == 2 && !is_local(&fields[1]) {
        let symbols = scope::imported(&tree.state);
        if let Some(target) = symbols.aliases.get(&fields[1])
            && let Some((package, name)) = symbols
                .state_signals
                .iter()
                .find(|signal| signal.name() == target)
                .and_then(|signal| signal.package_variable())
        {
            kind = "package";
            fields = vec!["name".into(), package.into(), "name".into(), name.into()];
        } else if let Ok(unit) = tree.root_unit()
            && unit.kind == celox_sv_syntax::UnitKind::Package
            && packages::package_variables(&unit.name.spelling, tree)?.contains(&fields[1])
        {
            // A package subroutine is later instantiated at its caller, but
            // its unqualified package references retain their lexical owner.
            kind = "package";
            fields = vec![
                "name".into(),
                instances::normalize_identifier(&unit.name.spelling),
                "name".into(),
                fields[1].clone(),
            ];
        }
    }
    if fields.is_empty() {
        return Err(unsupported(
            "$dumpvars selection must name a module or variable",
        ));
    }
    // A lexical declaration wins over a root module with the same spelling.
    // Keep that decision in metadata: the runtime cannot recover lexical
    // binding from an already flattened path or the top module's name.
    if kind == "relative"
        && (is_local(&fields[1]) || imports::scope_names(tree)?.contains(&fields[1]))
    {
        kind = "local";
    }
    Ok(std::iter::once("celox.dump.path.v1".to_string())
        .chain(std::iter::once(kind.to_string()))
        .chain(fields)
        .collect::<Vec<_>>()
        .join("\0"))
}
