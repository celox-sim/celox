//! Combinational process collection: `always_comb`, `always @*`, and continuous assignments.

use super::*;

pub(super) fn comb_processes_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Vec<CombProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    let active = generate::items(
        syntax_tree,
        session,
        const_env,
        &packed_dimensions.type_aliases,
    )?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for (creation_order, item) in active.iter().enumerate() {
        let syntax_tree = &item.scoped(syntax_tree);
        if !matches!(
            &syntax_tree.declarations().body_item(item.id).kind,
            celox_sv_syntax::BodyItemKind::Assign(_)
                | celox_sv_syntax::BodyItemKind::LogicGate(_)
                | celox_sv_syntax::BodyItemKind::Procedure(_)
                | celox_sv_syntax::BodyItemKind::Net(_)
                | celox_sv_syntax::BodyItemKind::Unconverted { .. }
        ) {
            continue;
        }
        let syntax_tree = &item.enter(syntax_tree);
        let start = processes.len();
        let (dimensions, literals) = views.get(item);
        comb_processes_from_item(
            item,
            None,
            syntax_tree,
            session,
            &item.env,
            dimensions,
            literals,
            &mut processes,
            state,
        )?;
        for process in &mut processes[start..] {
            process.creation_order = creation_order;
            let tokens = syntax_tree.declarations().body_item(item.id).tokens;
            process.site = procedural::source_span(tokens, syntax_tree);
            process.process_site = procedural::process_site("always", tokens, syntax_tree);
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
    session: &mut Session,
    const_env: &SharedEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &SharedMap<Expr>,
    processes: &mut Vec<CombProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    match &syntax_tree.declarations().body_item(item.id).kind {
        celox_sv_syntax::BodyItemKind::Assign(id) => {
            let assign = syntax_tree.declarations().assign_statement(*id);
            for mut process in
                continuous_assignments(assign, syntax_tree, session, packed_dimensions)?
            {
                process.condition = condition.clone();
                process.drive_strength = assign.strength.is_some();
                processes.push(substitute_process_constants_with_parameter_literals(
                    syntax_tree,
                    session,
                    process,
                    const_env,
                    parameter_literals,
                ));
            }
        }
        celox_sv_syntax::BodyItemKind::LogicGate(gate) => {
            for mut process in logic_gate_processes(gate, syntax_tree, session, packed_dimensions)?
            {
                process.condition = condition.clone();
                process.drive_strength = logic_gate_drive_strength(gate);
                processes.push(substitute_process_constants_with_parameter_literals(
                    syntax_tree,
                    session,
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
                session,
                &local_packed_dimensions,
                parameter_literals,
                state,
            )? {
                processes.push(substitute_process_constants(
                    syntax_tree,
                    session,
                    process,
                    const_env,
                ));
            }
        }
        celox_sv_syntax::BodyItemKind::Net(id) => {
            // `wire [7:0] w = expr;` declares a net and drives it continuously.
            let declaration = syntax_tree.declarations().net_declaration(*id);
            let delay = declaration
                .delay
                .as_ref()
                .map(|delay| {
                    continuous_delays::ticks(delay, syntax_tree, session, packed_dimensions)
                })
                .transpose()?;
            for assignment in
                net_declaration_assignments(declaration, syntax_tree, session, packed_dimensions)?
            {
                let assignment = substitute_assignment_constants_with_parameter_literals(
                    syntax_tree,
                    session,
                    assignment,
                    const_env,
                    parameter_literals,
                );
                let mut process = CombProcess::new(
                    CombProcessKind::ContinuousAssign,
                    condition
                        .clone()
                        .map(|condition| substitute_const_expr_constants(condition, const_env)),
                    vec![assignment],
                );
                process.delay = delay;
                process.drive_strength = declaration.strength.is_some();
                processes.push(process);
            }
            if delay.is_some_and(|ticks| ticks != 0) {
                // An uninitialized declarator delays the net, rather than
                // declaring a driver. Resolve this private marker once the
                // module's complete set of local drivers has been collected.
                for variable in declaration
                    .variables
                    .iter()
                    .filter(|variable| variable.value.is_none())
                {
                    let name = instances::normalize_identifier(&variable.name.spelling);
                    let mut process = CombProcess::new(
                        CombProcessKind::ContinuousAssign,
                        condition
                            .clone()
                            .map(|value| substitute_const_expr_constants(value, const_env)),
                        vec![Assignment::new(
                            LValue::Ident(name.clone()),
                            Expr::Ident(name),
                        )],
                    );
                    process.delay = delay;
                    process.net_delay = true;
                    processes.push(process);
                }
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

/// Scalar n-input and n-output truth tables (IEEE 1800-2023 28.4, 28.5): a
/// one-input gate, and `buf` and `not`, must also turn Z into X, which the
/// identity operand does.
///
/// An array of `n` instances (28.3.5) is one bitwise operation on `n` bits: a
/// one-bit terminal connects to every instance and an `n`-bit one gives each
/// instance one bit of it, the instance of the right-hand index its LSB
/// (28.3.6).
fn logic_gate_processes(
    gate: &celox_sv_syntax::LogicGate,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Vec<CombProcess>> {
    if let Some(primitive) = tree
        .declarations()
        .primitives
        .iter()
        .find(|primitive| primitive.name.value() == gate.kind)
    {
        return primitive_processes(primitive, gate, tree, session, dimensions);
    }
    if gate.strength.is_some() || gate.delay.is_some() {
        return Err(unsupported("gate primitive instantiation"));
    }
    let (op, identity, invert) = match gate.kind.as_str() {
        "and" | "buf" => (BinaryOp::BitAnd, "1'b1", false),
        "nand" | "not" => (BinaryOp::BitAnd, "1'b1", true),
        "or" => (BinaryOp::BitOr, "1'b0", false),
        "nor" => (BinaryOp::BitOr, "1'b0", true),
        "xor" => (BinaryOp::BitXor, "1'b0", false),
        "xnor" => (BinaryOp::BitXor, "1'b0", true),
        "bufif0" | "bufif1" => (BinaryOp::BitAnd, "1'b1", false),
        "notif0" | "notif1" => (BinaryOp::BitAnd, "1'b1", true),
        _ => return Err(unsupported("gate primitive instantiation")),
    };
    // The outputs of `buf` and `not` are all terminals but the last.
    let n_output = matches!(gate.kind.as_str(), "buf" | "not");
    // A three-state gate drives its data input, as `buf` or `not` does,
    // while its control input is 1 (`bufif1`, `notif1`) or 0 (`bufif0`,
    // `notif0`), and z while it is the other value (28.6, Table 28-5).
    let enabled_by = match gate.kind.as_str() {
        "bufif1" | "notif1" => Some(true),
        "bufif0" | "notif0" => Some(false),
        _ => None,
    };
    let mut processes = Vec::new();
    for instance in &gate.instances {
        let count = gate_instance_count(instance, tree, session, dimensions)?;
        let replicate = |expr: Expr| {
            if count == 1 {
                expr
            } else {
                Expr::RepeatConcat {
                    count: ConstExpr::Literal(count.to_string()),
                    parts: vec![expr],
                }
            }
        };
        // A terminal's value for each instance: the bits of `count`.
        let widen = |input: Expr, width: Option<usize>| match width {
            Some(1) => Some(replicate(input)),
            Some(width) if width == count => Some(Expr::Resize {
                expr: Box::new(input),
                width,
                signed: false,
            }),
            _ => None,
        };
        let mismatch = |width: Option<usize>| match width {
            // Too many or too few bits for the instances are an error.
            Some(width) if count > 1 => Err(AnalyzerError::Parse(format!(
                "{} gate instance array: a {width}-bit terminal, where {count} instances need 1 or {count} bits (IEEE 1800-2023 28.3.6)",
                gate.kind
            ))),
            _ => Err(unsupported("non-scalar gate terminal")),
        };
        let outputs = if n_output {
            instance.terminals.len() - 1
        } else {
            1
        };
        let (outputs, mut inputs) = instance.terminals.split_at(outputs);
        let mut control = None;
        if enabled_by.is_some() {
            let [data, enable] = inputs else {
                return Err(AnalyzerError::Parse(format!(
                    "{} gate without one data and one control terminal",
                    gate.kind
                )));
            };
            let enable =
                expression_binding::value(*enable, tree.declarations(), tree, session, dimensions)?;
            let width = expr_static_width(tree, &enable, dimensions);
            let Some(enable) = widen(enable, width) else {
                return mismatch(width);
            };
            control = Some(enable);
            inputs = std::slice::from_ref(data);
        }
        let mut rhs = replicate(Expr::Literal(identity.into()));
        for &input in inputs {
            let input =
                expression_binding::value(input, tree.declarations(), tree, session, dimensions)?;
            let width = expr_static_width(tree, &input, dimensions);
            let Some(input) = widen(input, width) else {
                return mismatch(width);
            };
            rhs = Expr::Binary {
                left: Box::new(rhs),
                op,
                right: Box::new(input),
            };
        }
        if invert {
            rhs = Expr::Unary {
                op: UnaryOp::BitNot,
                expr: Box::new(rhs),
            };
        }
        // A control input of x or z gives L, H or x (Table 28-5): with the
        // high impedance of a disabled gate, the conditional operator
        // yields x for the 0, 1 or x of the data (11.4.11), the value of L
        // and H on a net with no other driver. Such a gate is the only
        // driver of its net (see `logic_gate_drive_strength`). Each
        // instance of an array has its own condition.
        if let (Some(control), Some(enabled_by)) = (control, enabled_by) {
            let bit = |expr: &Expr, bit: usize| {
                if count == 1 {
                    expr.clone()
                } else {
                    Expr::Select {
                        expr: Box::new(expr.clone()),
                        msb: ConstExpr::Literal(bit.to_string()),
                        lsb: ConstExpr::Literal(bit.to_string()),
                        signed: false,
                    }
                }
            };
            let mut instances = (0..count)
                .rev()
                .map(|index| {
                    let high_impedance = Expr::Literal("1'bz".into());
                    let (then_expr, else_expr) = if enabled_by {
                        (bit(&rhs, index), high_impedance)
                    } else {
                        (high_impedance, bit(&rhs, index))
                    };
                    Expr::Mux {
                        condition: Box::new(bit(&control, index)),
                        then_expr: Box::new(then_expr),
                        else_expr: Box::new(else_expr),
                    }
                })
                .collect::<Vec<_>>();
            rhs = if count == 1 {
                instances.pop().unwrap()
            } else {
                Expr::Concat(instances)
            };
        }
        for &output in outputs {
            // An output is a net lvalue or a concatenation of them, nested
            // to any depth.
            let mut pending = vec![output];
            let mut parts = Vec::new();
            while let Some(id) = pending.pop() {
                if let celox_sv_syntax::ExprKind::Concatenation(items) =
                    &tree.declarations().expression(id).kind
                {
                    pending.extend(items.iter().rev().copied());
                } else {
                    parts.push(expression_binding::net_lvalue(
                        id,
                        tree.declarations(),
                        tree,
                        session,
                        dimensions,
                    )?);
                }
            }
            let width = parts.iter().try_fold(0, |total, part| {
                Some(total + lvalue_expr_type(&tree.functions, part, dimensions)?.width)
            });
            if width == Some(count) {
                processes.push(gate_output_process(tree, parts, rhs.clone(), dimensions));
            } else if width == Some(1) {
                // Every instance drives the one-bit output.
                for bit in 0..count {
                    let rhs = if count == 1 {
                        rhs.clone()
                    } else {
                        Expr::Select {
                            expr: Box::new(rhs.clone()),
                            msb: ConstExpr::Literal(bit.to_string()),
                            lsb: ConstExpr::Literal(bit.to_string()),
                            signed: false,
                        }
                    };
                    processes.push(gate_output_process(tree, parts.clone(), rhs, dimensions));
                }
            } else {
                return mismatch(width);
            }
        }
    }
    Ok(processes)
}

/// Whether a gate's net keeps the single-driver rule as that of a driver
/// with a drive strength does. A three-state gate outputs L or H while its
/// control input is x or z (IEEE 1800-2023 28.6): with another driver of
/// the net they resolve by strength (28.12.2), which nets do not model.
fn logic_gate_drive_strength(gate: &celox_sv_syntax::LogicGate) -> bool {
    gate.strength.is_some()
        || matches!(
            gate.kind.as_str(),
            "bufif0" | "bufif1" | "notif0" | "notif1"
        )
}

/// The number of instances that a gate instance declares (IEEE 1800-2023
/// 28.3.5): `abs(lhi - rhi) + 1` for a range, one without.
fn gate_instance_count(
    instance: &celox_sv_syntax::LogicGateInstance,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<usize> {
    match instance.dimensions.as_slice() {
        [] => Ok(1),
        [dimension] => {
            let range =
                constant_binding::bind_unpacked_dimension(dimension, tree, session, dimensions)?;
            let bound = |expr| eval_ast_const_expr(&tree.functions, expr, &dimensions.const_env);
            bound(range.left())
                .zip(bound(range.right()))
                .map(|(left, right)| left.abs_diff(right))
                .filter(|&span| span < 4096)
                .map(|span| span as usize + 1)
                .ok_or_else(|| {
                    unsupported(
                        "gate primitive instantiation with nonconstant instance array bounds",
                    )
                })
        }
        _ => Err(unsupported(
            "gate primitive instantiation with a multidimensional instance array",
        )),
    }
}

/// The continuous assignment of a gate output: one net lvalue, or the parts
/// of a concatenation.
fn gate_output_process(
    tree: &AnalysisContext<'_>,
    mut parts: Vec<LValue>,
    rhs: Expr,
    dimensions: &PackedDimensions,
) -> CombProcess {
    if parts.len() == 1 {
        let lhs = parts.pop().unwrap();
        let rhs = if matches!(
            lhs,
            LValue::Select {
                is_2state: true,
                ..
            }
        ) {
            coerce_procedural_assignment_rhs(tree, rhs, &lhs, dimensions)
        } else {
            rhs
        };
        return CombProcess::new(
            CombProcessKind::ContinuousAssign,
            None,
            vec![Assignment::new(lhs, rhs)],
        );
    }
    CombProcess::procedural(
        CombProcessKind::ContinuousAssign,
        None,
        vec![Stmt::AssignConcat {
            parts,
            rhs,
            nonblocking: false,
        }],
    )
}

fn always_dimensions(
    dimensions: &PackedDimensions,
    const_env: &SharedEnv,
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
    session: &mut Session,
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
                syntax_tree,
                session,
                value,
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
            session,
            packed_dimensions,
        )?;
        assignments.push(Assignment::new(lhs, rhs));
    }
    Ok(assignments)
}

/// The continuous assignments of the outputs of the instances `gate` of
/// combinational user-defined primitive `primitive` (IEEE 1800-2023 29.3):
/// the output of the first table entry whose input symbols match the
/// inputs, or x when none does. An input matches `0` or `1` when it has
/// that value, `b` when it has either, `x` when it has neither (z counts as
/// x, 29.3.5), and `?` always.
fn primitive_processes(
    primitive: &celox_sv_syntax::Primitive,
    gate: &celox_sv_syntax::LogicGate,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Vec<CombProcess>> {
    let name = primitive.name.value();
    if gate.strength.is_some()
        || gate
            .instances
            .iter()
            .any(|instance| !instance.dimensions.is_empty())
    {
        return Err(unsupported(
            "user-defined primitive instance with a drive strength or an array of instances",
        ));
    }
    let delay = gate
        .delay
        .as_ref()
        .map(|delay| continuous_delays::ticks(delay, tree, session, dimensions))
        .transpose()?;
    let native = tree.declarations();
    let mut processes = Vec::new();
    for instance in &gate.instances {
        if instance.terminals.len() != primitive.ports.len() {
            return Err(AnalyzerError::Parse(format!(
                "instance of user-defined primitive `{name}` with {} terminals for its {} ports",
                instance.terminals.len(),
                primitive.ports.len()
            )));
        }
        let mut output = instance.terminals[0];
        while let celox_sv_syntax::ExprKind::Concatenation(parts) = &native.expression(output).kind
        {
            if parts.len() != 1 {
                return Err(unsupported("non-scalar user-defined primitive terminal"));
            }
            output = parts[0];
        }
        let lhs = expression_binding::net_lvalue(output, native, tree, session, dimensions)?;
        if !lvalue_expr_type(&tree.functions, &lhs, dimensions).is_some_and(|ty| ty.width == 1) {
            return Err(unsupported("non-scalar user-defined primitive terminal"));
        }
        let mut inputs = Vec::new();
        for &input in &instance.terminals[1..] {
            let input = expression_binding::value(input, native, tree, session, dimensions)?;
            if expr_static_width(tree, &input, dimensions) != Some(1) {
                return Err(unsupported("non-scalar user-defined primitive terminal"));
            }
            inputs.push(input);
        }
        // A sequential UDP drives its output with its state (see
        // `primitives`).
        let rhs = if primitive.sequential {
            Expr::Ident(primitives::state_name(gate, instance))
        } else {
            let mut rhs = Expr::Literal("1'bx".into());
            for row in primitive.rows.iter().rev() {
                let condition = row
                    .inputs
                    .iter()
                    .zip(&inputs)
                    .filter_map(|(symbol, value)| primitives::level(symbol, value))
                    .reduce(|left, right| Expr::Binary {
                        left: Box::new(left),
                        op: BinaryOp::LogicAnd,
                        right: Box::new(right),
                    });
                let value = Expr::Literal(
                    match row.output.as_str() {
                        "0" => "1'b0",
                        "1" => "1'b1",
                        _ => "1'bx",
                    }
                    .into(),
                );
                rhs = match condition {
                    Some(condition) => Expr::Mux {
                        condition: Box::new(condition),
                        then_expr: Box::new(value),
                        else_expr: Box::new(rhs),
                    },
                    None => value,
                };
            }
            rhs
        };
        let mut process = CombProcess::procedural(
            CombProcessKind::ContinuousAssign,
            None,
            vec![Stmt::Assign {
                lhs,
                rhs,
                nonblocking: false,
            }],
        );
        process.delay = delay;
        // The output of a sequential UDP has its initial value when
        // simulation begins, before any delay (29.3.3).
        if primitive.sequential
            && let Some(value) = primitive.initial
        {
            process.delay_initial = Some(expression_binding::value(
                value, native, tree, session, dimensions,
            )?);
        }
        processes.push(process);
    }
    Ok(processes)
}

/// The continuous connections of explicitly named ports to their
/// expressions (IEEE 1800-2023 23.2.2.2): an input port drives its
/// expression, and the expression of an output port drives the port.
pub(super) fn port_connections(
    ports: &[Port],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Vec<CombProcess>> {
    let native = tree.declarations();
    let mut processes = Vec::new();
    for port in ports {
        let Some(expression) = port.expression() else {
            continue;
        };
        let value = Expr::Ident(port.name().to_string());
        let stmt = match port.direction() {
            PortDirection::Input => match &native.expression(expression).kind {
                celox_sv_syntax::ExprKind::Concatenation(parts) => Stmt::AssignConcat {
                    parts: parts
                        .iter()
                        .map(|&part| {
                            expression_binding::net_lvalue(part, native, tree, session, dimensions)
                        })
                        .collect::<Converted<_>>()?,
                    rhs: value,
                    nonblocking: false,
                },
                _ => Stmt::Assign {
                    lhs: expression_binding::net_lvalue(
                        expression, native, tree, session, dimensions,
                    )?,
                    rhs: value,
                    nonblocking: false,
                },
            },
            PortDirection::Output => Stmt::Assign {
                lhs: LValue::Ident(port.name().to_string()),
                rhs: expression_binding::value(expression, native, tree, session, dimensions)?,
                nonblocking: false,
            },
            _ => {
                return Err(unsupported(
                    "explicitly named port that is not an input or an output",
                ));
            }
        };
        processes.push(CombProcess::procedural(
            CombProcessKind::ContinuousAssign,
            None,
            vec![stmt],
        ));
    }
    Ok(processes)
}

/// Native continuous assignment membership and values drive lowering.
fn continuous_assignments(
    native: &celox_sv_syntax::AssignSyntax,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
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
    let delay = native
        .delay
        .as_ref()
        .map(|delay| continuous_delays::ticks(delay, tree, session, dimensions))
        .transpose()?;
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
                            session,
                            dimensions,
                        )?);
                    }
                }
                let rhs = expression_binding::value(
                    assignment.right,
                    native_tree,
                    tree,
                    session,
                    dimensions,
                )?;
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
                tree,
                session,
                dimensions,
            ) {
                array_compatibility::check_native_unpacked_assignment(
                    tree,
                    session,
                    assignment.right,
                    &shape,
                    || "continuous assignment".into(),
                    dimensions,
                )?;
            }
            let lhs = expression_binding::net_lvalue(
                assignment.left,
                tree.declarations(),
                tree,
                session,
                dimensions,
            )?;
            let rhs = match expression_binding::selected_pattern(
                assignment.left,
                assignment.right,
                &lhs,
                tree,
                session,
                dimensions,
            )? {
                Some(rhs) => rhs,
                None => expression_binding::expression(
                    assignment.right,
                    tree.declarations(),
                    &lhs,
                    tree,
                    session,
                    dimensions,
                )?,
            };
            // A selected string-array element receives text as a heap handle,
            // just as an identifier does; the flattened lvalue has no scalar shape.
            let rhs = if expression_binding::native_string(
                tree,
                session,
                assignment.left,
                native_tree,
                dimensions,
            ) {
                expression_binding::string_operand(rhs, dimensions)
            } else {
                rhs
            };
            // A variable of unpacked structure type: drive its leaves.
            if let LValue::Ident(name) = &lhs
                && let Some(parts) = unpacked_structs::targets(name, dimensions)
            {
                unpacked_structs::check_assignment(
                    name,
                    assignment.right,
                    tree,
                    session,
                    dimensions,
                )?;
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
            let rhs = if matches!(
                lhs,
                LValue::Select {
                    is_2state: true,
                    ..
                }
            ) {
                coerce_procedural_assignment_rhs(tree, rhs, &lhs, dimensions)
            } else {
                rhs
            };
            Ok(CombProcess::new(
                CombProcessKind::ContinuousAssign,
                None,
                vec![Assignment::new(lhs, rhs)],
            ))
        })
        .map(|process: Converted<CombProcess>| {
            process.map(|mut process| {
                process.delay = delay;
                process
            })
        })
        .collect()
}

fn comb_process_from_procedure(
    procedure: &celox_sv_syntax::Procedure,
    condition: Option<ConstExpr>,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Option<CombProcess>, AnalyzerError> {
    if matches!(
        procedure.kind,
        celox_sv_syntax::ProcedureKind::Initial | celox_sv_syntax::ProcedureKind::Final
    ) {
        return Ok(None);
    }
    let Some(body) = always_comb_body(procedure, syntax_tree.declarations())? else {
        return Ok(None);
    };
    let mut builder = procedural::BodyBuilder::new(
        syntax_tree,
        session,
        packed_dimensions,
        state,
        system_functions::Body::Always,
    );
    let mut body = builder.native_body(&[body], &procedure.authored_statements)?;
    for stmt in &mut body {
        procedural::substitute_stmt_constants(
            syntax_tree,
            builder.session,
            stmt,
            &ConstEnv::default(),
            parameter_literals,
        );
    }
    let sensitivity_body = std::mem::take(&mut builder.sensitivity_body);
    Ok(Some(CombProcess {
        sensitivity_body,
        ..CombProcess::procedural(
            match procedure.kind {
                celox_sv_syntax::ProcedureKind::AlwaysComb => CombProcessKind::AlwaysComb,
                celox_sv_syntax::ProcedureKind::AlwaysLatch => CombProcessKind::AlwaysLatch,
                _ => CombProcessKind::AlwaysStar,
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
        let env: SharedEnv = (0..4096).map(|i| (format!("P{i}"), i)).collect();
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
