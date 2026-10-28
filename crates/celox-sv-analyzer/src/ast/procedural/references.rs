//! Uses of reference formals and calls passing by reference that IEEE
//! 1800-2023 forbids, checked once the bodies are bound.
use super::*;
use crate::procedural::ParamDirection;
use lifetimes::{expr_reads, stmt_exprs, visit_calls};

pub(in crate::ast) fn validate<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    processes: &[InitialProcess],
    directions: &lifetimes::Directions,
    names: &HashMap<String, Vec<String>>,
    nets: &HashSet<&str>,
) -> Result<(), AnalyzerError> {
    let formal = |name: &str, position: usize| {
        names
            .get(name)
            .and_then(|names| names.get(position))
            .cloned()
            .unwrap_or_else(|| (position + 1).to_string())
    };
    let invalid =
        |subroutine: &str, argument: &str, detail: &str| AnalyzerError::InvalidRefArgument {
            subroutine: subroutine.to_owned(),
            argument: argument.to_owned(),
            detail: detail.to_owned(),
        };
    let direction = |name: &str, position: usize| {
        directions
            .get(name)
            .and_then(|directions| directions.get(position))
            .copied()
    };
    // A net is never passed by reference (13.5.2).
    let check_call = |name: &str, args: &[Option<Expr>]| -> Result<(), AnalyzerError> {
        for (position, arg) in args.iter().enumerate() {
            if let Some(actual) = arg.as_ref().and_then(root_variable)
                && direction(name, position).is_some_and(ParamDirection::is_reference)
                && nets.contains(actual)
            {
                return Err(invalid(
                    name,
                    &formal(name, position),
                    "a net cannot be passed by reference (IEEE 1800-2023 13.5.2)",
                ));
            }
        }
        Ok(())
    };
    let check_exprs = |exprs: &mut dyn Iterator<Item = &Expr>| -> Result<(), AnalyzerError> {
        let mut result = Ok(());
        for expr in exprs {
            visit_calls(expr, &mut |callee, args| {
                if let Callee::Subroutine(name) = callee
                    && result.is_ok()
                {
                    result = check_call(name, args);
                }
            });
        }
        result
    };
    let check_body = |body: &[Stmt]| -> Result<(), AnalyzerError> {
        let mut result = Ok(());
        for stmt in body {
            stmt.walk(&mut |stmt| {
                if result.is_err() {
                    return;
                }
                if let Stmt::Call { name, args, .. } = stmt {
                    result = check_call(name, args);
                }
                if result.is_ok() {
                    result = check_exprs(&mut stmt_exprs(stmt).into_iter());
                }
            });
        }
        result
    };
    // A function with a ref argument is called only from procedural code,
    // and never in an event expression (13.4.1); const ref is allowed.
    let passes_reference = |expr: &Expr| {
        let mut found = None;
        visit_calls(expr, &mut |callee, args| {
            if let Callee::Subroutine(name) = callee
                && let Some(position) = (0..args.len())
                    .find(|position| direction(name, *position) == Some(ParamDirection::Ref))
            {
                found.get_or_insert_with(|| procedural_only(name, &formal(name, position)));
            }
        });
        found
    };
    for process in comb {
        for assignment in process.assignments() {
            if let Some(error) = passes_reference(assignment.rhs()) {
                return Err(error);
            }
            check_exprs(&mut std::iter::once(assignment.rhs()))?;
        }
        // A continuous assignment to a concatenation is a statement.
        if process.kind() == CombProcessKind::ContinuousAssign {
            for stmt in process.body() {
                let mut found = None;
                stmt.walk(&mut |stmt| {
                    for expr in stmt_exprs(stmt) {
                        if found.is_none() {
                            found = passes_reference(expr);
                        }
                    }
                });
                if let Some(error) = found {
                    return Err(error);
                }
            }
        }
        check_body(process.body())?;
    }
    for process in ff {
        check_body(process.body())?;
    }
    for process in processes {
        check_body(process.body())?;
        let mut found = None;
        for stmt in process.body() {
            stmt.walk(&mut |stmt| {
                if let Stmt::WaitEvent(items) = stmt {
                    for item in items {
                        for expr in std::iter::once(&item.expr).chain(&item.iff) {
                            if found.is_none() {
                                found = passes_reference(expr);
                            }
                        }
                    }
                }
            });
        }
        if let Some(error) = found {
            return Err(error);
        }
    }
    for subroutine in subroutines {
        check_body(&subroutine.body)?;
        for param in subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference())
        {
            // A formal of unpacked structure type is used through its members.
            let formals = if param.leaves.is_empty() {
                std::slice::from_ref(&param.name)
            } else {
                param.leaves.as_slice()
            };
            let mut error = None;
            for formal in formals {
                for stmt in &subroutine.body {
                    check_formal(stmt, formal, param.direction, directions, &mut error);
                }
            }
            if let Some(detail) = error {
                return Err(invalid(&subroutine.name, &param.source_name, detail));
            }
        }
    }
    Ok(())
}

