//! Statement-structured procedural bodies.
//!
//! `always`, `initial`, and subroutine bodies are converted statement by
//! statement into [`Stmt`] trees. Variables declared inside a body are
//! lexically scoped (IEEE 1800-2023 6.21): each receives a module-unique name
//! (`name@n`) and is collected as a [`LocalVariable`], so a local may shadow a
//! module signal or share its name with a local of another process.

use super::*;
use crate::procedural::{
    CaseItemBase, CaseKind, CaseLabel, LoopKind, ParamDirection, SystemTaskArg,
};

/// One lexical scope: the locals it declares and the bindings they shadow.
struct Scope {
    entries: Vec<ScopeEntry>,
}

struct ScopeEntry {
    source: String,
    unique: String,
    shadowed_dimensions: Option<VariableDimensions>,
    shadowed_constant: Option<i128>,
    shadowed_signedness: Option<bool>,
}

/// Per-module state shared by every body converted in that module.
pub(super) struct BodyState<'a> {
    pub locals: &'a mut Vec<LocalVariable>,
    pub counter: &'a mut usize,
    /// Positional argument names of each subroutine, for named arguments.
    pub subroutine_params: &'a HashMap<String, Vec<String>>,
}

/// Converts the statements of one procedural body.
pub(super) struct BodyBuilder<'s, 't, 'a> {
    tree: &'t SyntaxTree,
    dims: PackedDimensions,
    scopes: Vec<Scope>,
    state: &'s mut BodyState<'a>,
    type_aliases: HashMap<String, Type>,
    /// The values of the local parameters declared in the body.
    local_constants: HashMap<String, String>,
    /// The kind of body, for the system tasks it may call.
    body: system_functions::Body,
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
        tree: &'t SyntaxTree,
        dims: &PackedDimensions,
        state: &'s mut BodyState<'a>,
        body: system_functions::Body,
    ) -> Self {
        let type_aliases = dims.type_aliases.clone();
        Self {
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
            match entry.shadowed_dimensions {
                Some(dimensions) => {
                    self.dims.insert(entry.source.clone(), dimensions);
                }
                None => {
                    self.dims.remove(&entry.source);
                }
            }
            if let Some(value) = entry.shadowed_constant {
                self.dims.const_env.insert(entry.source.clone(), value);
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
        let shadowed_dimensions = self
            .dims
            .insert(source.to_string(), dimensions_from_type(&r#type));
        let shadowed_constant = self.dims.const_env.remove(source);
        let shadowed_signedness = self
            .dims
            .expression_signedness
            .insert(source.to_string(), r#type.is_signed());
        self.state.locals.push(LocalVariable {
            name: unique.clone(),
            source_name: source.to_string(),
            r#type: crate::ir::Type::from_ast(
                scoped_type(r#type, &self.dims.const_env),
                &self.dims.const_env,
            ),
        });
        self.scopes
            .last_mut()
            .expect("a body always has a scope")
            .entries
            .push(ScopeEntry {
                source: source.to_string(),
                unique: unique.clone(),
                shadowed_dimensions,
                shadowed_constant,
                shadowed_signedness,
            });
        unique
    }

    fn lookup(&self, source: &str) -> Option<&str> {
        self.scopes.iter().rev().find_map(|scope| {
            scope
                .entries
                .iter()
                .rev()
                .find(|entry| entry.source == source)
                .map(|entry| entry.unique.as_str())
        })
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
            ConstExpr::Select { expr, bit } => {
                self.rename_const(expr);
                self.rename_const(bit);
            }
            ConstExpr::Function { args, .. } => {
                args.iter_mut().for_each(|arg| self.rename_const(arg));
            }
            ConstExpr::Unary { expr, .. } => self.rename_const(expr),
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
            Expr::Call { args, .. } => args.iter_mut().for_each(|arg| self.rename_expr(arg)),
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

    fn expr(&self, expr: &sv_parser::Expression) -> Result<Expr, AnalyzerError> {
        let mut lowered = expr_from_expression_with_types(expr, self.tree, &self.dims)?;
        self.rename_expr(&mut lowered);
        Ok(lowered)
    }

    /// The lvalue as written (before local renaming) and as stored.
    fn lvalue(&self, node: &sv_parser::VariableLvalue) -> Result<(LValue, LValue), AnalyzerError> {
        let lowered = variable_lvalue_from_node(node, self.tree, &self.dims)?;
        let mut renamed = lowered.clone();
        self.rename_lvalue(&mut renamed);
        Ok((lowered, renamed))
    }

    /// The parts of a concatenated assignment target, most significant first.
    fn concat_lvalue(&self, node: &sv_parser::VariableLvalue) -> Option<Vec<LValue>> {
        let sv_parser::VariableLvalue::Lvalue(concat) = node else {
            return None;
        };
        let mut parts = Vec::new();
        for part in concat.nodes.0.nodes.1.contents() {
            match part {
                sv_parser::VariableLvalue::Lvalue(_) => parts.extend(self.concat_lvalue(part)?),
                _ => parts.push(self.lvalue(part).ok()?.1),
            }
        }
        Some(parts)
    }

    fn assignment(
        &self,
        lvalue: &sv_parser::VariableLvalue,
        op: &str,
        rhs: &sv_parser::Expression,
        nonblocking: bool,
    ) -> Result<Stmt, AnalyzerError> {
        if let Some(parts) = self.concat_lvalue(lvalue) {
            if op != "=" {
                return Err(unsupported("compound assignment to a concatenation"));
            }
            return Ok(Stmt::AssignConcat {
                parts,
                rhs: self.expr(rhs)?,
                nonblocking,
            });
        }
        if op == "="
            && let Some(target) = variable_lvalue_unpacked_shape(lvalue, self.tree, &self.dims)
        {
            check_unpacked_array_assignment(
                rhs,
                &target,
                || "assignment".to_string(),
                self.tree,
                &self.dims,
            )?;
        }
        let (written, lhs) = self.lvalue(lvalue)?;
        let mut rhs = if op == "=" {
            expr_from_expression_for_lvalue(rhs, &written, self.tree, &self.dims)?
        } else {
            let value = expr_from_expression_with_types(rhs, self.tree, &self.dims)?;
            assignment_op_expr(&written, op, value, &self.dims)?
        };
        // A two-state member of a four-state packed struct drops unknown bits.
        if matches!(
            written,
            LValue::Select {
                is_2state: true,
                ..
            }
        ) {
            rhs = coerce_procedural_assignment_rhs(rhs, &written, &self.dims);
        }
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking,
        })
    }

    fn inc_or_dec(&self, expr: &sv_parser::IncOrDecExpression) -> Result<Stmt, AnalyzerError> {
        let (op, lvalue) = match expr {
            sv_parser::IncOrDecExpression::Prefix(expr) => (&expr.nodes.0, &expr.nodes.2),
            sv_parser::IncOrDecExpression::Suffix(expr) => (&expr.nodes.2, &expr.nodes.0),
        };
        let op = match self.tree.get_str(&op.nodes.0) {
            Some("++") => "+=",
            Some("--") => "-=",
            _ => return Err(unsupported("increment or decrement operator")),
        };
        let (written, lhs) = self.lvalue(lvalue)?;
        let mut rhs = assignment_op_expr(&written, op, Expr::Literal("1".to_string()), &self.dims)?;
        self.rename_expr(&mut rhs);
        Ok(Stmt::Assign {
            lhs,
            rhs,
            nonblocking: false,
        })
    }

    pub(super) fn statements(
        &mut self,
        stmts: &[sv_parser::StatementOrNull],
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut lowered = Vec::new();
        for stmt in stmts {
            lowered.extend(self.statement_or_null(stmt)?);
        }
        Ok(lowered)
    }

    pub(super) fn statement_or_null(
        &mut self,
        stmt: &sv_parser::StatementOrNull,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        match stmt {
            sv_parser::StatementOrNull::Statement(stmt) => self.statement(stmt),
            sv_parser::StatementOrNull::Attribute(_) => Ok(Vec::new()),
        }
    }

    pub(super) fn statement(
        &mut self,
        stmt: &sv_parser::Statement,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        match &stmt.nodes.2 {
            sv_parser::StatementItem::BlockingAssignment(assignment) => match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    if !matches!(assignment.nodes.2, sv_parser::DelayOrEventControl::Delay(_))
                        && delay_or_event_control_is_empty(&assignment.nodes.2)
                    {
                        Ok(vec![self.assignment(
                            &assignment.nodes.0,
                            "=",
                            &assignment.nodes.3,
                            false,
                        )?])
                    } else {
                        Err(unsupported("intra-assignment timing control"))
                    }
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    let op = self
                        .tree
                        .get_str(&assignment.nodes.1.nodes.0.nodes.0)
                        .unwrap_or("=");
                    Ok(vec![self.assignment(
                        &assignment.nodes.0,
                        op,
                        &assignment.nodes.2,
                        false,
                    )?])
                }
                _ => Err(unsupported("dynamic array or class assignment")),
            },
            sv_parser::StatementItem::NonblockingAssignment(assignment) => {
                if assignment.0.nodes.2.is_some() {
                    return Err(unsupported("intra-assignment timing control"));
                }
                Ok(vec![self.assignment(
                    &assignment.0.nodes.0,
                    "=",
                    &assignment.0.nodes.3,
                    true,
                )?])
            }
            sv_parser::StatementItem::IncOrDecExpression(expr) => {
                Ok(vec![self.inc_or_dec(&expr.0)?])
            }
            sv_parser::StatementItem::SubroutineCallStatement(call) => {
                self.subroutine_call_statement(call)
            }
            sv_parser::StatementItem::ConditionalStatement(stmt) => self.conditional(stmt),
            sv_parser::StatementItem::CaseStatement(stmt) => self.case(stmt),
            sv_parser::StatementItem::LoopStatement(stmt) => self.loop_statement(stmt),
            sv_parser::StatementItem::JumpStatement(jump) => match &**jump {
                sv_parser::JumpStatement::Break(_) => Ok(vec![Stmt::Break]),
                sv_parser::JumpStatement::Continue(_) => Ok(vec![Stmt::Continue]),
                sv_parser::JumpStatement::Return(stmt) => Ok(vec![Stmt::Return(
                    stmt.nodes
                        .1
                        .as_ref()
                        .map(|expr| self.expr(expr))
                        .transpose()?,
                )]),
            },
            sv_parser::StatementItem::SeqBlock(block) => {
                self.push_scope();
                let result = self.seq_block(block);
                self.pop_scope();
                result
            }
            sv_parser::StatementItem::ProceduralAssertionStatement(assertion) => {
                self.assertion(assertion)
            }
            sv_parser::StatementItem::ProceduralTimingControlStatement(_)
            | sv_parser::StatementItem::WaitStatement(_)
            | sv_parser::StatementItem::EventTrigger(_) => {
                Err(unsupported("procedural timing control"))
            }
            sv_parser::StatementItem::ProceduralContinuousAssignment(_) => {
                Err(unsupported("procedural continuous assignment"))
            }
            sv_parser::StatementItem::DisableStatement(_) => Err(unsupported("disable statement")),
            sv_parser::StatementItem::ParBlock(_) => Err(unsupported("fork-join block")),
            _ => Err(unsupported("procedural statement")),
        }
    }

    fn seq_block(&mut self, block: &sv_parser::SeqBlock) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut stmts = Vec::new();
        for item in &block.nodes.2 {
            stmts.extend(self.block_item_declaration(item)?);
        }
        stmts.extend(self.statements(&block.nodes.3)?);
        Ok(stmts)
    }

    /// Declare the variables of a block item and return their initializations.
    pub(super) fn block_item_declaration(
        &mut self,
        item: &sv_parser::BlockItemDeclaration,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let item = match item {
            sv_parser::BlockItemDeclaration::Data(item) => item,
            sv_parser::BlockItemDeclaration::LocalParameter(item) => {
                return self.local_parameters(RefNode::LocalParameterDeclaration(&item.nodes.1));
            }
            sv_parser::BlockItemDeclaration::Parameter(item) => {
                return self.local_parameters(RefNode::ParameterDeclaration(&item.nodes.1));
            }
            _ => return Err(unsupported("let declaration inside a procedural block")),
        };
        let data = &item.nodes.1;
        let sv_parser::DataDeclaration::Variable(variable) = data else {
            return Err(unsupported("type declaration inside a procedural block"));
        };
        let signals = signals_from_data_declaration(
            data,
            self.tree,
            &self.type_aliases,
            &self.dims.const_env,
            None,
        )?;
        let mut stmts = Vec::new();
        for (signal, assignment) in signals.into_iter().zip(variable.nodes.4.nodes.0.contents()) {
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
            let sv_parser::VariableDeclAssignment::Variable(assignment) = assignment else {
                return Err(unsupported("dynamic array or class declaration"));
            };
            let init = match &assignment.nodes.2 {
                Some((_, expr)) => {
                    // The initializer is evaluated before the new name is in
                    // scope (IEEE 1800-2023 6.21).
                    let target = LValue::Ident(signal.name().to_string());
                    let dims = self.dims.clone();
                    let mut scoped = dims;
                    scoped.insert(
                        signal.name().to_string(),
                        dimensions_from_type(signal.r#type()),
                    );
                    let mut init =
                        expr_from_expression_for_lvalue(expr, &target, self.tree, &scoped)?;
                    self.rename_expr(&mut init);
                    Some(init)
                }
                None => None,
            };
            let name = self.declare(signal.name(), signal.r#type().clone());
            stmts.push(Stmt::Local { name, init });
        }
        Ok(stmts)
    }

    /// Make the parameters of a declaration in the body known as constants.
    fn local_parameters(&mut self, node: RefNode<'_>) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut parameters = Vec::new();
        parameters_from_ref_node(
            node,
            self.tree,
            &mut parameters,
            true,
            &self.dims.const_env,
            &self.type_aliases,
            &HashMap::default(),
        )?;
        for parameter in &parameters {
            let types = parameter_types_from_const_env(&self.dims.const_env);
            let value = parameter
                .resolved_value(&self.dims.const_env, &types)
                .ok_or_else(|| unsupported("local parameter whose value is not constant"))?;
            let literal = match parameter.resolved_type(&types) {
                Some(r#type) => format_typed_parameter_literal(value, r#type.width, r#type.signed),
                None => value.to_string(),
            };
            self.local_constants
                .insert(parameter.name().to_string(), literal);
        }
        extend_const_env_with_parameters(&mut self.dims.const_env, &parameters);
        Ok(Vec::new())
    }

    fn subroutine_call_statement(
        &mut self,
        call: &sv_parser::SubroutineCallStatement,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let call = match call {
            sv_parser::SubroutineCallStatement::SubroutineCall(call) => &call.0,
            sv_parser::SubroutineCallStatement::Function(call) => &call.nodes.2.nodes.1.nodes.0,
        };
        match call {
            sv_parser::SubroutineCall::TfCall(call) => {
                let name = reference_name(
                    RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0),
                    self.tree,
                )
                .ok_or_else(|| unsupported("subroutine call"))?;
                let args =
                    self.call_arguments(&name, call.nodes.2.as_ref().map(|paren| &paren.nodes.1))?;
                Ok(vec![Stmt::Call { name, args }])
            }
            sv_parser::SubroutineCall::SystemTfCall(system_call) => {
                let (name, args) = system_tf_call_parts(system_call, self.tree)
                    .ok_or_else(|| unsupported("system task or function name"))?;
                let tf = system_functions::check_call(
                    name,
                    args.as_deref(),
                    system_functions::CallSite::Statement(self.body),
                )?;
                if tf.expression {
                    // A system function called as a statement is converted
                    // and checked like the same call in an expression, then
                    // its value is discarded. `$bits` and `$size` become
                    // constants and do not evaluate their operands
                    // (IEEE 1800-2023 20.6.2).
                    let mut value = expr_from_subroutine_call(call, self.tree, &self.dims)?;
                    self.rename_expr(&mut value);
                    return Ok(vec![Stmt::Eval(value)]);
                }
                let call = system_call;
                let (name, args) = match &**call {
                    sv_parser::SystemTfCall::ArgOptionl(call) => (
                        self.tree.get_str(&call.nodes.0.nodes.0),
                        call.nodes
                            .1
                            .as_ref()
                            .map(|paren| self.system_task_arguments(&paren.nodes.1))
                            .transpose()?
                            .unwrap_or_default(),
                    ),
                    sv_parser::SystemTfCall::ArgExpression(call) => {
                        let mut args = Vec::new();
                        for arg in call.nodes.1.nodes.1.0.contents() {
                            args.push(self.system_task_argument(arg.as_ref())?);
                        }
                        if args.len() == 1 && args[0] == SystemTaskArg::Empty {
                            args.clear();
                        }
                        (self.tree.get_str(&call.nodes.0.nodes.0), args)
                    }
                    sv_parser::SystemTfCall::ArgDataType(_) => {
                        return Err(unsupported("system task with a data type argument"));
                    }
                };
                let name = name.ok_or_else(|| unsupported("system task"))?.to_string();
                // Veryl's `$assert(cond, ...)` and `$assert_continue(cond, ...)`
                // are immediate assertions that end the simulation or continue.
                if let ("$assert" | "$assert_continue", Some(SystemTaskArg::Expr(condition))) =
                    (name.as_str(), args.first())
                {
                    let mut message = args[1..].to_vec();
                    if !matches!(message.first(), Some(SystemTaskArg::Str(_))) {
                        message.insert(0, SystemTaskArg::Str("assertion failed".to_string()));
                    }
                    let task = if name == "$assert" {
                        "$fatal"
                    } else {
                        "$error"
                    };
                    return Ok(vec![Stmt::If {
                        condition: procedural_truth_condition(condition.clone()),
                        then_body: Vec::new(),
                        else_body: vec![Stmt::SystemTask {
                            name: task.to_string(),
                            args: message,
                        }],
                    }]);
                }
                Ok(vec![Stmt::SystemTask { name, args }])
            }
            _ => Err(unsupported("method or randomize call")),
        }
    }

    fn system_task_arguments(
        &self,
        args: &sv_parser::ListOfArguments,
    ) -> Result<Vec<SystemTaskArg<Expr>>, AnalyzerError> {
        let sv_parser::ListOfArguments::Ordered(args) = args else {
            return Err(unsupported("named system task arguments"));
        };
        if !args.nodes.1.is_empty() {
            return Err(unsupported("named system task arguments"));
        }
        let contents = args.nodes.0.contents();
        if contents.len() == 1 && contents[0].is_none() {
            return Ok(Vec::new());
        }
        contents
            .into_iter()
            .map(|arg| self.system_task_argument(arg.as_ref()))
            .collect()
    }

    fn system_task_argument(
        &self,
        arg: Option<&sv_parser::Expression>,
    ) -> Result<SystemTaskArg<Expr>, AnalyzerError> {
        let Some(arg) = arg else {
            return Ok(SystemTaskArg::Empty);
        };
        if let sv_parser::Expression::Primary(primary) = arg
            && let sv_parser::Primary::PrimaryLiteral(literal) = &**primary
            && let sv_parser::PrimaryLiteral::StringLiteral(text) = &**literal
        {
            let text = self
                .tree
                .get_str(&text.nodes.0)
                .ok_or_else(|| unsupported("string literal"))?;
            let text = text
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(text);
            return Ok(SystemTaskArg::Str(text.to_string()));
        }
        Ok(SystemTaskArg::Expr(self.expr(arg)?))
    }

    /// Positional arguments of a subroutine call, binding named arguments by
    /// the declared argument order.
    fn call_arguments(
        &self,
        name: &str,
        args: Option<&sv_parser::ListOfArguments>,
    ) -> Result<Vec<Option<Expr>>, AnalyzerError> {
        let Some(args) = args else {
            return Ok(Vec::new());
        };
        let (positional, named): (Vec<Option<&sv_parser::Expression>>, Vec<_>) = match args {
            sv_parser::ListOfArguments::Ordered(args) => {
                let contents = args.nodes.0.contents();
                let positional = if contents.len() == 1 && contents[0].is_none() {
                    Vec::new()
                } else {
                    contents.into_iter().map(|arg| arg.as_ref()).collect()
                };
                let named = args
                    .nodes
                    .1
                    .iter()
                    .map(|(_, _, identifier, value)| (identifier, value.nodes.1.as_ref()))
                    .collect();
                (positional, named)
            }
            sv_parser::ListOfArguments::Named(args) => {
                let mut named = vec![(&args.nodes.1, args.nodes.2.nodes.1.as_ref())];
                named.extend(
                    args.nodes
                        .3
                        .iter()
                        .map(|(_, _, identifier, value)| (identifier, value.nodes.1.as_ref())),
                );
                (Vec::new(), named)
            }
        };
        let mut lowered: Vec<Option<Expr>> = positional
            .into_iter()
            .enumerate()
            .map(|(position, arg)| {
                arg.map(|arg| self.call_argument(name, position, arg))
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        if !named.is_empty() {
            let params = self
                .state
                .subroutine_params
                .get(name)
                .ok_or_else(|| unsupported(format!("named arguments to `{name}`")))?;
            for (identifier, value) in named {
                let formal = identifier_text(RefNode::Identifier(identifier), self.tree)
                    .ok_or_else(|| unsupported("named argument"))?;
                let position = params
                    .iter()
                    .position(|param| *param == formal)
                    .ok_or_else(|| unsupported(format!("argument `{formal}` of `{name}`")))?;
                if lowered.len() <= position {
                    lowered.resize(position + 1, None);
                }
                lowered[position] = value
                    .map(|value| self.call_argument(name, position, value))
                    .transpose()?;
            }
        }
        Ok(lowered)
    }

    /// One subroutine argument; an assignment pattern takes the formal's shape.
    fn call_argument(
        &self,
        name: &str,
        position: usize,
        arg: &sv_parser::Expression,
    ) -> Result<Expr, AnalyzerError> {
        let shape = self
            .dims
            .subroutine_param_shapes
            .get(name)
            .and_then(|shapes| shapes.get(position));
        if let Some(shape) = shape {
            // Passing an argument in any direction is an assignment-like
            // context (IEEE 1800-2023 10.8).
            check_unpacked_array_assignment(
                arg,
                shape,
                || format!("argument {} of `{name}`", position + 1),
                self.tree,
                &self.dims,
            )?;
        }
        if let Some(pattern) =
            patterns::pattern_expression(arg).filter(|pattern| pattern.nodes.0.is_none())
            && let Some(shape) = shape
        {
            let mut lowered =
                patterns::expr_from_pattern(&pattern.nodes.1, shape, self.tree, &self.dims)?;
            self.rename_expr(&mut lowered);
            return Ok(lowered);
        }
        self.expr(arg)
    }

    fn conditional(
        &mut self,
        stmt: &sv_parser::ConditionalStatement,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let condition = self.cond_predicate(&stmt.nodes.2.nodes.1)?;
        let then_body = self.statement_or_null(&stmt.nodes.3)?;
        let mut branches = vec![(condition, then_body)];
        for (_, _, predicate, branch) in &stmt.nodes.4 {
            let condition = self.cond_predicate(&predicate.nodes.1)?;
            branches.push((condition, self.statement_or_null(branch)?));
        }
        let mut else_body = match &stmt.nodes.5 {
            Some((_, branch)) => self.statement_or_null(branch)?,
            None => Vec::new(),
        };
        for (condition, then_body) in branches.into_iter().rev() {
            else_body = vec![Stmt::If {
                condition,
                then_body,
                else_body,
            }];
        }
        Ok(else_body)
    }

    fn cond_predicate(&self, predicate: &sv_parser::CondPredicate) -> Result<Expr, AnalyzerError> {
        let mut condition = expr_from_cond_predicate(predicate, self.tree, &self.dims)?;
        self.rename_expr(&mut condition);
        Ok(condition)
    }

    fn case(&mut self, stmt: &sv_parser::CaseStatement) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut items = Vec::new();
        let mut default = None;
        let (kind, selector) = match stmt {
            sv_parser::CaseStatement::Normal(stmt) => {
                let kind = match &stmt.nodes.1 {
                    sv_parser::CaseKeyword::Case(_) => CaseKind::Exact,
                    sv_parser::CaseKeyword::Casez(_) => CaseKind::Z,
                    sv_parser::CaseKeyword::Casex(_) => CaseKind::X,
                };
                let selector = self.expr(&stmt.nodes.2.nodes.1.nodes.0)?;
                for item in std::iter::once(&stmt.nodes.3).chain(stmt.nodes.4.iter()) {
                    match item {
                        sv_parser::CaseItem::NonDefault(item) => {
                            let labels = item
                                .nodes
                                .0
                                .contents()
                                .into_iter()
                                .map(|label| self.expr(&label.nodes.0).map(CaseLabel::Value))
                                .collect::<Result<_, _>>()?;
                            let body = self.statement_or_null(&item.nodes.2)?;
                            items.push(CaseItemBase { labels, body });
                        }
                        sv_parser::CaseItem::Default(item) => {
                            default = Some(self.statement_or_null(&item.nodes.2)?);
                        }
                    }
                }
                (kind, selector)
            }
            sv_parser::CaseStatement::Inside(stmt) => {
                let selector = self.expr(&stmt.nodes.2.nodes.1.nodes.0)?;
                for item in std::iter::once(&stmt.nodes.4).chain(stmt.nodes.5.iter()) {
                    match item {
                        sv_parser::CaseInsideItem::NonDefault(item) => {
                            let labels = item
                                .nodes
                                .0
                                .nodes
                                .0
                                .contents()
                                .into_iter()
                                .map(|range| self.value_range(&range.nodes.0))
                                .collect::<Result<_, _>>()?;
                            let body = self.statement_or_null(&item.nodes.2)?;
                            items.push(CaseItemBase { labels, body });
                        }
                        sv_parser::CaseInsideItem::Default(item) => {
                            default = Some(self.statement_or_null(&item.nodes.2)?);
                        }
                    }
                }
                (CaseKind::Inside, selector)
            }
            sv_parser::CaseStatement::Matches(_) => {
                return Err(unsupported("pattern-matching case statement"));
            }
        };
        Ok(vec![Stmt::Case {
            kind,
            selector,
            items,
            default,
        }])
    }

    fn value_range(&self, range: &sv_parser::ValueRange) -> Result<CaseLabel<Expr>, AnalyzerError> {
        match range {
            sv_parser::ValueRange::Expression(expr) => Ok(CaseLabel::Value(self.expr(expr)?)),
            sv_parser::ValueRange::Binary(range) => {
                let (low, _, high) = &range.nodes.0.nodes.1;
                Ok(CaseLabel::Range {
                    low: self.expr(low)?,
                    high: self.expr(high)?,
                })
            }
        }
    }

    fn loop_statement(
        &mut self,
        stmt: &sv_parser::LoopStatement,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        match stmt {
            sv_parser::LoopStatement::For(stmt) => {
                self.push_scope();
                let result = self.for_loop(stmt);
                self.pop_scope();
                result
            }
            sv_parser::LoopStatement::While(stmt) => {
                let condition = self.expr(&stmt.nodes.1.nodes.1)?;
                let body = self.statement_or_null(&stmt.nodes.2)?;
                Ok(vec![Stmt::Loop {
                    kind: LoopKind::While,
                    init: Vec::new(),
                    condition: Some(condition),
                    step: Vec::new(),
                    body,
                }])
            }
            sv_parser::LoopStatement::DoWhile(stmt) => {
                let body = self.statement_or_null(&stmt.nodes.1)?;
                let condition = self.expr(&stmt.nodes.3.nodes.1)?;
                Ok(vec![Stmt::Loop {
                    kind: LoopKind::DoWhile,
                    init: Vec::new(),
                    condition: Some(condition),
                    step: Vec::new(),
                    body,
                }])
            }
            sv_parser::LoopStatement::Repeat(stmt) => {
                let count = self.expr(&stmt.nodes.1.nodes.1)?;
                let body = self.statement_or_null(&stmt.nodes.2)?;
                Ok(vec![Stmt::Loop {
                    kind: LoopKind::Repeat(count),
                    init: Vec::new(),
                    condition: None,
                    step: Vec::new(),
                    body,
                }])
            }
            sv_parser::LoopStatement::Forever(stmt) => {
                let body = self.statement_or_null(&stmt.nodes.1)?;
                Ok(vec![Stmt::Loop {
                    kind: LoopKind::Forever,
                    init: Vec::new(),
                    condition: None,
                    step: Vec::new(),
                    body,
                }])
            }
            sv_parser::LoopStatement::Foreach(stmt) => {
                self.push_scope();
                let result = self.foreach_loop(stmt);
                self.pop_scope();
                result
            }
        }
    }

    fn for_loop(&mut self, stmt: &sv_parser::LoopStatementFor) -> Result<Vec<Stmt>, AnalyzerError> {
        let (initialization, _, condition, _, step) = &stmt.nodes.1.nodes.1;
        let mut init = Vec::new();
        match initialization {
            None => {}
            Some(sv_parser::ForInitialization::Declaration(declaration)) => {
                for variable in declaration.nodes.0.contents() {
                    let r#type = function_type_from_ref_node(
                        RefNode::DataType(&variable.nodes.1),
                        self.tree,
                        &self.dims.const_env,
                        &self.type_aliases,
                    )
                    .ok_or_else(|| unsupported("for-loop variable type"))?;
                    for (identifier, _, value) in variable.nodes.2.contents() {
                        let source =
                            identifier_text(RefNode::VariableIdentifier(identifier), self.tree)
                                .ok_or_else(|| unsupported("for-loop variable"))?;
                        // The initializer is evaluated outside the new scope.
                        let mut value_expr = {
                            let target = LValue::Ident(source.clone());
                            let mut scoped = self.dims.clone();
                            scoped.insert(source.clone(), dimensions_from_type(&r#type));
                            expr_from_expression_for_lvalue(value, &target, self.tree, &scoped)?
                        };
                        self.rename_expr(&mut value_expr);
                        let name = self.declare(&source, r#type.clone());
                        init.push(Stmt::Local {
                            name,
                            init: Some(value_expr),
                        });
                    }
                }
            }
            Some(sv_parser::ForInitialization::ListOfVariableAssignments(assignments)) => {
                for assignment in assignments.nodes.0.contents() {
                    init.push(self.assignment(
                        &assignment.nodes.0,
                        "=",
                        &assignment.nodes.2,
                        false,
                    )?);
                }
            }
        }
        let condition = condition.as_ref().map(|expr| self.expr(expr)).transpose()?;
        let mut steps = Vec::new();
        if let Some(step) = step {
            for step in step.nodes.0.contents() {
                steps.push(match step {
                    sv_parser::ForStepAssignment::OperatorAssignment(assignment) => {
                        let op = self
                            .tree
                            .get_str(&assignment.nodes.1.nodes.0.nodes.0)
                            .unwrap_or("=");
                        self.assignment(&assignment.nodes.0, op, &assignment.nodes.2, false)?
                    }
                    sv_parser::ForStepAssignment::IncOrDecExpression(expr) => {
                        self.inc_or_dec(expr)?
                    }
                    sv_parser::ForStepAssignment::FunctionSubroutineCall(_) => {
                        return Err(unsupported("subroutine call as a for-loop step"));
                    }
                });
            }
        }
        let body = self.statement_or_null(&stmt.nodes.2)?;
        Ok(vec![Stmt::Loop {
            kind: LoopKind::For,
            init,
            condition,
            step: steps,
            body,
        }])
    }

    /// `foreach (a[i, j])` as nested `for` loops that step each named index
    /// from the left bound of its dimension to the right bound (IEEE
    /// 1800-2023 12.7.3).
    fn foreach_loop(
        &mut self,
        stmt: &sv_parser::LoopStatementForeach,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let (array, variables) = &stmt.nodes.1.nodes.1;
        let array = identifier_text(RefNode::PsOrHierarchicalArrayIdentifier(array), self.tree)
            .ok_or_else(|| unsupported("foreach array"))?;
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
        for (position, variable) in variables.nodes.1.nodes.0.contents().into_iter().enumerate() {
            let Some(variable) = variable else {
                continue;
            };
            let (left, right) = bounds
                .get(position)
                .ok_or_else(|| unsupported("foreach index beyond the array dimensions"))?;
            let left = eval_ast_const_expr(left, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let right = eval_ast_const_expr(right, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let source = identifier_text(RefNode::IndexVariableIdentifier(variable), self.tree)
                .ok_or_else(|| unsupported("foreach index variable"))?;
            let name = self.declare(&source, int_type());
            loops.push((name, left, right));
        }
        let mut body = self.statement(&stmt.nodes.2)?;
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
        Ok(body)
    }

    /// An immediate assertion: the pass action runs when the condition is
    /// true; otherwise the fail action, or `$error` without one (IEEE
    /// 1800-2023 16.3).
    fn assertion(
        &mut self,
        assertion: &sv_parser::ProceduralAssertionStatement,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let sv_parser::ProceduralAssertionStatement::Immediate(assertion) = assertion else {
            return Err(unsupported("concurrent assertion"));
        };
        let (keyword, expr, action) = match &**assertion {
            sv_parser::ImmediateAssertionStatement::Simple(assertion) => match &**assertion {
                sv_parser::SimpleImmediateAssertionStatement::Assert(stmt) => {
                    ("assert", &stmt.nodes.1.nodes.1, Some(&stmt.nodes.2))
                }
                sv_parser::SimpleImmediateAssertionStatement::Assume(stmt) => {
                    ("assume", &stmt.nodes.1.nodes.1, Some(&stmt.nodes.2))
                }
                sv_parser::SimpleImmediateAssertionStatement::Cover(stmt) => {
                    return Ok(vec![Stmt::If {
                        condition: procedural_truth_condition(self.expr(&stmt.nodes.1.nodes.1)?),
                        then_body: self.statement_or_null(&stmt.nodes.2)?,
                        else_body: Vec::new(),
                    }]);
                }
            },
            sv_parser::ImmediateAssertionStatement::Deferred(assertion) => match &**assertion {
                sv_parser::DeferredImmediateAssertionStatement::Assert(stmt) => {
                    ("assert", &stmt.nodes.2.nodes.1, Some(&stmt.nodes.3))
                }
                sv_parser::DeferredImmediateAssertionStatement::Assume(stmt) => {
                    ("assume", &stmt.nodes.2.nodes.1, Some(&stmt.nodes.3))
                }
                sv_parser::DeferredImmediateAssertionStatement::Cover(_) => {
                    return Err(unsupported("deferred immediate cover"));
                }
            },
        };
        let condition = procedural_truth_condition(self.expr(expr)?);
        let (pass, fail) = match action {
            Some(sv_parser::ActionBlock::StatementOrNull(stmt)) => {
                (self.statement_or_null(stmt)?, None)
            }
            Some(sv_parser::ActionBlock::Else(action)) => (
                match &action.nodes.0 {
                    Some(stmt) => self.statement(stmt)?,
                    None => Vec::new(),
                },
                Some(self.statement_or_null(&action.nodes.2)?),
            ),
            None => (Vec::new(), None),
        };
        let fail = fail.unwrap_or_else(|| {
            vec![Stmt::SystemTask {
                name: "$error".to_string(),
                args: vec![SystemTaskArg::Str(format!("{keyword} failed"))],
            }]
        });
        Ok(vec![Stmt::If {
            condition,
            then_body: pass,
            else_body: fail,
        }])
    }
}

/// Whether a delay or event control is absent (sv-parser models the omitted
/// control of `a = b` as an empty one).
fn delay_or_event_control_is_empty(control: &sv_parser::DelayOrEventControl) -> bool {
    RefNode::DelayOrEventControl(control)
        .into_iter()
        .all(|node| {
            !matches!(
                node,
                RefNode::Delay3(_) | RefNode::DelayControl(_) | RefNode::EventControl(_)
            )
        })
}
/// The declared arguments of a task or function: `(name, direction, type,
/// default)`. An argument without a direction takes the previous one's, and
/// one without a type the previous one's type (IEEE 1800-2023 13.3).
pub(super) fn subroutine_param_declarations<'t>(
    list: Option<&'t sv_parser::TfPortList>,
    items: &'t [sv_parser::TfItemDeclaration],
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<
    Vec<(
        String,
        ParamDirection,
        Type,
        Option<&'t sv_parser::Expression>,
    )>,
    AnalyzerError,
> {
    let direction_of = |direction: &sv_parser::TfPortDirection| match direction {
        sv_parser::TfPortDirection::PortDirection(direction) => match &**direction {
            sv_parser::PortDirection::Input(_) => Ok(ParamDirection::Input),
            sv_parser::PortDirection::Output(_) => Ok(ParamDirection::Output),
            sv_parser::PortDirection::Inout(_) => Ok(ParamDirection::Inout),
            sv_parser::PortDirection::Ref(_) => Err(unsupported("ref subroutine argument")),
        },
        sv_parser::TfPortDirection::ConstRef(_) => Err(unsupported("ref subroutine argument")),
    };
    let unpacked = |dimensions: &[sv_parser::VariableDimension]| {
        unpacked_ranges_from_variable_dimensions_with_env(dimensions, tree, const_env, type_aliases)
    };
    let mut params = Vec::new();
    if let Some(list) = list {
        let mut direction = ParamDirection::Input;
        let mut previous_type: Option<Type> = None;
        for port in list.nodes.0.contents() {
            if let Some(declared) = &port.nodes.1 {
                direction = direction_of(declared)?;
            }
            let type_node = RefNode::DataTypeOrImplicit(&port.nodes.3);
            let omitted_type = matches!(
                port.nodes.3,
                sv_parser::DataTypeOrImplicit::ImplicitDataType(_)
            ) && is_signed_from_ref_node(type_node.clone()).is_none()
                && function_type_from_ref_node(type_node.clone(), tree, const_env, type_aliases)
                    .is_none_or(|r#type| r#type.packed_ranges().is_empty());
            let declared_type =
                || function_type_from_ref_node(type_node.clone(), tree, const_env, type_aliases);
            let (name, r#type, dimensions, default) = match port.nodes.4.as_ref() {
                Some((identifier, dimensions, default)) => {
                    let name = identifier_text(RefNode::PortIdentifier(identifier), tree)
                        .ok_or_else(|| unsupported("subroutine argument"))?;
                    let r#type = if port.nodes.1.is_none() && omitted_type {
                        previous_type.clone().or_else(declared_type)
                    } else {
                        declared_type()
                    };
                    (
                        name,
                        r#type,
                        dimensions.as_slice(),
                        default.as_ref().map(|(_, expr)| expr),
                    )
                }
                None => {
                    // sv-parser reads the `b` of `input a, b` as a type name.
                    if type_alias_from_data_type_or_implicit(&port.nodes.3, tree, type_aliases)
                        .is_some()
                    {
                        return Err(unsupported("subroutine argument without a name"));
                    }
                    // An empty `()` list parses as one item without a name.
                    let Some(name) = identifier_text(type_node, tree) else {
                        continue;
                    };
                    (name, previous_type.clone(), &[][..], None)
                }
            };
            let r#type = match r#type {
                Some(r#type) => r#type,
                // A data type that is not integral, such as `real`.
                None if port.nodes.4.is_some()
                    && matches!(port.nodes.3, sv_parser::DataTypeOrImplicit::DataType(_)) =>
                {
                    return Err(unsupported("unsupported function formal data type"));
                }
                None => Type::new(TypeKind::Logic),
            };
            previous_type = Some(r#type.clone());
            let r#type = type_with_unpacked_ranges(r#type, unpacked(dimensions)?);
            params.push((name, direction, r#type, default));
        }
    }
    for item in items {
        let sv_parser::TfItemDeclaration::TfPortDeclaration(declaration) = item else {
            continue;
        };
        let direction = direction_of(&declaration.nodes.1)?;
        let r#type = match function_type_from_ref_node(
            RefNode::DataTypeOrImplicit(&declaration.nodes.3),
            tree,
            const_env,
            type_aliases,
        ) {
            Some(r#type) => r#type,
            None if matches!(
                declaration.nodes.3,
                sv_parser::DataTypeOrImplicit::DataType(_)
            ) =>
            {
                return Err(unsupported("unsupported function formal data type"));
            }
            None => Type::new(TypeKind::Logic),
        };
        for (identifier, dimensions, default) in declaration.nodes.4.nodes.0.contents() {
            let name = identifier_text(RefNode::PortIdentifier(identifier), tree)
                .ok_or_else(|| unsupported("subroutine argument"))?;
            params.push((
                name,
                direction,
                type_with_unpacked_ranges(r#type.clone(), unpacked(dimensions)?),
                default.as_ref().map(|(_, expr)| expr),
            ));
        }
    }
    Ok(params)
}

/// The syntax of one function or task body.
struct SubroutineSyntax<'t> {
    name: String,
    is_task: bool,
    return_type: Option<&'t sv_parser::FunctionDataTypeOrImplicit>,
    ports: Option<&'t sv_parser::TfPortList>,
    items: &'t [sv_parser::TfItemDeclaration],
    block_items: Vec<&'t sv_parser::BlockItemDeclaration>,
    statements: Vec<&'t sv_parser::Statement>,
}

fn function_syntax<'t>(
    declaration: &'t sv_parser::FunctionDeclaration,
    tree: &SyntaxTree,
) -> Option<SubroutineSyntax<'t>> {
    let statements = |list: &'t [sv_parser::FunctionStatementOrNull]| {
        list.iter()
            .filter_map(|stmt| match stmt {
                sv_parser::FunctionStatementOrNull::Statement(stmt) => Some(&stmt.nodes.0),
                sv_parser::FunctionStatementOrNull::Attribute(_) => None,
            })
            .collect::<Vec<_>>()
    };
    Some(match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => SubroutineSyntax {
            name: identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), tree)?,
            is_task: false,
            return_type: Some(&body.nodes.0),
            ports: body.nodes.3.nodes.1.as_ref(),
            items: &[],
            block_items: body.nodes.5.iter().collect(),
            statements: statements(&body.nodes.6),
        },
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => SubroutineSyntax {
            name: identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), tree)?,
            is_task: false,
            return_type: Some(&body.nodes.0),
            ports: None,
            items: &body.nodes.4,
            block_items: body
                .nodes
                .4
                .iter()
                .filter_map(|item| match item {
                    sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                    sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
                })
                .collect(),
            statements: statements(&body.nodes.5),
        },
    })
}

fn task_syntax<'t>(
    declaration: &'t sv_parser::TaskDeclaration,
    tree: &SyntaxTree,
) -> Option<SubroutineSyntax<'t>> {
    let statements = |list: &'t [sv_parser::StatementOrNull]| {
        list.iter()
            .filter_map(|stmt| match stmt {
                sv_parser::StatementOrNull::Statement(stmt) => Some(&**stmt),
                sv_parser::StatementOrNull::Attribute(_) => None,
            })
            .collect::<Vec<_>>()
    };
    Some(match &declaration.nodes.2 {
        sv_parser::TaskBodyDeclaration::WithPort(body) => SubroutineSyntax {
            name: identifier_text(RefNode::TaskIdentifier(&body.nodes.1), tree)?,
            is_task: true,
            return_type: None,
            ports: body.nodes.2.nodes.1.as_ref(),
            items: &[],
            block_items: body.nodes.4.iter().collect(),
            statements: statements(&body.nodes.5),
        },
        sv_parser::TaskBodyDeclaration::WithoutPort(body) => SubroutineSyntax {
            name: identifier_text(RefNode::TaskIdentifier(&body.nodes.1), tree)?,
            is_task: true,
            return_type: None,
            ports: None,
            items: &body.nodes.3,
            block_items: body
                .nodes
                .3
                .iter()
                .filter_map(|item| match item {
                    sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                    sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
                })
                .collect(),
            statements: statements(&body.nodes.4),
        },
    })
}

