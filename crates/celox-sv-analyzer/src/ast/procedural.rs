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
mod foreach;
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
    pub(super) sensitivity_body: Vec<Stmt>,
}

/// The packed and unpacked shape of a declared type, as selects see it.
pub(super) fn dimensions_from_type(r#type: &Type) -> VariableDimensions {
    VariableDimensions {
        packed: dimensions::signal_packed_dimension_widths(r#type.packed_ranges()),
        unpacked: unpacked_dimension_widths(r#type.unpacked_ranges()),
        signed: r#type.is_signed(),
        is_2state: r#type.kind() == TypeKind::Bit,
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
fn int_type() -> Type {
    Type {
        kind: TypeKind::Bit,
        is_signed: true,
        packed_ranges: vec![PackedRange::new(
            ConstExpr::Literal("31".to_string()),
            ConstExpr::Literal("0".to_string()),
        )],
        unpacked_ranges: Vec::new(),
        members: Vec::new(),
        signed_element_depth: None,
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
        let (written, lhs) = self.native_lvalue(left)?;
        let mut rhs = if op == "=" {
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
        // A two-state member of a four-state packed struct drops unknown bits.
        if matches!(
            written,
            LValue::Select {
                is_2state: true,
                ..
            }
        ) {
            rhs = coerce_procedural_assignment_rhs(&self.tree.state, rhs, &written, &self.dims);
        }
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking,
        })
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
        let (written, lhs) = self.native_lvalue(*expression)?;
        let mut rhs = assignment_op_expr(
            &self.tree.state,
            &written,
            op,
            Expr::Literal("1".into()),
            &self.dims,
        )?;
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
            StatementKind::WaitFork => Err(unsupported("wait fork statement")),
            StatementKind::WaitOrder { .. } => Err(unsupported("wait order statement")),
            StatementKind::EventTrigger(_) => Err(unsupported("event trigger")),
            StatementKind::ProceduralAssign(_) | StatementKind::ProceduralRelease(_) => {
                Err(unsupported("procedural continuous assignment"))
            }
            StatementKind::Disable(_) => Err(unsupported("disable statement")),
            StatementKind::Parallel { .. } => Err(unsupported("fork-join block")),
            _ => Err(unsupported("procedural statement")),
        }
    }

    /// Timing controls suspend the process that executes them: an
    /// `initial` or `always` process. An `always_comb` or `always_ff` body
    /// and a function may not suspend; subroutines with timing controls are
    /// not lowered yet.
    fn allow_timing(&self, construct: &'static str) -> Result<(), AnalyzerError> {
        match self.body {
            system_functions::Body::Initial => Ok(()),
            system_functions::Body::Always => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "an always_comb or always_ff body",
            }),
            system_functions::Body::Subroutine => {
                Err(unsupported(format!("{construct} in a subroutine")))
            }
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
                    type_binding::ordinary_variable(variable, self.tree.declarations())
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
        let discarded = matches!(
            self.tree.declarations().expression(root).kind,
            celox_sv_syntax::ExprKind::Cast { .. }
        );
        {
            let query_root =
                expression_binding::call_statement_root(root, discarded, self.tree.declarations())?;
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
            .map(|arg| {
                let Some(id) = arg.value else {
                    return Ok(SystemTaskArg::Empty);
                };
                if let celox_sv_syntax::ExprKind::Literal(text) = &declarations.expression(id).kind
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
        // Keep Veryl's assertion calls as calls: their message arguments
        // execute even on success, unlike an SV assertion's fail action.
        Ok(vec![Stmt::SystemTask {
            task,
            args,
            site: source_span(declarations.expression(root).tokens, self.tree),
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
    ) -> Result<Vec<(String, i128, i128)>, AnalyzerError> {
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
            loops.push((name, left, right));
        }
        Ok(loops)
    }
    fn fold_foreach(loops: Vec<(String, i128, i128)>, mut body: Vec<Stmt>) -> Vec<Stmt> {
        for (name, left, right) in loops.into_iter().rev() {
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
fn assertion_failure(keyword: &str) -> Vec<Stmt> {
    vec![Stmt::SystemTask {
        task: SystemTask::Severity(system_functions::Severity::Error),
        args: vec![SystemTaskArg::Str(format!("{keyword} failed"))],
        site: None,
    }]
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
    }
    Ok(subroutines)
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
) -> Converted<Subroutine> {
    let native = tree.declarations().subroutine(native_id);
    let name = instances::normalize_identifier(&native.name.spelling);
    let params = subroutine_param_declarations(native, tree, declaration_env, type_aliases)?;
    let mut formal_names = HashSet::default();
    if let Some((name, ..)) = params
        .iter()
        .find(|(name, ..)| !formal_names.insert(name.as_str()))
    {
        return Err(unsupported(format!("duplicate function argument `{name}`")));
    }
    let return_type = match native.return_type {
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
    builder.automatic = match native.lifetime.as_deref() {
        Some("automatic") => true,
        Some("static") => false,
        Some(_) => {
            return Err(AnalyzerError::Parse(
                "unknown native subroutine lifetime".into(),
            ));
        }
        None => builder.automatic,
    };
    let first_static = builder.state.statics.len();
    builder.push_scope();
    // Defaults bind in the enclosing declaration scope, before
    // any formal can shadow those names (IEEE 1800-2023 13.5.3).
    let defaults = params
        .iter()
        .map(|(_, _, _, default)| {
            default
                .map(|root| {
                    let mut value = expression_binding::bind(root, tree, &builder.dims)?;
                    builder.rename_expr(&mut value);
                    Ok(value)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lowered_params = Vec::new();
    for ((source, direction, r#type, _), default) in params.into_iter().zip(defaults) {
        let name = builder.declare(&source, r#type.clone());
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
    let body = builder.native_body(&native.items, &native.authored_statements)?;
    builder.pop_scope();
    let statics: HashSet<String> = builder
        .state
        .statics
        .split_off(first_static)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    if return_var.is_some() {
        let mut expressionless = false;
        for stmt in &body {
            stmt.walk(&mut |stmt| expressionless |= matches!(stmt, Stmt::Return(None)));
        }
        if expressionless {
            return Err(unsupported("expressionless function return"));
        }
    }
    let mut subroutine = Subroutine {
        name: item.name(&name),
        is_task: native.kind == celox_sv_syntax::SubroutineKind::Task,
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
    lifetimes::check_subroutine_statics(
        &tree.state,
        &subroutine,
        &statics,
        builder.state.subroutine_directions,
    )?;
    if !builder.automatic {
        lifetimes::check_static_result(&subroutine, builder.state.subroutine_directions)?;
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
            for variable in declaration
                .variables
                .iter()
                .filter(|variable| type_binding::ordinary_variable(variable, tree.declarations()))
            {
                if let Some(value) = variable.value {
                    assignments.push((
                        instances::normalize_identifier(&variable.name.spelling),
                        value,
                    ));
                }
            }
        }
        if !assignments.is_empty() {
            let (item_dimensions, literals) = views.get(item);
            let mut body = Vec::new();
            for (name, native) in assignments {
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
                body.push(Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                });
            }
            for stmt in &mut body {
                substitute_stmt_constants(&tree.state, stmt, &item.env, literals);
                qualify_stmt(item, stmt);
                substitute_stmt_constants(&tree.state, stmt, const_env, parameter_literals);
            }
            if !body.is_empty() {
                initializers.push(InitialProcess {
                    creation_order,
                    condition: None,
                    body,
                    initializer: true,
                    synthetic: false,
                    repeats: false,
                });
            }
            continue;
        }
        let Some(procedure) = item.procedure(tree.declarations())? else {
            continue;
        };
        let repeats = match procedure.kind {
            celox_sv_syntax::ProcedureKind::Initial => false,
            celox_sv_syntax::ProcedureKind::Always
                if validation::always_kind(procedure, tree.declarations())?
                    == validation::AlwaysKind::Process =>
            {
                true
            }
            _ => continue,
        };
        let (item_dimensions, literals) = views.get(item);
        // A process statement runs in the body kind of an `initial` block:
        // it may suspend and call any task.
        let mut builder = BodyBuilder::new(
            tree,
            item_dimensions,
            state,
            system_functions::Body::Initial,
        );
        let body = builder.native_body(&[procedure.body], &procedure.authored_statements)?;
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
        for stmt in &mut body {
            substitute_stmt_constants(&tree.state, stmt, &item.env, literals);
            qualify_stmt(item, stmt);
            substitute_stmt_constants(&tree.state, stmt, const_env, parameter_literals);
        }
        processes.push(InitialProcess {
            creation_order,
            condition: None,
            body,
            initializer: false,
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
            result = Some(crate::procedural::SourceSpan {
                path,
                start: span.start,
                end: span.end,
            });
        }
    }
    result
}
