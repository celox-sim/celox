//! Combinational process collection: `always_comb`, `always @*`, and continuous assignments.

use super::*;

pub(super) fn comb_processes_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Vec<CombProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    let active = generate::items(syntax_tree, const_env, &packed_dimensions.type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for (creation_order, item) in active.iter().enumerate() {
        let _interface_scope = item.interface_scope();
        if !matches!(
            &syntax_tree.declarations().body_item(item.id).kind,
            celox_sv_syntax::BodyItemKind::Assign(_)
                | celox_sv_syntax::BodyItemKind::Procedure(_)
                | celox_sv_syntax::BodyItemKind::Net(_)
                | celox_sv_syntax::BodyItemKind::Unconverted { .. }
        ) {
            continue;
        }
        let _constant_functions = item.enter();
        let start = processes.len();
        let (dimensions, literals) = views.get(item);
        comb_processes_from_item(
            item,
            None,
            syntax_tree,
            &item.env,
            dimensions,
            literals,
            &mut processes,
            state,
        )?;
        for process in &mut processes[start..] {
            process.creation_order = creation_order;
            for assignment in &mut process.assignments {
                item.assignment(assignment);
            }
            for stmt in process.body.iter_mut().chain(&mut process.sensitivity_body) {
                procedural::qualify_stmt(item, stmt);
            }
        }
    }
    Ok(processes)
}