/// The argument names of every function and task in the active generate
/// items, in declaration order, keyed by their (qualified) names.
pub(super) fn subroutine_argument_names(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<
    (
        HashMap<String, Vec<String>>,
        HashMap<String, Vec<VariableDimensions>>,
    ),
    AnalyzerError,
> {
    let mut names = HashMap::default();
    let mut shapes = HashMap::default();
    for item in generate::items(node, tree, const_env, type_aliases)? {
        for child in item.node.node() {
            let syntax = match child {
                RefNode::FunctionDeclaration(declaration) => function_syntax(declaration, tree),
                RefNode::TaskDeclaration(declaration) => task_syntax(declaration, tree),
                _ => continue,
            };
            let Some(syntax) = syntax else {
                continue;
            };
            let params = subroutine_param_declarations(
                syntax.ports,
                syntax.items,
                tree,
                &item.env,
                type_aliases,
            )?;
            let qualified = item.name(&syntax.name);
            let param_shapes: Vec<VariableDimensions> = params
                .iter()
                .map(|(_, _, r#type, _)| {
                    dimensions_from_type(&scoped_type(r#type.clone(), &item.env))
                })
                .collect();
            let params: Vec<String> = params.into_iter().map(|(name, ..)| name).collect();
            names.insert(syntax.name.clone(), params.clone());
            names.insert(qualified.clone(), params);
            shapes.insert(syntax.name.clone(), param_shapes.clone());
            shapes.insert(qualified, param_shapes);
        }
    }
    Ok((names, shapes))
}

/// Every function and task of the active generate items, with statement
/// bodies.
pub(super) fn subroutines_from_module_node(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    subroutines_from_module_node_with(
        node,
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
pub(super) fn subroutines_from_module_node_with(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    mut rejected: Option<&mut HashMap<String, AnalyzerError>>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    let type_aliases = packed_dimensions.type_aliases.clone();
    let mut subroutines = Vec::new();
    let active = generate::items(node, tree, const_env, &type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for item in &active {
        if item.is_parameter_declaration() {
            continue;
        }
        for child in item.node.node() {
            let syntax = match child {
                RefNode::FunctionDeclaration(declaration) => function_syntax(declaration, tree),
                RefNode::TaskDeclaration(declaration) => task_syntax(declaration, tree),
                _ => continue,
            };
            let Some(syntax) = syntax else {
                continue;
            };
            let (item_dimensions, literals) = views.get(item);
            let name = syntax.name.clone();
            let lowered = (|| -> Result<Subroutine, AnalyzerError> {
                let params = subroutine_param_declarations(
                    syntax.ports,
                    syntax.items,
                    tree,
                    &item.env,
                    &type_aliases,
                )?;
                let return_type = match syntax.return_type {
                    Some(node) => function_type_from_ref_node(
                        RefNode::FunctionDataTypeOrImplicit(node),
                        tree,
                        &item.env,
                        &type_aliases,
                    )
                    .or_else(|| match node {
                        // `function f` without a type returns one bit.
                        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(_) => {
                            Some(Type::new(TypeKind::Logic))
                        }
                        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(_) => None,
                    }),
                    None => None,
                };
                let mut builder = BodyBuilder::new(
                    tree,
                    item_dimensions,
                    state,
                    system_functions::Body::Subroutine,
                );
                builder.push_scope();
                let mut lowered_params = Vec::new();
                for (source, direction, r#type, default) in params {
                    let default = default.map(|expr| builder.expr(expr)).transpose()?;
                    let name = builder.declare(&source, r#type.clone());
                    lowered_params.push(SubroutineParam {
                        name,
                        source_name: source,
                        direction,
                        r#type: crate::ir::Type::from_ast(
                            scoped_type(r#type, &item.env),
                            &item.env,
                        ),
                        default,
                    });
                }
                let return_var = return_type
                    .as_ref()
                    .map(|r#type| builder.declare(&syntax.name, r#type.clone()));
                let mut body = Vec::new();
                for block_item in &syntax.block_items {
                    body.extend(builder.block_item_declaration(block_item)?);
                }
                for stmt in &syntax.statements {
                    body.extend(builder.statement(stmt)?);
                }
                builder.pop_scope();
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
                    name: item.name(&syntax.name),
                    is_task: syntax.is_task,
                    return_type: return_type.map(|r#type| {
                        crate::ir::Type::from_ast(scoped_type(r#type, &item.env), &item.env)
                    }),
                    params: lowered_params,
                    return_var,
                    body,
                };
                for stmt in &mut subroutine.body {
                    qualify_stmt(item, stmt);
                    substitute_stmt_constants(stmt, &item.env, literals);
                }
                for param in &mut subroutine.params {
                    if let Some(default) = &mut param.default {
                        item.expr(default);
                        *default = substitute_expr_constants_with_parameter_literals(
                            default.clone(),
                            &item.env,
                            literals,
                        );
                    }
                }
                Ok(subroutine)
            })();
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

/// Qualify the module-scope names of a statement for its generate scope.
pub(super) fn qualify_stmt(item: &generate::Item<'_>, stmt: &mut Stmt) {
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
    stmt: &mut Stmt,
    const_env: &HashMap<String, i128>,
    literals: &HashMap<String, Expr>,
) {
    stmt.visit_mut(
        &mut |expr| {
            *expr = substitute_expr_constants_with_parameter_literals(
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
pub(super) fn initial_processes_from_module_node(
    node: RefNode<'_>,
    tree: &SyntaxTree,
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
    let active = generate::items(node, tree, const_env, &type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for item in &active {
        if item.is_parameter_declaration() {
            continue;
        }
        if let Some(sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data)) =
            item.node.declaration()
            && let sv_parser::DataDeclaration::Variable(variable) = &**data
        {
            if !variable.nodes.4.nodes.0.contents().into_iter().any(|assignment| matches!(assignment, sv_parser::VariableDeclAssignment::Variable(assignment) if assignment.nodes.2.is_some())) {
                continue;
            }
            let (item_dimensions, literals) = views.get(item);
            let mut body = Vec::new();
            for assignment in variable.nodes.4.nodes.0.contents() {
                let sv_parser::VariableDeclAssignment::Variable(assignment) = assignment else {
                    continue;
                };
                let Some((_, expr)) = &assignment.nodes.2 else {
                    continue;
                };
                let name = identifier_text(RefNode::VariableIdentifier(&assignment.nodes.0), tree)
                    .ok_or_else(|| unsupported("variable declaration initializer"))?;
                if let Some(target) = selected_unpacked_shape(&name, 0, item_dimensions) {
                    check_unpacked_array_assignment(
                        expr,
                        &target,
                        || format!("initializer of `{name}`"),
                        tree,
                        item_dimensions,
                    )?;
                }
                let lhs = LValue::Ident(name);
                let rhs = expr_from_expression_for_lvalue(expr, &lhs, tree, item_dimensions)?;
                body.push(Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                });
            }
            for stmt in &mut body {
                substitute_stmt_constants(stmt, &item.env, literals);
                qualify_stmt(item, stmt);
                substitute_stmt_constants(stmt, const_env, parameter_literals);
            }
            if !body.is_empty() {
                initializers.push(InitialProcess {
                    condition: None,
                    body,
                    initializer: true,
                });
            }
            continue;
        }
        let ScopeItem::Module(sv_parser::ModuleOrGenerateItem::ModuleItem(module_item)) = item.node
        else {
            continue;
        };
        let sv_parser::ModuleCommonItem::InitialConstruct(initial) = &module_item.nodes.1 else {
            continue;
        };
        let (item_dimensions, literals) = views.get(item);
        let mut builder = BodyBuilder::new(
            tree,
            item_dimensions,
            state,
            system_functions::Body::Initial,
        );
        let mut body = builder.statement_or_null(&initial.nodes.1)?;
        for stmt in &mut body {
            substitute_stmt_constants(stmt, &item.env, literals);
            qualify_stmt(item, stmt);
            substitute_stmt_constants(stmt, const_env, parameter_literals);
        }
        processes.push(InitialProcess {
            condition: None,
            body,
            initializer: false,
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
