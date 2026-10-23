//! Built-in class randomization methods (IEEE 1800-2023 18.6, 18.13.3).
use super::*;
use crate::system_functions::ObjectOp;

/// Generate ordinary automatic subroutines so hooks and property updates use
/// the same call machinery as authored class methods. Unsupported randomization
/// is diagnosed at the call site, rather than rejecting an unused class.
pub(super) fn lower(
    class: &classes::Class,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    state: &mut BodyState<'_>,
    item: &generate::Item,
) -> Converted<Vec<Subroutine>> {
    let handle = classes::handle_type(tree, &class.name)
        .ok_or_else(|| unsupported("class randomization receiver"))?;
    let convert_type = |ty| {
        crate::ir::Type::from_ast(
            ty,
            &const_functions::Environment {
                constants: &*item.env,
                functions: &tree.state.const_functions,
            },
        )
    };
    let mut result = Vec::new();
    for method in ["randomize", "srandom"] {
        if method == "randomize" && class.check_randomize(tree).is_err() {
            continue;
        }
        let mut builder =
            BodyBuilder::new(tree, dimensions, state, system_functions::Body::Subroutine);
        builder.automatic = true;
        let first_local = builder.state.locals.len();
        let this = builder.declare(classes::THIS, handle.clone());
        let mut params = vec![SubroutineParam {
            name: this.clone(),
            source_name: classes::THIS.into(),
            direction: ParamDirection::Input,
            r#type: convert_type(handle.clone()),
            default: None,
        }];
        // Heap faults are reported after the kernel completes. Return now so
        // a rejected receiver cannot run hooks or mutate the object's state.
        let mut body = vec![Stmt::If {
            condition: classes::object_call(
                ObjectOp::Check(if method == "randomize" { class.id } else { 0 }),
                vec![Expr::Ident(this.clone())],
            ),
            then_body: Vec::new(),
            else_body: vec![Stmt::Return(
                (method == "randomize").then(|| Expr::Literal("32'sd0".into())),
            )],
        }];
        let return_var = if method == "srandom" {
            let seed = builder.declare("seed", int_type());
            params.push(SubroutineParam {
                name: seed.clone(),
                source_name: "seed".into(),
                direction: ParamDirection::Input,
                r#type: convert_type(int_type()),
                default: None,
            });
            body.push(Stmt::Eval(classes::object_call(
                ObjectOp::Seed,
                vec![Expr::Ident(this.clone()), Expr::Ident(seed)],
            )));
            None
        } else {
            let hook = |name| {
                class.method(name).map(|method| Stmt::Call {
                    name: classes::Class::subroutine_name(&method.owner, &method.name),
                    args: vec![Some(Expr::Ident(this.clone()))],
                    arg_order: Vec::new(),
                })
            };
            body.extend(hook("pre_randomize"));
            for property in class.properties.iter().filter(|property| property.rand) {
                body.push(Stmt::Eval(classes::object_call(
                    ObjectOp::Store(property.field),
                    vec![
                        Expr::Ident(this.clone()),
                        classes::object_call(ObjectOp::Random, vec![Expr::Ident(this.clone())]),
                    ],
                )));
            }
            body.extend(hook("post_randomize"));
            let returned = builder.declare("randomize", int_type());
            body.push(Stmt::Assign {
                lhs: LValue::Ident(returned.clone()),
                rhs: Expr::Literal("32'sd1".into()),
                nonblocking: false,
            });
            Some(returned)
        };
        for stmt in &mut body {
            qualify_stmt(item, stmt);
        }
        result.push(Subroutine {
            name: item.name(&classes::Class::subroutine_name(&class.name, method)),
            is_task: false,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: return_var.as_ref().map(|_| convert_type(int_type())),
            params,
            return_var,
            body,
            sensitivity_body: Vec::new(),
        });
    }
    Ok(result)
}
