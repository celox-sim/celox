//! Built-in class randomization methods (IEEE 1800-2023 18.6, 18.8, 18.11).
use super::*;
use crate::system_functions::ObjectOp;

/// Generate automatic methods so hooks, dispatch and property updates use
/// ordinary call machinery. The hidden selection argument is zero for the
/// persistent random modes, or has bit 0 set and bit i+1 for selected property i.
/// Inherited properties keep their indices, including hidden base properties.
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
    for method in ["randomize", "srandom", "rand_mode"] {
        if method == "randomize" && class.check_randomize(tree).is_err() {
            continue;
        }
        let mut builder =
            BodyBuilder::new(tree, dimensions, state, system_functions::Body::Subroutine);
        builder.automatic = true;
        let first_local = builder.state.locals.len();
        let this = builder.declare(classes::THIS, handle.clone());
        let (argument, argument_type) = match method {
            "randomize" => ("$selection", class.selection_type()),
            "srandom" => ("seed", int_type()),
            _ => ("on_off", classes::unsigned_type(1)),
        };
        let argument = builder.declare(argument, argument_type.clone());
        let params = vec![
            SubroutineParam {
                name: this.clone(),
                source_name: classes::THIS.into(),
                direction: ParamDirection::Input,
                r#type: convert_type(handle.clone()),
                default: None,
            },
            SubroutineParam {
                name: argument.clone(),
                source_name: argument.clone(),
                direction: ParamDirection::Input,
                r#type: convert_type(argument_type),
                default: None,
            },
        ];
        // Faults are consumed after the kernel completes. Return immediately
        // on rejection, before hooks, draws or property/mode updates.
        let mut body = vec![Stmt::If {
            condition: classes::object_call(
                ObjectOp::Check(if method == "srandom" { 0 } else { class.id }),
                vec![Expr::Ident(this.clone())],
            ),
            then_body: Vec::new(),
            else_body: vec![Stmt::Return(
                (method == "randomize").then(|| Expr::Literal("32'sd0".into())),
            )],
        }];
        let return_var = match method {
            "srandom" => {
                body.push(Stmt::Eval(classes::object_call(
                    ObjectOp::Seed,
                    vec![Expr::Ident(this.clone()), Expr::Ident(argument.clone())],
                )));
                None
            }
            "rand_mode" => {
                for property in &class.properties {
                    let update = Stmt::Eval(classes::object_call(
                        ObjectOp::ModeSet(property.field.offset),
                        vec![Expr::Ident(this.clone()), Expr::Ident(argument.clone())],
                    ));
                    body.push(if property.rand || property.randc {
                        update
                    } else {
                        Stmt::If {
                            condition: classes::object_call(
                                ObjectOp::ModeSelected(property.field.offset),
                                vec![Expr::Ident(this.clone())],
                            ),
                            then_body: vec![update],
                            else_body: Vec::new(),
                        }
                    });
                }
                None
            }
            _ => {
                let hook = |name| {
                    class.method(name).map(|method| Stmt::Call {
                        name: classes::Class::subroutine_name(&method.owner, &method.name),
                        args: vec![Some(Expr::Ident(this.clone()))],
                        arg_order: Vec::new(),
                    })
                };
                let mut select = vec![Stmt::Eval(classes::object_call(
                    ObjectOp::ModePush,
                    vec![Expr::Ident(this.clone())],
                ))];
                for (index, property) in class.properties.iter().enumerate() {
                    let selected = Expr::Binary {
                        left: Box::new(Expr::Binary {
                            left: Box::new(Expr::Ident(argument.clone())),
                            op: BinaryOp::Shr,
                            right: Box::new(Expr::Literal((index + 1).to_string())),
                        }),
                        op: BinaryOp::BitAnd,
                        right: Box::new(Expr::Literal("1'b1".into())),
                    };
                    select.push(Stmt::Eval(classes::object_call(
                        ObjectOp::ModeSelect(property.field.offset, property.rand),
                        vec![Expr::Ident(this.clone()), selected],
                    )));
                }
                body.push(Stmt::If {
                    condition: Expr::Ident(argument.clone()),
                    then_body: select,
                    else_body: Vec::new(),
                });
                body.extend(hook("pre_randomize"));
                for property in &class.properties {
                    if property.constant
                        || property.field.handle.is_some()
                        || (property.r#type.is_enum && property.r#type.enum_values.is_none())
                    {
                        continue;
                    }
                    body.push(Stmt::If {
                        condition: Expr::Binary {
                            left: Box::new(Expr::Binary {
                                left: Box::new(Expr::Ident(argument.clone())),
                                op: BinaryOp::Ne,
                                right: Box::new(Expr::Literal("1".into())),
                            }),
                            op: BinaryOp::LogicAnd,
                            right: Box::new(classes::object_call(
                                ObjectOp::ModeGet(property.field.offset, property.rand),
                                vec![Expr::Ident(this.clone())],
                            )),
                        },
                        then_body: random_property(&mut builder, property, &this),
                        else_body: Vec::new(),
                    });
                }
                body.extend(hook("post_randomize"));
                body.push(Stmt::If {
                    condition: Expr::Ident(argument.clone()),
                    then_body: vec![Stmt::Eval(classes::object_call(
                        ObjectOp::ModePop,
                        vec![Expr::Ident(this.clone())],
                    ))],
                    else_body: Vec::new(),
                });
                let returned = builder.declare("randomize", int_type());
                body.push(Stmt::Assign {
                    lhs: LValue::Ident(returned.clone()),
                    rhs: Expr::Literal("32'sd1".into()),
                    nonblocking: false,
                });
                Some(returned)
            }
        };
        if method != "srandom" {
            // randomize is virtual, while its pre/post hooks are not (18.6.2).
            // Object-wide rand_mode also covers the dynamic object's members.
            let mut items = Vec::new();
            let aliases = item.type_aliases(&dimensions.type_aliases);
            for declaration in &tree.declarations().classes {
                if declaration.interface || declaration.virtual_class {
                    continue;
                }
                let name = instances::normalize_identifier(&declaration.name.spelling);
                if name == class.name {
                    continue;
                }
                let Ok(derived) = classes::class(tree, &name, &item.env, &aliases) else {
                    continue;
                };
                if (method == "randomize" && derived.check_randomize(tree).is_err())
                    || !derived.derives_from(tree, &class.name, &item.env, &aliases)
                {
                    continue;
                }
                let args = vec![
                    Some(Expr::Ident(this.clone())),
                    Some(Expr::Ident(argument.clone())),
                ];
                let name = classes::Class::subroutine_name(&name, method);
                let call = if method == "randomize" {
                    vec![Stmt::Return(Some(Expr::Call {
                        callee: Callee::Subroutine(name),
                        args,
                        arg_order: Vec::new(),
                    }))]
                } else {
                    vec![
                        Stmt::Call {
                            name,
                            args,
                            arg_order: Vec::new(),
                        },
                        Stmt::Return(None),
                    ]
                };
                items.push(CaseItemBase {
                    labels: vec![CaseLabel::Value(Expr::Literal(format!(
                        "32'd{}",
                        derived.id
                    )))],
                    body: call,
                });
            }
            body.insert(
                0,
                Stmt::Case {
                    kind: CaseKind::Exact,
                    selector: classes::object_call(
                        ObjectOp::ClassOf,
                        vec![Expr::Ident(this.clone())],
                    ),
                    items,
                    default: None,
                },
            );
        }
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

/// Select a declared enum value with rejection sampling, so non-power-of-two
/// domains are uniform (IEEE 1800-2023 18.3, 18.4.1). Keep the draws inside the
/// mode guard: a disabled property must not consume its object's random stream.
fn random_property(
    builder: &mut BodyBuilder<'_, '_, '_>,
    property: &classes::Property,
    this: &str,
) -> Vec<Stmt> {
    let random = || classes::object_call(ObjectOp::Random, vec![Expr::Ident(this.into())]);
    let mut body = Vec::new();
    let value = if let Some(values) = &property.r#type.enum_values {
        let count = values.len() as u64;
        let threshold = count.wrapping_neg() % count;
        let draw = builder.declare("$enum_draw", classes::unsigned_type(64));
        body.push(Stmt::Loop {
            kind: LoopKind::DoWhile,
            init: Vec::new(),
            condition: Some(Expr::Binary {
                left: Box::new(Expr::Ident(draw.clone())),
                op: BinaryOp::Lt,
                right: Box::new(Expr::Literal(format!("64'd{threshold}"))),
            }),
            step: Vec::new(),
            body: vec![Stmt::Assign {
                lhs: LValue::Ident(draw.clone()),
                rhs: random(),
                nonblocking: false,
            }],
        });
        body.push(Stmt::Assign {
            lhs: LValue::Ident(draw.clone()),
            rhs: Expr::Binary {
                left: Box::new(Expr::Ident(draw.clone())),
                op: BinaryOp::Mod,
                right: Box::new(Expr::Literal(format!("64'd{count}"))),
            },
            nonblocking: false,
        });
        let literal = |value| {
            Expr::Literal(format_typed_parameter_literal(
                value,
                property.field.width as usize,
                property.field.signed,
            ))
        };
        let mut value = literal(*values.last().unwrap());
        for (index, &member) in values.iter().enumerate().rev().skip(1) {
            value = Expr::Mux {
                condition: Box::new(Expr::Binary {
                    left: Box::new(Expr::Ident(draw.clone())),
                    op: BinaryOp::Eq,
                    right: Box::new(Expr::Literal(format!("64'd{index}"))),
                }),
                then_expr: Box::new(literal(member)),
                else_expr: Box::new(value),
            };
        }
        value
    } else {
        random()
    };
    body.push(Stmt::Eval(classes::object_call(
        ObjectOp::Store(property.field),
        vec![Expr::Ident(this.into()), value],
    )));
    body
}