fn procedural_only(name: &str, argument: &str) -> AnalyzerError {
    AnalyzerError::InvalidRefArgument {
        subroutine: name.to_owned(),
        argument: argument.to_owned(),
        detail: "a function with a ref argument can only be called in a procedural statement \
                 outside an event expression (IEEE 1800-2023 13.4.1)"
            .into(),
    }
}

/// The variable that an actual names or selects from.
fn root_variable(actual: &Expr) -> Option<&str> {
    match actual {
        Expr::Ident(name) => Some(name),
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } => root_variable(expr),
        _ => None,
    }
}

/// Whether an actual passed to a writable formal writes variable `name`:
/// the variable itself, a select of it, or a part of a concatenation.
fn writes_through(actual: &Expr, name: &str) -> bool {
    match actual {
        Expr::Ident(ident) => ident == name,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } => writes_through(expr, name),
        Expr::Concat(parts) => parts.iter().any(|part| writes_through(part, name)),
        Expr::Call {
            callee: Callee::Checked { .. },
            args,
            ..
        } => args
            .last()
            .and_then(Option::as_ref)
            .is_some_and(|value| writes_through(value, name)),
        _ => false,
    }
}

/// Record in `error` why statement `stmt` cannot use reference formal
/// `formal` of direction `direction`.
fn check_formal(
    stmt: &Stmt,
    formal: &str,
    direction: ParamDirection,
    directions: &lifetimes::Directions,
    error: &mut Option<&'static str>,
) {
    let constant = direction == ParamDirection::ConstRef;
    let written_by_call = |callee: &Callee, args: &[Option<Expr>]| {
        args.iter().enumerate().any(|(position, arg)| {
            arg.as_ref().is_some_and(|arg| writes_through(arg, formal))
                && match callee {
                    Callee::Subroutine(name) => directions
                        .get(name)
                        .and_then(|directions| directions.get(position))
                        .is_none_or(|direction| direction.is_written()),
                    Callee::System(function) => function.writes_argument(position),
                    Callee::Object(op) => op.writes_argument(position),
                    Callee::String(_) | Callee::Checked { .. } | Callee::Lookup { .. } => false,
                }
        })
    };
    stmt.walk(&mut |stmt| {
        if error.is_some() {
            return;
        }
        let (targets, nonblocking): (Vec<&LValue>, bool) = match stmt {
            Stmt::Assign {
                lhs, nonblocking, ..
            } => (vec![lhs], *nonblocking),
            Stmt::AssignConcat {
                parts, nonblocking, ..
            } => (parts.iter().collect(), *nonblocking),
            _ => (Vec::new(), false),
        };
        if targets.iter().any(|target| target.name() == formal) {
            if nonblocking {
                *error = Some(
                    "a ref argument cannot be written by a nonblocking assignment \
                     (IEEE 1800-2023 6.21, 13.5.2)",
                );
            } else if constant {
                *error = Some("a const ref argument cannot be written (IEEE 1800-2023 13.5.2)");
            }
        }
        if constant && error.is_none() {
            let mut written = match stmt {
                Stmt::Call { name, args, .. } => {
                    written_by_call(&Callee::Subroutine(name.clone()), args)
                }
                Stmt::SystemTask { task, args, .. } => {
                    args.iter().enumerate().any(|(position, arg)| {
                        task.writes_argument(position)
                            && matches!(arg, crate::procedural::SystemTaskArg::Expr(arg)
                                if writes_through(arg, formal))
                    })
                }
                _ => false,
            };
            for expr in stmt_exprs(stmt) {
                visit_calls(expr, &mut |callee, args| {
                    written |= written_by_call(callee, args);
                });
            }
            if written {
                *error = Some("a const ref argument cannot be written (IEEE 1800-2023 13.5.2)");
            }
        }
        // Processes a fork-join_any or fork-join_none spawns may outlive
        // the call (9.3.2).
        if let Stmt::Fork { join, branches } = stmt
            && *join != celox_design::ForkJoin::All
        {
            let mut used = false;
            for branch in branches {
                for stmt in branch {
                    stmt.walk(&mut |stmt| {
                        // Variables of the fork may be initialized from it.
                        if matches!(stmt, Stmt::Local { .. }) {
                            return;
                        }
                        used |= stmt_exprs(stmt).iter().any(|expr| expr_reads(expr, formal))
                            || match stmt {
                                Stmt::Assign { lhs, .. } => lhs.name() == formal,
                                Stmt::AssignConcat { parts, .. } => {
                                    parts.iter().any(|part| part.name() == formal)
                                }
                                _ => false,
                            };
                    });
                }
            }
            if used {
                *error = Some(
                    "a ref argument cannot be used in a fork-join_any or fork-join_none block \
                     (IEEE 1800-2023 9.3.2)",
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::{AnalyzerError, analyze_source};
    use std::path::Path;

    fn invalid(body: &str) -> String {
        let source = format!("module Top; {body} endmodule");
        match analyze_source(&source, Path::new("ref_arguments.sv")) {
            Err(error @ AnalyzerError::InvalidRefArgument { .. }) => error.to_string(),
            other => panic!("{body}: expected an invalid ref argument, got {other:?}"),
        }
    }

    #[test]
    fn reference_formals_need_automatic_subroutines() {
        let error = invalid("int g; function void f(ref int x); x = 1; endfunction initial f(g);");
        assert!(error.contains("static lifetime"), "{error}");
    }

    #[test]
    fn only_variables_are_passed_by_reference() {
        for (declarations, actual) in [
            ("int g;", "g + 1"),
            ("int g;", "3"),
            ("logic [31:0] g;", "g[7:0]"),
            ("parameter int P = 1;", "P"),
        ] {
            let error = invalid(&format!(
                "{declarations} function automatic void f(ref int x); x = 1; endfunction \
                 initial f({actual});"
            ));
            assert!(error.contains("13.5.2"), "{actual}: {error}");
        }
        let error = invalid(
            "wire [31:0] w; function automatic void f(ref logic [31:0] x); $display(x); \
             endfunction initial f(w);",
        );
        assert!(error.contains("net"), "{error}");
        let error = invalid(
            "wire [31:0] w[2]; function automatic void f(ref logic [31:0] x); x = 1; \
             endfunction initial f(w[0]);",
        );
        assert!(error.contains("net"), "{error}");
    }

    #[test]
    fn actuals_have_equivalent_types() {
        for declaration in [
            "logic [31:0] g;",
            "shortint g;",
            "int unsigned g;",
            "typedef enum {A, B} e_t; e_t g;",
            "int g[$];",
        ] {
            let error = invalid(&format!(
                "{declaration} function automatic void f(ref int x); x = 1; endfunction \
                 initial f(g);"
            ));
            assert!(error.contains("6.22.2"), "{declaration}: {error}");
        }
        let error = invalid(
            "class A; endclass class B extends A; endclass \
             function automatic void f(ref A x); x = null; endfunction \
             initial begin automatic B b; f(b); end",
        );
        assert!(error.contains("6.22.2"), "{error}");
        for declaration in ["bit signed [31:0] g;", "int g;"] {
            analyze_source(
                &format!(
                    "module Top; {declaration} function automatic void f(ref int x); x = 1; \
                     endfunction initial f(g); endmodule"
                ),
                Path::new("ref_equivalent.sv"),
            )
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        }
    }

    #[test]
    fn queues_of_different_bounds_are_not_bound_by_reference() {
        let result = analyze_source(
            "module Top; int q[$:1]; function automatic void f(ref int x[$]); \
             x.push_back(3); endfunction initial f(q); endmodule",
            Path::new("ref_queue_bounds.sv"),
        );
        assert!(
            matches!(&result, Err(AnalyzerError::Unsupported(construct)) if construct.contains("different bounds")),
            "{result:?}"
        );
    }

    #[test]
    fn const_ref_formals_are_read_only() {
        for body in ["x = 1;", "x++;", "h(x);", "{x, y} = 64'd1;"] {
            let error = invalid(&format!(
                "int g, y; function automatic void h(ref int z); z = 2; endfunction \
                 function automatic void f(const ref int x); {body} endfunction initial f(g);"
            ));
            assert!(error.contains("const ref"), "{body}: {error}");
        }
        let error = invalid(
            "int a[2]; function automatic void set(ref int x); x = 1; endfunction \
             function automatic void f(const ref int v[2]); set(v[0]); endfunction \
             initial f(a);",
        );
        assert!(error.contains("const ref"), "{error}");
        let error = invalid(
            "int a[2]; task automatic load(const ref int v[2]); $readmemh(\"data.hex\", v); \
             endtask initial load(a);",
        );
        assert!(error.contains("const ref"), "{error}");
        let error = invalid(
            "int q[$]; function automatic void f(const ref int x[$]); x.push_back(1); \
             endfunction initial f(q);",
        );
        assert!(error.contains("const ref"), "{error}");
        // `const` is part of the direction the next formal inherits (13.3).
        let error = invalid(
            "int g, h; function automatic void f(const ref int a, b); b = 1; endfunction \
             initial f(g, h);",
        );
        assert!(error.contains("`b`"), "{error}");
    }

    #[test]
    fn reference_formals_follow_automatic_variable_rules() {
        let error = invalid("int g; task automatic f(ref int x); x <= 1; endtask initial f(g);");
        assert!(error.contains("nonblocking"), "{error}");
        for join in ["join_none", "join_any"] {
            let error = invalid(&format!(
                "int g; task automatic f(ref int x); fork x = 1; {join} endtask initial f(g);"
            ));
            assert!(error.contains("9.3.2"), "{join}: {error}");
        }
        analyze_source(
            "module Top; int g; task automatic f(ref int x); \
             fork automatic int k = x; $display(k); join_none \
             fork x = 2; join endtask initial f(g); endmodule",
            Path::new("ref_fork_initializer.sv"),
        )
        .unwrap();
    }

    #[test]
    fn functions_with_ref_arguments_are_called_procedurally() {
        let error = invalid(
            "int g, r; function automatic int f(ref int x); return x; endfunction \
             assign r = f(g);",
        );
        assert!(error.contains("13.4.1"), "{error}");
        let error = invalid(
            "int g; logic [15:0] a, b; function automatic int f(ref int x); return x; \
             endfunction assign {a, b} = f(g);",
        );
        assert!(error.contains("13.4.1"), "{error}");
        analyze_source(
            "module Top; int g, r; function automatic int f(const ref int x); return x; \
             endfunction assign r = f(g); endmodule",
            Path::new("const_ref_continuous.sv"),
        )
        .unwrap();
    }
}