fn comb_processes_from_item(
    item: &generate::Item,
    condition: Option<ConstExpr>,
    syntax_tree: &AnalysisContext<'_>,
    const_env: &SharedMap<i128>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &SharedMap<Expr>,
    processes: &mut Vec<CombProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    match &syntax_tree.declarations().body_item(item.id).kind {
        celox_sv_syntax::BodyItemKind::Assign(id) => {
            for mut process in continuous_assignments(
                syntax_tree.declarations().assign_statement(*id),
                syntax_tree,
                packed_dimensions,
            )? {
                process.condition = condition.clone();
                processes.push(substitute_process_constants_with_parameter_literals(
                    &syntax_tree.state,
                    process,
                    const_env,
                    parameter_literals,
                ));
            }
        }
        celox_sv_syntax::BodyItemKind::Procedure(id) => {
            let local_packed_dimensions =
                always_dimensions(packed_dimensions, const_env, parameter_literals);
            let procedure = syntax_tree.declarations().procedure(*id);
            if let Some(process) = comb_process_from_procedure(
                procedure,
                condition,
                syntax_tree,
                &local_packed_dimensions,
                parameter_literals,
                state,
            )? {
                processes.push(substitute_process_constants(
                    &syntax_tree.state,
                    process,
                    const_env,
                ));
            }
        }
        celox_sv_syntax::BodyItemKind::Net(id) => {
            // `wire [7:0] w = expr;` declares a net and drives it continuously.
            for assignment in net_declaration_assignments(
                syntax_tree.declarations().net_declaration(*id),
                syntax_tree,
                packed_dimensions,
            )? {
                let assignment = substitute_assignment_constants_with_parameter_literals(
                    &syntax_tree.state,
                    assignment,
                    const_env,
                    parameter_literals,
                );
                processes.push(CombProcess::new(
                    CombProcessKind::ContinuousAssign,
                    condition
                        .clone()
                        .map(|condition| substitute_const_expr_constants(condition, const_env)),
                    vec![assignment],
                ));
            }
        }
        celox_sv_syntax::BodyItemKind::Unconverted { category, .. } if category == "alias" => {
            return Err(AnalyzerError::Unsupported(
                "module-level net alias".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn always_dimensions(
    dimensions: &PackedDimensions,
    const_env: &SharedMap<i128>,
    literals: &SharedMap<Expr>,
) -> PackedDimensions {
    let mut local = dimensions.clone();
    // Preserve the same replacements while sharing their immutable contents.
    // Body-local constant declarations detach their own tables on mutation.
    local.const_env = const_env.clone();
    local.parameter_values = literals.clone();
    local
}

fn net_declaration_assignments(
    native: &celox_sv_syntax::NetDeclaration,
    syntax_tree: &AnalysisContext<'_>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    let mut assignments = Vec::new();
    for variable in &native.variables {
        let Some(value) = variable.value else {
            continue;
        };
        let name = instances::normalize_identifier(&variable.name.spelling);
        if let Some(target) = packed_dimensions.get(&name) {
            array_compatibility::check_native_unpacked_assignment(
                &syntax_tree.state,
                value,
                syntax_tree.declarations(),
                target,
                || "net declaration assignment".to_string(),
                packed_dimensions,
            )?;
        }
        let lhs = LValue::Ident(name);
        let rhs = expression_binding::expression(
            value,
            syntax_tree.declarations(),
            &lhs,
            syntax_tree,
            packed_dimensions,
        )?;
        assignments.push(Assignment::new(lhs, rhs));
    }
    Ok(assignments)
}

/// Native continuous assignment membership and values drive lowering.
fn continuous_assignments(
    native: &celox_sv_syntax::AssignSyntax,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Vec<CombProcess>> {
    if native.assignments.is_empty() {
        return Err(AnalyzerError::Parse(
            "native continuous assignment has no operands".into(),
        ));
    }
    let mut ranges = native
        .assignments
        .iter()
        .map(|assignment| assignment.tokens)
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|range| range.start);
    if ranges.iter().any(|range| range.start >= range.end)
        || ranges.windows(2).any(|pair| pair[0].end > pair[1].start)
    {
        return Err(AnalyzerError::Parse(
            "invalid native continuous assignment membership".into(),
        ));
    }
    if native.delay.is_some() {
        return Err(unsupported("delayed continuous assignment"));
    }
    native
        .assignments
        .iter()
        .map(|assignment| {
            let native_tree = tree.declarations();
            if matches!(
                native_tree.expression(assignment.left).kind,
                celox_sv_syntax::ExprKind::Concatenation(_)
            ) {
                let mut pending = vec![assignment.left];
                let mut parts = Vec::new();
                while let Some(id) = pending.pop() {
                    if let celox_sv_syntax::ExprKind::Concatenation(items) =
                        &native_tree.expression(id).kind
                    {
                        pending.extend(items.iter().rev().copied());
                    } else {
                        parts.push(expression_binding::net_lvalue(
                            id,
                            native_tree,
                            tree,
                            dimensions,
                        )?);
                    }
                }
                let rhs =
                    expression_binding::value(assignment.right, native_tree, tree, dimensions)?;
                return Ok(CombProcess::procedural(
                    CombProcessKind::ContinuousAssign,
                    None,
                    vec![Stmt::AssignConcat {
                        parts,
                        rhs,
                        nonblocking: false,
                    }],
                ));
            }
            if let Some(shape) = array_compatibility::native_unpacked_shape(
                assignment.left,
                tree.declarations(),
                dimensions,
            ) {
                array_compatibility::check_native_unpacked_assignment(
                    &tree.state,
                    assignment.right,
                    tree.declarations(),
                    &shape,
                    || "continuous assignment".into(),
                    dimensions,
                )?;
            }
            let lhs = expression_binding::net_lvalue(
                assignment.left,
                tree.declarations(),
                tree,
                dimensions,
            )?;
            let rhs = expression_binding::expression(
                assignment.right,
                tree.declarations(),
                &lhs,
                tree,
                dimensions,
            )?;
            let rhs = if matches!(
                lhs,
                LValue::Select {
                    is_2state: true,
                    ..
                }
            ) {
                coerce_procedural_assignment_rhs(&tree.state, rhs, &lhs, dimensions)
            } else {
                rhs
            };
            Ok(CombProcess::new(
                CombProcessKind::ContinuousAssign,
                None,
                vec![Assignment::new(lhs, rhs)],
            ))
        })
        .collect()
}

fn comb_process_from_procedure(
    procedure: &celox_sv_syntax::Procedure,
    condition: Option<ConstExpr>,
    syntax_tree: &AnalysisContext<'_>,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Option<CombProcess>, AnalyzerError> {
    if procedure.kind == celox_sv_syntax::ProcedureKind::Initial {
        return Ok(None);
    }
    let Some(body) = always_comb_body(procedure, syntax_tree.declarations())? else {
        return Ok(None);
    };
    let mut builder = procedural::BodyBuilder::new(
        syntax_tree,
        packed_dimensions,
        state,
        system_functions::Body::Always,
    );
    let mut body = builder.native_body(&[body], &procedure.authored_statements)?;
    for stmt in &mut body {
        procedural::substitute_stmt_constants(
            &syntax_tree.state,
            stmt,
            &HashMap::default(),
            parameter_literals,
        );
    }
    let sensitivity_body = std::mem::take(&mut builder.sensitivity_body);
    Ok(Some(CombProcess {
        sensitivity_body,
        ..CombProcess::procedural(
            if procedure.kind == celox_sv_syntax::ProcedureKind::AlwaysComb {
                CombProcessKind::AlwaysComb
            } else {
                CombProcessKind::AlwaysStar
            },
            condition,
            body,
        )
    }))
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn always_context_shares_large_inputs_and_detaches_body_local_mutations() {
        let env: SharedMap<i128> = (0..4096)
            .map(|i| (format!("P{i}"), i))
            .collect::<HashMap<_, _>>()
            .into();
        let literals: SharedMap<Expr> = (0..4096)
            .map(|i| (format!("P{i}"), Expr::Literal(i.to_string())))
            .collect::<HashMap<_, _>>()
            .into();
        let mut dimensions = PackedDimensions {
            scope_types_complete: true,
            ..PackedDimensions::default()
        };
        dimensions.const_env.insert("inherited".into(), 99);
        dimensions
            .parameter_values
            .insert("inherited".into(), Expr::Literal("99".into()));
        let mut local = always_dimensions(&dimensions, &env, &literals);
        assert_eq!(local.const_env.identity(), env.identity());
        assert_eq!(local.parameter_values.identity(), literals.identity());
        assert!(!local.const_env.contains_key("inherited"));
        assert!(!local.parameter_values.contains_key("inherited"));
        assert!(local.scope_types_complete);
        local.const_env.insert("P0".into(), 7);
        local
            .parameter_values
            .insert("P0".into(), Expr::Literal("7".into()));
        assert_eq!(env["P0"], 0);
        assert_eq!(literals["P0"], Expr::Literal("0".into()));
        assert_eq!(dimensions.const_env["inherited"], 99);
    }
}
