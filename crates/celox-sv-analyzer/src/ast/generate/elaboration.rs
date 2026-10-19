//! Native generate control and scope worklist.
use super::*;
use celox_sv_syntax::{BodyItemId, BodyItemKind, ExprKind, ForStep};
use std::{cell::Cell, rc::Rc};
type Ordinal = Rc<Cell<usize>>;
enum Job {
    ExitScope,
    Item {
        id: BodyItemId,
        scope: Rc<Scope>,
        ordinal: Ordinal,
    },
    Block {
        id: BodyItemId,
        parent: Rc<Scope>,
        ordinal: usize,
        index: Option<(String, i128)>,
    },
    Loop {
        id: BodyItemId,
        parent: Rc<Scope>,
        ordinal: usize,
        value: i128,
    },
    Update {
        id: BodyItemId,
        parent: Rc<Scope>,
        iteration: Rc<Scope>,
        ordinal: usize,
        value: i128,
    },
}
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("invalid native generate membership".into())
}
fn children(id: BodyItemId, native: &celox_sv_syntax::Declarations) -> Vec<BodyItemId> {
    match &native.body_item(id).kind {
        BodyItemKind::Region(items)
        | BodyItemKind::Block { items, .. }
        | BodyItemKind::Unconverted { items, .. } => items.clone(),
        BodyItemKind::Conditional {
            then_body,
            otherwise,
            ..
        } => std::iter::once(*then_body).chain(*otherwise).collect(),
        BodyItemKind::Case { arms, .. } => arms.iter().map(|arm| arm.body).collect(),
        BodyItemKind::Loop { body, .. } => vec![*body],
        _ => Vec::new(),
    }
}
fn contains_control(root: BodyItemId, native: &celox_sv_syntax::Declarations) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if matches!(
            native.body_item(id).kind,
            BodyItemKind::Conditional { .. }
                | BodyItemKind::Case { .. }
                | BodyItemKind::Loop { .. }
                | BodyItemKind::Region(_)
        ) {
            return true;
        }
        pending.extend(children(id, native));
    }
    false
}
fn contains_function(root: BodyItemId, native: &celox_sv_syntax::Declarations) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let BodyItemKind::Subroutine(id) = native.body_item(id).kind
            && native.subroutine(id).kind == celox_sv_syntax::SubroutineKind::Function
        {
            return true;
        }
        if let BodyItemKind::Unit(id) = native.body_item(id).kind {
            pending.extend(&native.unit(id).items);
        }
        pending.extend(children(id, native));
    }
    false
}
pub(super) fn needs_functions(tree: &AnalysisContext<'_>) -> Converted<bool> {
    let native = tree.declarations();
    let mut pending = tree.body_membership()?;
    let mut seen = HashSet::default();
    let mut expressions = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(mismatch());
        }
        match &native.body_item(id).kind {
            BodyItemKind::Conditional { condition, .. } => expressions.push(*condition),
            BodyItemKind::Case { selector, arms } => {
                expressions.push(*selector);
                expressions.extend(arms.iter().flat_map(|arm| arm.labels.iter().copied()));
            }
            BodyItemKind::Loop {
                initial,
                condition,
                step,
                ..
            } => {
                expressions.extend([*initial, *condition]);
                expressions.push(match step {
                    ForStep::Expression(id) => *id,
                    ForStep::Assignment { right, .. } => *right,
                });
            }
            _ => {}
        }
        pending.extend(children(id, native));
    }
    let mut seen = HashSet::default();
    while let Some(id) = expressions.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let ExprKind::Call { expression, .. } = native.expression(id).kind
            && !matches!(&native.expression(expression_binding::ungroup(expression, native)).kind, ExprKind::Name(name) if name.spelling.starts_with('$'))
        {
            return Ok(true);
        }
        expressions.extend(parameters::skipped::children(id, native));
    }
    Ok(false)
}
struct Context<'b> {
    outer: Cow<'b, HashMap<String, Type>>,
    _functions: Option<const_functions::Installed>,
}
struct Contexts<'b>(Vec<Context<'b>>);
impl Drop for Contexts<'_> {
    fn drop(&mut self) {
        while let Some(context) = self.0.pop() {
            drop(context);
        }
    }
}
impl<'a, 'b> Elaborator<'a, 'b> {
    pub(super) fn elaborate_root(
        &mut self,
        scope: Scope,
        literal_use: LiteralUse,
    ) -> Result<(), AnalyzerError> {
        let ids = self.tree.body_membership()?;
        let mut pending = ids.clone();
        let mut seen = HashSet::default();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                return Err(mismatch());
            }
            let native = self.tree.declarations();
            let owner = native.body_item(id).tokens;
            let nested = children(id, native);
            if nested.iter().any(|child| {
                let range = native.body_item(*child).tokens;
                range.start < owner.start || range.end > owner.end
            }) {
                return Err(mismatch());
            }
            pending.extend(nested);
        }
        let scope = Rc::new(scope);
        let ordinal = Rc::new(Cell::new(0));
        let mut jobs = ids
            .into_iter()
            .rev()
            .filter(|id| {
                !matches!(literal_use, LiteralUse::RootSubroutines)
                    || !contains_control(*id, self.tree.declarations())
            })
            .map(|id| Job::Item {
                id,
                scope: scope.clone(),
                ordinal: ordinal.clone(),
            })
            .collect::<Vec<_>>();
        let mut contexts = Contexts(Vec::new());
        while let Some(job) = jobs.pop() {
            if let Err(error) = self.execute_job(job, &mut jobs, &mut contexts) {
                while let Some(context) = contexts.0.pop() {
                    self.aliases = context.outer;
                    drop(context._functions);
                }
                return Err(error);
            }
        }
        if !contexts.0.is_empty() {
            return Err(mismatch());
        }
        Ok(())
    }
    fn execute_job(
        &mut self,
        job: Job,
        jobs: &mut Vec<Job>,
        contexts: &mut Contexts<'b>,
    ) -> Result<(), AnalyzerError> {
        match job {
            Job::ExitScope => {
                let context = contexts.0.pop().ok_or_else(mismatch)?;
                self.aliases = context.outer;
                drop(context._functions);
                Ok(())
            }
            Job::Item { id, scope, ordinal } => self.item_job(id, scope, ordinal, jobs),
            Job::Block {
                id,
                parent,
                ordinal,
                index,
            } => self.block_job(id, parent, ordinal, index, jobs, contexts),
            Job::Loop {
                id,
                parent,
                ordinal,
                value,
            } => self.loop_job(id, parent, ordinal, value, jobs),
            Job::Update {
                id,
                parent,
                iteration,
                ordinal,
                value,
            } => {
                let next = self.next_value(id, value, &iteration)?;
                jobs.push(Job::Loop {
                    id,
                    parent,
                    ordinal,
                    value: next,
                });
                Ok(())
            }
        }
    }
    fn item_job(
        &mut self,
        id: BodyItemId,
        scope: Rc<Scope>,
        ordinal: Ordinal,
        jobs: &mut Vec<Job>,
    ) -> Result<(), AnalyzerError> {
        let kind = &self.tree.declarations().body_item(id).kind;
        if let BodyItemKind::Region(items) = kind {
            for id in items.iter().rev() {
                jobs.push(Job::Item {
                    id: *id,
                    scope: scope.clone(),
                    ordinal: ordinal.clone(),
                });
            }
            return Ok(());
        }
        if matches!(kind, BodyItemKind::Unit(_)) {
            return Ok(());
        }
        if matches!(
            kind,
            BodyItemKind::Conditional { .. }
                | BodyItemKind::Case { .. }
                | BodyItemKind::Loop { .. }
        ) {
            let number = ordinal.get() + 1;
            ordinal.set(number);
            match kind {
                BodyItemKind::Conditional {
                    condition,
                    then_body,
                    otherwise,
                } => {
                    let selected = if self.condition(
                        *condition,
                        &scope,
                        "unknown conditional-generate condition",
                    )? {
                        Some(*then_body)
                    } else {
                        *otherwise
                    };
                    if let Some(id) = selected {
                        jobs.push(Job::Block {
                            id,
                            parent: scope,
                            ordinal: number,
                            index: None,
                        });
                    }
                }
                BodyItemKind::Case { selector, arms } => {
                    if let Some(id) = self.case_block(*selector, arms, &scope)? {
                        jobs.push(Job::Block {
                            id,
                            parent: scope,
                            ordinal: number,
                            index: None,
                        });
                    }
                }
                BodyItemKind::Loop {
                    variable,
                    initial,
                    step,
                    ..
                } => {
                    let name = instances::normalize_identifier(&variable.spelling);
                    if step_name(step, self.tree.declarations()).as_deref() != Some(name.as_str()) {
                        return Err(unsupported("genvar update targets a different variable"));
                    }
                    let value = self.value(*initial, &scope, "loop-generate initializer")?;
                    jobs.push(Job::Loop {
                        id,
                        parent: scope,
                        ordinal: number,
                        value,
                    });
                }
                _ => unreachable!(),
            }
            return Ok(());
        }
        if matches!(kind, BodyItemKind::Block { .. }) {
            return Err(mismatch());
        }
        if scope.in_loop && contains_function(id, self.tree.declarations()) {
            return Err(unsupported("function declaration inside loop-generate"));
        }
        self.push(id, &scope);
        Ok(())
    }
    fn block_job(
        &mut self,
        id: BodyItemId,
        parent: Rc<Scope>,
        ordinal: usize,
        index: Option<(String, i128)>,
        jobs: &mut Vec<Job>,
        contexts: &mut Contexts<'b>,
    ) -> Result<(), AnalyzerError> {
        let kind = &self.tree.declarations().body_item(id).kind;
        if index.is_none()
            && matches!(
                kind,
                BodyItemKind::Conditional { .. } | BodyItemKind::Case { .. }
            )
        {
            jobs.push(Job::Item {
                id,
                scope: parent,
                ordinal: Rc::new(Cell::new(ordinal - 1)),
            });
            return Ok(());
        }
        let (name, children) = match kind {
            BodyItemKind::Block {
                leading_label,
                label,
                items,
                ..
            } => (
                label
                    .as_ref()
                    .or(leading_label.as_ref())
                    .map(|name| instances::normalize_identifier(&name.spelling)),
                items.clone(),
            ),
            _ => (None, vec![id]),
        };
        let mut name = scope_component(&name.unwrap_or_else(|| format!("genblk{ordinal}")));
        if let Some((_, index)) = &index {
            name.push_str(&format!("[{index}]"));
        }
        let mut scope = (*parent).clone();
        scope.in_loop |= index.is_some();
        scope.path = if parent.path.is_empty() {
            name
        } else {
            format!("{}.{name}", parent.path)
        };
        let imported = scope::imported(&self.tree.state);
        if let Some(bindings) = imported.generate_imports.get(&id) {
            for (name, target) in bindings {
                bind_import(&mut scope, name, target);
            }
        }
        let index_ref = index.as_ref().map(|(name, value)| (name.as_str(), *value));
        let functions = scope.block_imports().install_constant_functions();
        let aliases = scope.block_imports().type_aliases(self.module_aliases);
        let outer = std::mem::replace(&mut self.aliases, aliases);
        contexts.0.push(Context {
            outer,
            _functions: functions,
        });
        let scope = Rc::new(self.prepare_block_contents(&children, scope, index_ref)?);
        jobs.push(Job::ExitScope);
        let counter = Rc::new(Cell::new(0));
        for id in children.into_iter().rev() {
            jobs.push(Job::Item {
                id,
                scope: scope.clone(),
                ordinal: counter.clone(),
            });
        }
        Ok(())
    }
    fn loop_job(
        &mut self,
        id: BodyItemId,
        parent: Rc<Scope>,
        ordinal: usize,
        value: i128,
        jobs: &mut Vec<Job>,
    ) -> Result<(), AnalyzerError> {
        let BodyItemKind::Loop {
            variable,
            condition,
            body,
            ..
        } = &self.tree.declarations().body_item(id).kind
        else {
            return Err(mismatch());
        };
        let name = instances::normalize_identifier(&variable.spelling);
        let value = i128::from(value as i32);
        let mut iteration = (*parent).clone();
        for marker in parameters::binding_keys(&name, &iteration.env, 0) {
            iteration.env.remove(&marker);
        }
        iteration.env.insert(name.clone(), value);
        insert_parameter_type_markers(
            &mut iteration.env,
            &name,
            ExprType {
                width: 32,
                signed: true,
            },
        );
        iteration.env.insert(parameter_rank_marker(&name), 1);
        iteration.names.remove(&name);
        Arc::make_mut(&mut iteration.shadowed).insert(name.clone());
        iteration
            .parameters
            .retain(|parameter| parameter.name() != name);
        iteration.parameter_dimensions =
            Arc::new(parameter_packed_dimensions(&iteration.parameters));
        iteration.literals.remove(&name);
        if !self.condition(*condition, &iteration, "loop-generate condition")? {
            return Ok(());
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| unsupported("loop-generate unroll limit exceeded"))?;
        let iteration = Rc::new(iteration);
        jobs.push(Job::Update {
            id,
            parent,
            iteration: iteration.clone(),
            ordinal,
            value,
        });
        jobs.push(Job::Block {
            id: *body,
            parent: iteration,
            ordinal,
            index: Some((name, value)),
        });
        Ok(())
    }
    fn next_value(
        &self,
        id: BodyItemId,
        value: i128,
        scope: &Scope,
    ) -> Result<i128, AnalyzerError> {
        let BodyItemKind::Loop { step, .. } = &self.tree.declarations().body_item(id).kind else {
            return Err(mismatch());
        };
        let result = match step {
            ForStep::Expression(id) => {
                let ExprKind::Unary { operator, .. } =
                    &self.tree.declarations().expression(*id).kind
                else {
                    return Err(mismatch());
                };
                match operator.as_str() {
                    "++" => value.checked_add(1),
                    "--" => value.checked_sub(1),
                    _ => None,
                }
            }
            ForStep::Assignment {
                operator, right, ..
            } => {
                let rhs = self.expression(*right, scope)?;
                rhs.and_then(|rhs| {
                    next_assignment(&self.tree.state, value, operator, rhs, &scope.env)
                })
            }
        };
        result.ok_or_else(|| unsupported("genvar update operator"))
    }
    fn case_block(
        &self,
        selector: ExprId,
        arms: &[celox_sv_syntax::GenerateCaseArm],
        scope: &Scope,
    ) -> Result<Option<BodyItemId>, AnalyzerError> {
        let selector = self
            .expression(selector, scope)?
            .ok_or_else(|| unsupported("unknown case-generate selector"))?;
        let types = parameter_types_from_const_env(&scope.env)
            .into_iter()
            .map(|(name, ty)| (name, (ty.width, ty.signed)))
            .collect::<HashMap<_, _>>();
        let integral = |expr: &ConstExpr| {
            typecheck::eval_const_integral_literal_with_types(
                &expr.clone().into(),
                &scope.env,
                &types,
            )
            .ok_or_else(|| unsupported("nonconstant case-generate expression"))
        };
        let selector_value =
            integral(&selector).map_err(|_| unsupported("unknown case-generate selector"))?;
        let self_width = |expr: &ConstExpr, width| {
            if matches!(expr,ConstExpr::Literal(literal)if resize_unbased_fill_literal_for_cast(literal,1,false).is_some())
            {
                1
            } else {
                width
            }
        };
        let mut width = self_width(&selector, selector_value.width);
        let mut signed = selector_value.signed;
        let mut labels = Vec::new();
        let mut default = None;
        for arm in arms {
            if arm.labels.is_empty() {
                if default.replace(arm.body).is_some() {
                    return Err(unsupported("duplicate case-generate default"));
                }
            } else {
                if !arm.colon {
                    return Err(mismatch());
                }
                for id in &arm.labels {
                    let label = self
                        .expression(*id, scope)?
                        .ok_or_else(|| unsupported("unknown case-generate label"))?;
                    let value = integral(&label)?;
                    width = width.max(self_width(&label, value.width));
                    signed &= value.signed;
                    labels.push((label, arm.body));
                }
            }
        }
        let normalize = |expr: ConstExpr| {
            typecheck::eval_generate_case_operand(&expr.into(), &scope.env, &types, width, signed)
                .ok_or_else(|| unsupported("case-generate comparison"))
        };
        let selector = normalize(selector)?;
        let mut selected = None;
        for (label, id) in labels {
            let label = normalize(label)?;
            if label.value == selector.value && label.mask == selector.mask && selected.is_none() {
                selected = Some(id);
            }
        }
        Ok(selected.or(default))
    }
}
fn step_name(step: &ForStep, native: &celox_sv_syntax::Declarations) -> Option<String> {
    let id = match step {
        ForStep::Assignment { left, .. } => *left,
        ForStep::Expression(id) => match native.expression(*id).kind {
            ExprKind::Unary { expression, .. } => expression,
            _ => return None,
        },
    };
    match &native.expression(id).kind {
        ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
        _ => None,
    }
}
fn next_assignment(
    state: &AnalysisState,
    value: i128,
    operator: &str,
    rhs: ConstExpr,
    env: &HashMap<String, i128>,
) -> Option<i128> {
    if operator == "=" {
        return eval_ast_const_expr(state, &rhs, env);
    }
    let op = match operator {
        "+=" => BinaryOp::Add,
        "-=" => BinaryOp::Sub,
        "*=" => BinaryOp::Mul,
        "/=" => BinaryOp::Div,
        "%=" => BinaryOp::Mod,
        "<<=" => BinaryOp::Shl,
        ">>=" => BinaryOp::Shr,
        _ => return None,
    };
    eval_ast_const_expr(
        state,
        &ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal(format_typed_parameter_literal(
                value, 32, true,
            ))),
            op,
            right: Box::new(rhs),
        },
        env,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_function_seeding_defers_explicit_region_declarations() {
        let tree=crate::source_tree::parse_scope_fixture("module M;function automatic int rootf();return 1;endfunction generate function automatic int regionf();return 2;endfunction endgenerate endmodule",Path::new("native-root-seeding.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let env = HashMap::default();
        let aliases = HashMap::default();
        let seeded =
            items_with_literal_use(&tree, &env, &aliases, LiteralUse::RootSubroutines).unwrap();
        let all = items_with_literal_use(&tree, &env, &aliases, LiteralUse::Subroutines).unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(all.len(), 2);
        assert!(
            matches!(tree.declarations().body_item(seeded[0].id).kind, BodyItemKind::Subroutine(id) if tree.declarations().subroutine(id).name.spelling == "rootf")
        );
    }
}
