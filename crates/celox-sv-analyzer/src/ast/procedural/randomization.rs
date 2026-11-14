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
    session: &mut Session,
    dimensions: &PackedDimensions,
    state: &mut BodyState<'_>,
    item: &generate::Item,
) -> Converted<Vec<Subroutine>> {
    lower_methods(
        class,
        tree,
        session,
        dimensions,
        state,
        Some(item),
        None,
        false,
    )
}

fn lower_methods(
    class: &classes::Class,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
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
    let handle = classes::handle_type(tree, session, &class.name)
        .ok_or_else(|| unsupported("class randomization receiver"))?;
    let convert_type =
        |session: &mut Session, ty| crate::ast::ir_type(session, &tree.functions, ty, env);
    // The types the methods declare are bound to their semantic types where
    // they are made, as a declaration's are.
    let bound = |session: &mut Session, ty| {
        semantic_types::bound_builtin(&mut session.types, &tree.functions, ty)
    };
    let int = bound(session, int_type());
    let string = bound(session, string_type());
    let mut result = Vec::new();
    for method in RANDOMIZATION_METHODS {
        if inline.is_some() && method != "randomize" {
            continue;
        }
        if method == "randomize" && class.check_randomize().is_err() {
            continue;
        }
        let mut builder = BodyBuilder::new(
            tree,
            session,
            dimensions,
            state,
            system_functions::Body::Subroutine,
        );
        builder.automatic = true;
        builder.set_owner(method_name(&class.name, method));
        let first_local = builder.state.locals.len();
        let this = builder.declare(classes::THIS, handle.clone());
        let mut params = vec![SubroutineParam {
            name: this.clone(),
            source_name: classes::THIS.into(),
            direction: ParamDirection::Input,
            r#type: convert_type(builder.session, handle.clone()),
            default: None,
            leaves: Vec::new(),
        }];
        // `get_randstate` has no argument but the object.
        let argument = match randomization_method_argument(method, || class.selection_type()) {
            Some((argument, argument_type)) => {
                let argument_type = bound(builder.session, argument_type);
                let argument = builder.declare(argument, argument_type.clone());
                params.push(SubroutineParam {
                    name: argument.clone(),
                    source_name: argument.clone(),
                    direction: ParamDirection::Input,
                    r#type: convert_type(builder.session, argument_type),
                    default: None,
                    leaves: Vec::new(),
                });
                argument
            }
            None => String::new(),
        };
        let mut input_values = Vec::new();
        if let Some(inline) = inline.filter(|_| !prepare) {
            for (index, ty) in inline.inputs.iter().enumerate() {
                let name = builder.declare(&format!("$input{index}"), ty.clone());
                params.push(SubroutineParam {
                    name: name.clone(),
                    source_name: name.clone(),
                    direction: ParamDirection::Input,
                    r#type: convert_type(builder.session, ty.clone()),
                    default: None,
                    leaves: Vec::new(),
                });
                input_values.push(Expr::Ident(name));
            }
        }
        // Faults are consumed after the kernel completes. Return immediately
        // on rejection, before hooks, draws or property/mode updates.
        let mut body = vec![Stmt::If {
            condition: classes::object_call(
                ObjectOp::Check(if matches!(method, "randomize" | "rand_mode") {
                    class.id
                } else {
                    0
                }),
                vec![Expr::Ident(this.clone())],
            ),
            then_body: Vec::new(),
            else_body: vec![Stmt::Return(
                (method == "randomize").then(|| Expr::Literal("32'sd0".into())),
            )],
        }];
        let return_var = match method {
            // The state of the object's RNG (IEEE 1800-2023 18.13.4, 18.13.5).
            "get_randstate" => {
                let state = builder.declare("get_randstate", string.clone());
                body.push(Stmt::Assign {
                    lhs: LValue::Ident(state.clone()),
                    rhs: classes::object_call(
                        ObjectOp::GetRandstate,
                        vec![Expr::Ident(this.clone())],
                    ),
                    nonblocking: false,
                });
                Some(state)
            }
            "set_randstate" => {
                body.push(Stmt::Eval(classes::object_call(
                    ObjectOp::SetRandstate,
                    vec![Expr::Ident(this.clone()), Expr::Ident(argument.clone())],
                )));
                None
            }
            "srandom" => {
                body.push(Stmt::Eval(classes::object_call(
                    ObjectOp::Seed,
                    vec![Expr::Ident(this.clone()), Expr::Ident(argument.clone())],
                )));
                None
            }
            "rand_mode" => {
                // That of a static rand property is the bit every object
                // shares (IEEE 1800-2023 18.8).
                for slot in &class.static_slots {
                    body.push(Stmt::Assign {
                        lhs: LValue::Ident(slot.mode.clone()),
                        rhs: Expr::Unary {
                            op: UnaryOp::LogicNot,
                            expr: Box::new(Expr::Ident(argument.clone())),
                        },
                        nonblocking: false,
                    });
                }
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
                // The hooks of the object, and with rand object handles
                // those of each enabled object they reach, by its class
                // (18.6.2), which the heap finds. A pre_randomize may change
                // them, so each object's handles are read once its hook ran,
                // at every depth, each object visited once; the solve takes
                // the objects visited. post_randomize runs on the objects the
                // solve took, begun right after it, before any hook can
                // change a handle.
                let visits = if class.has_rand_objects() {
                    let members =
                        classes::rand_member_classes(tree, builder.session, class, env, &aliases)?;
                    let targets = ["pre_randomize", "post_randomize"]
                        .map(|hook| classes::hook_targets(&members, hook));
                    Some((
                        builder.declare("$visits", int.clone()),
                        builder.declare("$visit", int.clone()),
                        builder.declare("$visited", handle.clone()),
                        targets,
                    ))
                } else {
                    None
                };
                let hook = |name: &str| -> Vec<Stmt> {
                    let mut body: Vec<Stmt> = class
                        .method(name)
                        .map(|method| Stmt::Call {
                            name: classes::Class::subroutine_name(&method.owner, &method.name),
                            args: vec![Some(Expr::Ident(this.clone()))],
                            arg_order: Vec::new(),
                        })
                        .into_iter()
                        .collect();
                    let Some((count, index, visited, targets)) = &visits else {
                        return body;
                    };
                    let post = name == "post_randomize";
                    let targets = &targets[usize::from(post)];
                    let items = targets
                        .iter()
                        .map(|(id, target)| CaseItemBase {
                            labels: vec![CaseLabel::Value(Expr::Literal(format!("32'd{id}")))],
                            body: vec![Stmt::Call {
                                name: target.clone(),
                                args: vec![Some(Expr::Ident(visited.clone()))],
                                arg_order: Vec::new(),
                            }],
                        })
                        .collect::<Vec<_>>();
                    let begin = Stmt::Assign {
                        lhs: LValue::Ident(count.clone()),
                        rhs: if post {
                            classes::object_call(ObjectOp::VisitSolved, Vec::new())
                        } else {
                            classes::object_call(
                                ObjectOp::VisitBegin,
                                vec![Expr::Ident(this.clone())],
                            )
                        },
                        nonblocking: false,
                    };
                    if post {
                        body.insert(0, begin);
                    } else {
                        body.push(begin);
                    }
                    body.push(Stmt::Loop {
                        kind: LoopKind::For,
                        init: vec![Stmt::Assign {
                            lhs: LValue::Ident(index.clone()),
                            rhs: Expr::Literal("32'sd0".into()),
                            nonblocking: false,
                        }],
                        condition: Some(Expr::Binary {
                            left: Box::new(Expr::Ident(index.clone())),
                            op: BinaryOp::Lt,
                            right: Box::new(Expr::Ident(count.clone())),
                        }),
                        step: vec![Stmt::Assign {
                            lhs: LValue::Ident(index.clone()),
                            rhs: Expr::Binary {
                                left: Box::new(Expr::Ident(index.clone())),
                                op: BinaryOp::Add,
                                right: Box::new(Expr::Literal("32'sd1".into())),
                            },
                            nonblocking: false,
                        }],
                        body: vec![
                            Stmt::Assign {
                                lhs: LValue::Ident(visited.clone()),
                                rhs: classes::object_call(
                                    ObjectOp::Visit,
                                    vec![Expr::Ident(index.clone())],
                                ),
                                nonblocking: false,
                            },
                            Stmt::Case {
                                kind: CaseKind::Exact,
                                selector: classes::object_call(
                                    ObjectOp::ClassOf,
                                    vec![Expr::Ident(visited.clone())],
                                ),
                                items,
                                default: None,
                            },
                        ]
                        .into_iter()
                        .chain((!post).then(|| Stmt::Assign {
                            lhs: LValue::Ident(count.clone()),
                            rhs: classes::object_call(
                                ObjectOp::VisitExpand,
                                vec![Expr::Ident(visited.clone())],
                            ),
                            nonblocking: false,
                        }))
                        .collect(),
                    });
                    body.push(Stmt::Eval(classes::object_call(
                        ObjectOp::VisitEnd,
                        Vec::new(),
                    )));
                    body
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
                            right: Box::new(Expr::Literal(class.property_bits[index].to_string())),
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
                // The bits of the static rand properties (see
                // `classes::Class::property_bits`).
                for (index, slot) in class.static_slots.iter().enumerate() {
                    let selected = Expr::Binary {
                        left: Box::new(Expr::Binary {
                            left: Box::new(Expr::Ident(argument.clone())),
                            op: BinaryOp::Shr,
                            right: Box::new(Expr::Literal(class.slot_bits[index].to_string())),
                        }),
                        op: BinaryOp::BitAnd,
                        right: Box::new(Expr::Literal("1'b1".into())),
                    };
                    select.push(Stmt::Eval(classes::object_call(
                        ObjectOp::ModeSelect(slot.field.offset, true),
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
                    // A static rand property is solved in the object's slot
                    // for it, seeded from the property and random while the
                    // bit all objects share says so (IEEE 1800-2023 18.4,
                    // 18.8); the solution is written back below.
                    let shadows = static_shadows(class);
                    for (field, variable, off) in &shadows {
                        body.push(Stmt::Eval(classes::object_call(
                            ObjectOp::Store(*field),
                            vec![Expr::Ident(this.clone()), Expr::Ident(variable.clone())],
                        )));
                        // A call with arguments names its random variables
                        // itself, whatever their modes (18.11).
                        body.push(Stmt::If {
                            condition: Expr::Unary {
                                op: UnaryOp::LogicNot,
                                expr: Box::new(Expr::Ident(argument.clone())),
                            },
                            then_body: vec![Stmt::Eval(classes::object_call(
                                ObjectOp::ModeSet(field.offset),
                                vec![
                                    Expr::Ident(this.clone()),
                                    Expr::Unary {
                                        op: UnaryOp::LogicNot,
                                        expr: Box::new(Expr::Ident(off.clone())),
                                    },
                                ],
                            ))],
                            else_body: Vec::new(),
                        });
                    }
                    // The static properties the constraints read, then the
                    // values of the call (see `classes::StaticInput`).
                    let solve_inputs = class
                        .static_inputs
                        .iter()
                        .map(|input| Expr::Ident(input.variable.clone()))
                        .chain(input_values.iter().cloned())
                        .collect::<Vec<_>>();
                    if class.solved() || inline.is_some() {
                        let mut program = class.constraint_program()?;
                        if let Some(inline) = inline {
                            // The inline constraints were bound for the class
                            // of the handle, whose static inputs this class's
                            // start with; the call's values follow this
                            // class's (see `classes::static_inputs`).
                            let first = inline.class.static_inputs.len();
                            let by = class.static_inputs.len() - first;
                            // Its static rand properties are the first of
                            // this class's, whose slots follow the fields
                            // of the properties.
                            let slots = program.fields.len() - class.static_slots.len();
                            program
                                .constraints
                                .extend(inline.constraints.iter().cloned().map(
                                    |mut constraint| {
                                        shift_inputs(&mut constraint, first, by);
                                        classes::constraints::resolve_statics(
                                            &mut constraint,
                                            slots,
                                        );
                                        constraint
                                    },
                                ));
                        }
                        // A call fails, changing nothing, when the
                        // constraints have no solution.
                        let failure = vec![
                            Stmt::If {
                                condition: Expr::Ident(argument.clone()),
                                then_body: vec![Stmt::Eval(classes::object_call(
                                    ObjectOp::ModePop,
                                    vec![Expr::Ident(this.clone())],
                                ))],
                                else_body: Vec::new(),
                            },
                            Stmt::Return(Some(Expr::Literal("32'sd0".into()))),
                        ];
                        let check = Expr::Binary {
                            left: Box::new(Expr::Ident(argument.clone())),
                            op: BinaryOp::Eq,
                            right: Box::new(Expr::Literal("1".into())),
                        };
                        let mut solve_inputs = solve_inputs;
                        // The functions the constraints call (IEEE 1800-2023
                        // 18.5.11): the random variables in their arguments
                        // are solved first, by the constraints that name only
                        // them; then each function is called, as the object's
                        // class calls it, with the values of its arguments,
                        // and its value is an input of the solve of the rest.
                        if !class.constraint_calls.is_empty() {
                            let first = solve_inputs.len();
                            program.constraints = std::mem::take(&mut program.constraints)
                                .into_iter()
                                .map(|constraint| {
                                    classes::constraints::call_results(constraint, first)
                                })
                                .collect();
                            let solved = builder.declare("$arguments_solved", int_type());
                            body.push(Stmt::Assign {
                                lhs: LValue::Ident(solved.clone()),
                                rhs: classes::object_call(
                                    ObjectOp::SolveArguments(program.clone()),
                                    [
                                        vec![Expr::Ident(this.clone()), check.clone()],
                                        solve_inputs.clone(),
                                    ]
                                    .concat(),
                                ),
                                nonblocking: false,
                            });
                            body.push(Stmt::If {
                                condition: Expr::Ident(solved),
                                then_body: Vec::new(),
                                else_body: failure.clone(),
                            });
                            let mut argument_index = 0u32;
                            // A 4-state value passes its known bits, and
                            // after the values, the mask of its X and Z
                            // bits (see `Program::unknown_inputs`).
                            let mut result_masks = Vec::new();
                            for (index, call) in class.constraint_calls.iter().enumerate() {
                                // A block a derived class overrides makes no
                                // call; its value is never read.
                                if !call.live {
                                    solve_inputs.push(Expr::Literal("64'd0".into()));
                                    continue;
                                }
                                // A virtual method runs the override of
                                // the object's class, and another the
                                // method the constraint's class sees (8.20).
                                let owner = if call.is_virtual {
                                    &class
                                        .method(&call.method)
                                        .ok_or_else(|| {
                                            unsupported(format!(
                                                "function `{}` of a constraint",
                                                call.method
                                            ))
                                        })?
                                        .owner
                                } else {
                                    &call.owner
                                };
                                let mut args = vec![Some(Expr::Ident(this.clone()))];
                                for &(_, width, signed) in &call.arguments {
                                    args.push(Some(Expr::Resize {
                                        expr: Box::new(classes::object_call(
                                            ObjectOp::ArgumentValue(argument_index),
                                            Vec::new(),
                                        )),
                                        width: width as usize,
                                        signed,
                                    }));
                                    argument_index += 1;
                                }
                                let mut ty = classes::unsigned_type(call.width as usize);
                                ty.is_signed = call.signed;
                                if call.four_state {
                                    ty.kind = TypeKind::Logic;
                                }
                                let result =
                                    builder.declare(&format!("$constraint_call{index}"), ty);
                                body.push(Stmt::Assign {
                                    lhs: LValue::Ident(result.clone()),
                                    rhs: Expr::Call {
                                        callee: Callee::Subroutine(
                                            classes::Class::subroutine_name(owner, &call.method),
                                        ),
                                        args,
                                        arg_order: Vec::new(),
                                    },
                                    nonblocking: false,
                                });
                                if call.four_state {
                                    let (value, mask) = classes::known_bits(Expr::Ident(result));
                                    result_masks.push((solve_inputs.len(), mask));
                                    solve_inputs.push(value);
                                } else {
                                    solve_inputs.push(Expr::Ident(result));
                                }
                            }
                            for (result, mask) in result_masks {
                                program.unknown_inputs.push((result, solve_inputs.len()));
                                solve_inputs.push(mask);
                            }
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
                                    solve_inputs,
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
                            // An array a call selects (18.11): its elements
                            // are randomized at its size.
                            if classes::constraints::random_array(
                                property,
                                tree,
                                builder.session,
                                env,
                            ) {
                                let program = celox_heap::randomization::Program {
                                    arrays: vec![classes::constraints::array(
                                        property,
                                        tree,
                                        builder.session,
                                        env,
                                    )?],
                                    ..Default::default()
                                };
                                body.push(Stmt::If {
                                    condition: Expr::Binary {
                                        left: Box::new(Expr::Binary {
                                            left: Box::new(Expr::Ident(argument.clone())),
                                            op: BinaryOp::Ne,
                                            right: Box::new(Expr::Literal("1".into())),
                                        }),
                                        op: BinaryOp::LogicAnd,
                                        right: Box::new(classes::object_call(
                                            ObjectOp::ModeGet(property.field.offset, false),
                                            vec![Expr::Ident(this.clone())],
                                        )),
                                    },
                                    then_body: vec![Stmt::Eval(classes::object_call(
                                        ObjectOp::Randomize(program),
                                        vec![
                                            Expr::Ident(this.clone()),
                                            Expr::Literal("1'b0".into()),
                                        ],
                                    ))],
                                    else_body: Vec::new(),
                                });
                                continue;
                            }
                            if property.constant
                                || property.field.handle.is_some()
                                || (property.r#type.is_enum
                                    && property.r#type.enum_domain.is_none())
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
                        // A static rand property no constraint names is
                        // drawn in its slot as an instance one is, at any
                        // width (18.4).
                        for slot in &class.static_slots {
                            body.push(Stmt::If {
                                condition: Expr::Binary {
                                    left: Box::new(Expr::Binary {
                                        left: Box::new(Expr::Ident(argument.clone())),
                                        op: BinaryOp::Ne,
                                        right: Box::new(Expr::Literal("1".into())),
                                    }),
                                    op: BinaryOp::LogicAnd,
                                    right: Box::new(classes::object_call(
                                        ObjectOp::ModeGet(slot.field.offset, true),
                                        vec![Expr::Ident(this.clone())],
                                    )),
                                },
                                then_body: random_field(
                                    &mut builder,
                                    slot.field,
                                    &slot.r#type,
                                    None,
                                    &this,
                                ),
                                else_body: Vec::new(),
                            });
                        }
                    }
                    for (field, variable, _) in &shadows {
                        body.push(Stmt::Assign {
                            lhs: LValue::Ident(variable.clone()),
                            rhs: classes::object_call(
                                ObjectOp::Load(*field),
                                vec![Expr::Ident(this.clone())],
                            ),
                            nonblocking: false,
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
                    let returned = builder.declare("randomize", int.clone());
                    body.push(Stmt::Assign {
                        lhs: LValue::Ident(returned.clone()),
                        rhs: Expr::Literal("32'sd1".into()),
                        nonblocking: false,
                    });
                    Some(returned)
                }
            }
        };
        if matches!(method, "randomize" | "rand_mode") {
            // randomize is virtual, while its pre/post hooks are not (18.6.2).
            // Object-wide rand_mode also covers the dynamic object's members.
            let mut items = Vec::new();
            for name in classes::visible_classes(tree, builder.session) {
                if name == class.name {
                    continue;
                }
                let Ok(derived) = classes::class(tree, builder.session, &name, env, &aliases)
                else {
                    continue;
                };
                if derived.is_abstract || derived.is_interface {
                    continue;
                }
                if (inline.is_some() && derived.check_constraint_program().is_err())
                    || (method == "randomize" && derived.check_randomize().is_err())
                    || !derived.derives_from(tree, builder.session, &class.name, env, &aliases)
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
            automatic: true,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: return_var.as_ref().map(|_| {
                convert_type(
                    builder.session,
                    if prepare {
                        handle.clone()
                    } else if method == "get_randstate" {
                        string.clone()
                    } else {
                        int.clone()
                    },
                )
            }),
            params,
            return_var,
            body,
            sensitivity_body: Vec::new(),
        });
    }
    Ok(result)
}

/// The built-in methods of every class that randomizes its objects or
/// controls their RNG (IEEE 1800-2023 18.6, 18.8, 18.13), which each class
/// is given as subroutines.
pub(in crate::ast) const RANDOMIZATION_METHODS: [&str; 5] = [
    "randomize",
    "srandom",
    "rand_mode",
    "get_randstate",
    "set_randstate",
];

/// The argument of built-in method `method` besides the object, with its
/// type; `selection` is the type of the selection of `randomize`.
pub(in crate::ast) fn randomization_method_argument(
    method: &str,
    selection: impl FnOnce() -> Type,
) -> Option<(&'static str, Type)> {
    match method {
        "randomize" => Some(("$selection", selection())),
        "srandom" => Some(("seed", int_type())),
        "set_randstate" => Some(("state", string_type())),
        "get_randstate" => None,
        _ => Some(("on_off", classes::unsigned_type(1))),
    }
}

/// The `string` type (IEEE 1800-2023 6.16).
pub(in crate::ast) fn string_type() -> Type {
    crate::ast::type_binding::builtin_type("string", None).expect("string is a built-in type")
}

/// Select a declared enum value with rejection sampling, so non-power-of-two
/// domains are uniform (IEEE 1800-2023 18.3, 18.4.1). Keep the draws inside the
/// mode guard: a disabled property must not consume its object's random stream.
fn random_property(
    builder: &mut BodyBuilder<'_, '_, '_>,
    property: &classes::Property,
    this: &str,
) -> Vec<Stmt> {
    let cycle = property.randc.then(|| {
        property
            .cyclic_domain_size()
            .expect("validated randc domain")
    });
    random_field(builder, property.field, &property.r#type, cycle, this)
}

/// The statements that store a random value of `ty` in `field` of `this`,
/// the next of its cycle of `cycle` values for a randc one (IEEE 1800-2023
/// 18.4.1, 18.4.2): a member of an enumeration, or as many random bits as
/// the field has.
fn random_field(
    builder: &mut BodyBuilder<'_, '_, '_>,
    field: system_functions::ObjectField,
    ty: &Type,
    cycle: Option<u16>,
    this: &str,
) -> Vec<Stmt> {
    let randc = cycle.is_some();
    let random = || {
        let op = match cycle {
            Some(count) => ObjectOp::RandomCyclic {
                offset: field.offset,
                count,
            },
            None => ObjectOp::Random,
        };
        classes::object_call(op, vec![Expr::Ident(this.into())])
    };
    let mut body = Vec::new();
    let value = if let Some(values) = ty.enum_domain.as_ref().map(|domain| domain.values()) {
        let count = values.len() as u64;
        let threshold = count.wrapping_neg() % count;
        let draw = builder.declare("$enum_draw", classes::unsigned_type(64));
        if randc {
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
        let literal = |value: crate::ast::enums::MemberValue| {
            Expr::Literal(value.literal(field.width as usize, field.signed))
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
    } else if field.width > 64 && !randc {
        // Each draw gives 64 random bits.
        Expr::Concat((0..field.width.div_ceil(64)).map(|_| random()).collect())
    } else {
        random()
    };
    body.push(Stmt::Eval(classes::object_call(
        ObjectOp::Store(field),
        vec![Expr::Ident(this.into()), value],
    )));
    body
}

pub(in crate::ast) fn inline_subroutines(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    dimensions: &PackedDimensions,
) -> Converted<(Vec<Subroutine>, Vec<classes::constraints::InlineRequest>)> {
    let mut requests = std::mem::take(&mut session.inline_randomizations);
    for request in &scope::imported(tree).inline_randomizations {
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
        for name in classes::visible_classes(tree, session) {
            if name == inline.class.name {
                continue;
            }
            let Ok(class) =
                classes::shared_class(tree, &name, &dims.const_env, &dims.type_aliases, session)
            else {
                continue;
            };
            if !class.is_abstract
                && !class.is_interface
                && class.check_randomize().is_ok()
                && class.check_constraint_program().is_ok()
                && class.derives_from(
                    tree,
                    session,
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
                session,
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
                    session,
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

/// Shift the inputs of constraint `expr` from index `first` on by `by`.
fn shift_inputs(expr: &mut celox_heap::randomization::Expr, first: usize, by: usize) {
    use celox_heap::randomization::Expr;
    match expr {
        Expr::Input { index, .. } if *index >= first => *index += by,
        Expr::Enabled { value, .. }
        | Expr::Soft { value, .. }
        | Expr::Resize { value, .. }
        | Expr::Unary { value, .. } => shift_inputs(value, first, by),
        Expr::DisableSoft {
            variable,
            condition,
            ..
        } => {
            shift_inputs(variable, first, by);
            if let Some(condition) = condition {
                shift_inputs(condition, first, by);
            }
        }
        Expr::Foreach { body, .. } => shift_inputs(body, first, by),
        Expr::Binary { left, right, .. } => {
            shift_inputs(left, first, by);
            shift_inputs(right, first, by);
        }
        Expr::Dist {
            membership,
            choices,
        } => {
            shift_inputs(membership, first, by);
            for choice in choices {
                shift_inputs(&mut choice.predicate, first, by);
            }
        }
        Expr::Input { .. }
        | Expr::Literal(..)
        | Expr::Field(_)
        | Expr::Handle(_)
        | Expr::Size(_)
        | Expr::Sum(_)
        | Expr::Index { .. }
        | Expr::Static(_)
        | Expr::Solved(_)
        | Expr::Member { .. }
        | Expr::Element { .. }
        | Expr::ElementAt { .. } => {}
    }
}

/// The static rand properties of `class` (see `classes::StaticProperty::rand`):
/// the field of an object that a randomize call solves each in, its
/// variable, and the variable of the bit that turns it off.
fn static_shadows(class: &classes::Class) -> Vec<(system_functions::ObjectField, String, String)> {
    class
        .static_slots
        .iter()
        .map(|slot| (slot.field, slot.variable.clone(), slot.mode.clone()))
        .collect()
}
