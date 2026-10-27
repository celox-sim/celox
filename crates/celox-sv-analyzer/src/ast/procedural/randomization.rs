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
    lower_methods(class, tree, dimensions, state, Some(item), None, false)
}

fn lower_methods(
    class: &classes::Class,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    state: &mut BodyState<'_>,
    item: Option<&generate::Item>,
    inline: Option<&classes::constraints::Inline>,
    prepare: bool,
) -> Converted<Vec<Subroutine>> {
    let env = item.map(|item| &*item.env).unwrap_or(&dimensions.const_env);
    let aliases = item
        .map(|item| item.type_aliases(&dimensions.type_aliases))
        .unwrap_or_else(|| std::borrow::Cow::Borrowed(&dimensions.type_aliases));
    let method_name = |class: &str, method: &str| {
        if let Some(inline) = inline {
            if prepare {
                inline.prepare_name(class)
            } else {
                inline.name(class)
            }
        } else {
            let name = classes::Class::subroutine_name(class, method);
            item.map(|item| item.name(&name)).unwrap_or(name)
        }
    };
    let handle = classes::handle_type(tree, &class.name)
        .ok_or_else(|| unsupported("class randomization receiver"))?;
    let convert_type = |ty| {
        crate::ir::Type::from_ast(
            ty,
            &const_functions::Environment {
                constants: env,
                functions: &tree.state.const_functions,
            },
        )
    };
    let mut result = Vec::new();
    for method in ["randomize", "srandom", "rand_mode"] {
        if inline.is_some() && method != "randomize" {
            continue;
        }
        if method == "randomize" && class.check_randomize().is_err() {
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
        let mut params = vec![
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
        let mut input_values = Vec::new();
        if let Some(inline) = inline.filter(|_| !prepare) {
            for (index, ty) in inline.inputs.iter().enumerate() {
                let name = builder.declare(&format!("$input{index}"), ty.clone());
                params.push(SubroutineParam {
                    name: name.clone(),
                    source_name: name.clone(),
                    direction: ParamDirection::Input,
                    r#type: convert_type(ty.clone()),
                    default: None,
                });
                input_values.push(Expr::Ident(name));
            }
        }
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
                        ObjectOp::ModeSelect(
                            property.field.offset,
                            property.rand || property.randc,
                        ),
                        vec![Expr::Ident(this.clone()), selected],
                    )));
                }
                if prepare || inline.is_none_or(|inline| inline.inputs.is_empty()) {
                    body.push(Stmt::If {
                        condition: Expr::Ident(argument.clone()),
                        then_body: select,
                        else_body: Vec::new(),
                    });
                    body.extend(hook("pre_randomize"));
                }
                if prepare {
                    body.push(Stmt::Return(Some(Expr::Ident(this.clone()))));
                    Some(builder.declare("$prepared", handle.clone()))
                } else {
                    if class.has_constraints || inline.is_some() {
                        let mut program = class.constraint_program()?;
                        if let Some(inline) = inline {
                            program.constraints.extend(inline.constraints.clone());
                        }
                        let success = builder.declare("$constraint_success", int_type());
                        body.push(Stmt::Assign {
                            lhs: LValue::Ident(success.clone()),
                            rhs: classes::object_call(
                                ObjectOp::Randomize(program),
                                [
                                    vec![
                                        Expr::Ident(this.clone()),
                                        Expr::Binary {
                                            left: Box::new(Expr::Ident(argument.clone())),
                                            op: BinaryOp::Eq,
                                            right: Box::new(Expr::Literal("1".into())),
                                        },
                                    ],
                                    input_values.clone(),
                                ]
                                .concat(),
                            ),
                            nonblocking: false,
                        });
                        body.push(Stmt::If {
                            condition: Expr::Ident(success),
                            then_body: Vec::new(),
                            else_body: vec![
                                Stmt::If {
                                    condition: Expr::Ident(argument.clone()),
                                    then_body: vec![Stmt::Eval(classes::object_call(
                                        ObjectOp::ModePop,
                                        vec![Expr::Ident(this.clone())],
                                    ))],
                                    else_body: Vec::new(),
                                },
                                Stmt::Return(Some(Expr::Literal("32'sd0".into()))),
                            ],
                        });
                    } else {
                        for property in &class.properties {
                            if property.constant
                                || property.field.handle.is_some()
                                || (property.r#type.is_enum
                                    && property.r#type.enum_values.is_none())
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
                                        ObjectOp::ModeGet(
                                            property.field.offset,
                                            property.rand || property.randc,
                                        ),
                                        vec![Expr::Ident(this.clone())],
                                    )),
                                },
                                then_body: random_property(&mut builder, property, &this),
                                else_body: Vec::new(),
                            });
                        }
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
            }
        };
        if method != "srandom" {
            // randomize is virtual, while its pre/post hooks are not (18.6.2).
            // Object-wide rand_mode also covers the dynamic object's members.
            let mut items = Vec::new();
            for name in classes::visible_classes(tree) {
                if name == class.name {
                    continue;
                }
                let Ok(derived) = classes::class(tree, &name, env, &aliases) else {
                    continue;
                };
                if derived.is_abstract || derived.is_interface {
                    continue;
                }
                if (inline.is_some() && derived.constraint_program().is_err())
                    || (method == "randomize" && derived.check_randomize().is_err())
                    || !derived.derives_from(tree, &class.name, env, &aliases)
                {
                    continue;
                }
                let mut args = vec![
                    Some(Expr::Ident(this.clone())),
                    Some(Expr::Ident(argument.clone())),
                ];
                args.extend(input_values.iter().cloned().map(Some));
                let name = method_name(&name, method);
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
        if let Some(item) = item {
            for stmt in &mut body {
                qualify_stmt(item, stmt);
            }
        }
        result.push(Subroutine {
            name: method_name(&class.name, method),
            is_task: false,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: return_var
                .as_ref()
                .map(|_| convert_type(if prepare { handle.clone() } else { int_type() })),
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
    let random = || {
        let op = if property.randc {
            ObjectOp::RandomCyclic {
                offset: property.field.offset,
                count: property
                    .cyclic_domain_size()
                    .expect("validated randc domain"),
            }
        } else {
            ObjectOp::Random
        };
        classes::object_call(op, vec![Expr::Ident(this.into())])
    };
    let mut body = Vec::new();
    let value = if let Some(values) = &property.r#type.enum_values {
        let count = values.len() as u64;
        let threshold = count.wrapping_neg() % count;
        let draw = builder.declare("$enum_draw", classes::unsigned_type(64));
        if property.randc {
            body.push(Stmt::Assign {
                lhs: LValue::Ident(draw.clone()),
                rhs: random(),
                nonblocking: false,
            });
        } else {
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
        }
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

pub(in crate::ast) fn inline_subroutines(
    tree: &AnalysisContext<'_>,
    state: &mut BodyState<'_>,
    dimensions: &PackedDimensions,
) -> Converted<(Vec<Subroutine>, Vec<classes::constraints::InlineRequest>)> {
    let mut requests = std::mem::take(&mut *tree.state.inline_randomizations.borrow_mut());
    for request in &scope::imported(&tree.state).inline_randomizations {
        if !requests.iter().any(|known| known.key == request.key) {
            requests.push(classes::constraints::Inline {
                key: request.key,
                class: std::rc::Rc::new(request.class.clone()),
                constraints: request.constraints.clone(),
                inputs: request.inputs.clone(),
                dimensions: dimensions.clone(),
            });
        }
    }
    let exported = requests
        .iter()
        .map(classes::constraints::Inline::request)
        .collect();
    let mut result = Vec::new();
    for inline in requests {
        let dims = &inline.dimensions;
        let mut classes = vec![inline.class.clone()];
        for name in classes::visible_classes(tree) {
            if name == inline.class.name {
                continue;
            }
            let Ok(class) = classes::class(tree, &name, &dims.const_env, &dims.type_aliases) else {
                continue;
            };
            if !class.is_abstract
                && !class.is_interface
                && class.check_randomize().is_ok()
                && class.constraint_program().is_ok()
                && class.derives_from(
                    tree,
                    &inline.class.name,
                    &dims.const_env,
                    &dims.type_aliases,
                )
            {
                classes.push(class);
            }
        }
        for class in classes {
            result.extend(lower_methods(
                &class,
                tree,
                dims,
                state,
                None,
                Some(&inline),
                false,
            )?);
            if !inline.inputs.is_empty() {
                result.extend(lower_methods(
                    &class,
                    tree,
                    dims,
                    state,
                    None,
                    Some(&inline),
                    true,
                )?);
            }
        }
    }
    Ok((result, exported))
}
