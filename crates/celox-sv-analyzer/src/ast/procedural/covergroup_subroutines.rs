//! The subroutines the analysis gives each covergroup type (IEEE 1800-2023
//! clause 19), which the uses of the type call (see `ast::covergroups`):
//!
//! - `N::$type()` registers the descriptor of the type with the heap once
//!   and returns its key. Every operation on the type takes its call as the
//!   key, so the type is registered whatever uses it first: a `new`, or a
//!   `get_coverage` before any instance exists (19.8).
//! - `N::$new([this,] formals...)` constructs an instance and returns its
//!   handle.
//! - `N::$sample(this | handle, sample formals...)` samples an instance.
//!
//! An embedded covergroup's take the object that holds it: its type is
//! anonymous (19.4), and its expressions read the members of the class.
//! Each makes coverage operations only, which touch no simulation state.

use super::*;
use crate::ast::covergroups::{self, Covergroup, Formal, HANDLE};
use crate::system_functions::CoverageOp;

/// The IR type of formal or result `ty`, as `subroutines::interface_type`
/// binds one.
fn ir(
    session: &mut Session,
    tree: &AnalysisContext<'_>,
    ty: Type,
    declaration_env: &ConstEnv,
    item: &generate::Item,
) -> crate::ir::Type {
    let mut ty = scoped_type(ty, declaration_env);
    semantic_types::bind_declared(&mut session.types, &tree.functions, &mut ty, &item.env);
    super::super::ir_type(session, &tree.functions, ty, &item.env)
}

fn coverage(op: CoverageOp, args: Vec<Expr>) -> Expr {
    classes::object_call(ObjectOp::Coverage(op), args)
}

/// What one subroutine of a covergroup is made of.
struct Parts {
    name: String,
    /// The first formal: the object or the handle.
    first: Option<(&'static str, Type)>,
    formals: Vec<Formal>,
    result: Option<Type>,
}

/// The subroutines of covergroup type `model`, which `item` declares, in
/// class `class` when it is embedded. Nothing is made for a type that
/// cannot be used: its uses report why.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower(
    model: &Covergroup,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration_env: &ConstEnv,
    item_dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    item: &generate::Item,
    class: Option<&classes::Class>,
) -> Converted<Vec<Subroutine>> {
    if model.rejection.is_some() {
        return Ok(Vec::new());
    }
    let handle = classes::handle(&mut session.types, &model.name);
    let object = class.map(|class| classes::handle(&mut session.types, &class.name));
    let key = Expr::Literal(format!("64'd{}", model.key()));
    let mut descriptor = celox_heap::coverage::TypeDescriptor::new(&model.display);
    descriptor.embedded = model.owner.is_some();
    let parts = [
        Parts {
            name: model.type_subroutine(),
            first: None,
            formals: Vec::new(),
            result: Some(classes::unsigned_type(64)),
        },
        Parts {
            name: model.new_subroutine(),
            first: object.clone().map(|object| (classes::THIS, object)),
            formals: model.new_formals.clone(),
            result: Some(handle.clone()),
        },
        Parts {
            name: model.sample_subroutine(),
            first: Some(match &object {
                Some(object) => (classes::THIS, object.clone()),
                None => (HANDLE, handle.clone()),
            }),
            formals: model.sample_formals.clone(),
            result: None,
        },
    ];
    let mut subroutines = Vec::new();
    for (index, parts) in parts.into_iter().enumerate() {
        let mut builder = BodyBuilder::new(
            tree,
            session,
            item_dimensions,
            state,
            system_functions::Body::Subroutine,
        );
        builder.automatic = true;
        builder.set_owner(parts.name.clone());
        let first_local = builder.state.locals.len();
        builder.push_scope();
        if let Some(class) = class {
            builder.enter_class_scope(class);
        }
        let mut params = Vec::new();
        let first = parts.first.map(|(source, ty)| {
            let name = builder.declare(source, ty.clone());
            params.push(SubroutineParam {
                name: name.clone(),
                source_name: source.to_owned(),
                direction: ParamDirection::Input,
                r#type: ir(builder.session, tree, ty, declaration_env, item),
                default: None,
                leaves: Vec::new(),
            });
            name
        });
        // A default is bound before the formals, which could hide the
        // names it reads (IEEE 1800-2023 13.5.3).
        let defaults = parts
            .formals
            .iter()
            .map(|formal| {
                formal
                    .default
                    .map(|root| {
                        let mut value = expression_binding::bind_value(
                            root,
                            tree.declarations(),
                            tree,
                            builder.session,
                            &builder.dims,
                            None,
                        )?;
                        builder.names.rename_expr(&mut value);
                        Ok(value)
                    })
                    .transpose()
            })
            .collect::<Converted<Vec<_>>>()?;
        builder.push_scope();
        for (formal, default) in parts.formals.into_iter().zip(defaults) {
            let name = builder.declare(&formal.name, formal.r#type.clone());
            params.push(SubroutineParam {
                name,
                source_name: formal.name,
                direction: formal.direction,
                r#type: ir(builder.session, tree, formal.r#type, declaration_env, item),
                default,
                leaves: Vec::new(),
            });
        }
        let (return_var, body) = match index {
            0 => (
                Some(builder.declare("$type", classes::unsigned_type(64))),
                vec![
                    Stmt::If {
                        condition: coverage(CoverageOp::Registered, vec![key.clone()]),
                        then_body: Vec::new(),
                        else_body: vec![Stmt::Eval(coverage(
                            CoverageOp::Register(Arc::new(descriptor.clone())),
                            vec![key.clone()],
                        ))],
                    },
                    Stmt::Return(Some(key.clone())),
                ],
            ),
            1 => {
                let instance = builder.declare(HANDLE, handle.clone());
                let body = vec![
                    Stmt::Assign {
                        lhs: LValue::Ident(instance.clone()),
                        rhs: coverage(
                            CoverageOp::New {
                                class: model.class(),
                                size: 0,
                                handles: Vec::new(),
                            },
                            vec![Expr::Call {
                                callee: Callee::Subroutine(model.type_subroutine()),
                                args: Vec::new(),
                                arg_order: Vec::new(),
                            }],
                        ),
                        nonblocking: false,
                    },
                    Stmt::Eval(coverage(
                        CoverageOp::Build,
                        vec![Expr::Ident(instance.clone())],
                    )),
                ];
                (Some(instance), body)
            }
            _ => {
                let first = first.expect("the object or the handle");
                let mut body = Vec::new();
                let instance = match class {
                    // The handle is a property of the object (19.4).
                    Some(class) => {
                        let property = class
                            .properties
                            .iter()
                            .rev()
                            .find(|property| {
                                Some(&property.owner) == model.owner.as_ref()
                                    && property.name == model.identifier
                            })
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "covergroup `{}` has no property in its class",
                                    model.display
                                ))
                            })?;
                        let instance = builder.declare(HANDLE, handle.clone());
                        body.push(Stmt::Assign {
                            lhs: LValue::Ident(instance.clone()),
                            rhs: expression_binding::objects::load(property, Expr::Ident(first)),
                            nonblocking: false,
                        });
                        instance
                    }
                    None => first,
                };
                // An instance that is not built or was stopped, and a
                // `null` handle, sample nothing (19.4, 19.8).
                body.push(Stmt::If {
                    condition: coverage(CoverageOp::Sampling, vec![Expr::Ident(instance.clone())]),
                    then_body: vec![Stmt::Eval(coverage(
                        CoverageOp::SampleEnd,
                        vec![Expr::Ident(instance)],
                    ))],
                    else_body: Vec::new(),
                });
                (None, body)
            }
        };
        builder.pop_scope();
        builder.pop_scope();
        builder.flush_locator_locals();
        let mut subroutine = Subroutine {
            name: parts.name,
            is_task: false,
            automatic: true,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: parts
                .result
                .map(|ty| ir(builder.session, tree, ty, declaration_env, item)),
            params,
            return_var,
            body,
            sensitivity_body: Vec::new(),
            class_member: false,
        };
        for stmt in &mut subroutine.body {
            qualify_stmt(item, stmt);
            substitute_stmt_constants(tree, builder.session, stmt, declaration_env, literals);
        }
        for param in &mut subroutine.params {
            if let Some(default) = &mut param.default {
                item.expr(default);
                *default = substitute_expr_constants_with_parameter_literals(
                    tree,
                    builder.session,
                    default.clone(),
                    &item.env,
                    literals,
                );
            }
        }
        drop(builder);
        subroutines.push(subroutine);
    }
    Ok(subroutines)
}

/// The subroutines of the scope-level covergroup types the scope being
/// analyzed declares. A package and a compilation unit make those of every
/// type, as another scope may use it; a module or a program makes those of
/// the types it has bound a `new` or a method of, so that a type nothing
/// uses has none.
pub(in crate::ast) fn scope_subroutines(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    every: bool,
) -> Converted<Vec<Subroutine>> {
    if session.covergroups.is_empty() || !(every || !session.covergroup_requests.is_empty()) {
        return Ok(Vec::new());
    }
    let native = tree.declarations();
    let active =
        generate::subroutine_items(tree, session, const_env, &packed_dimensions.type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    let mut subroutines = Vec::new();
    for item in &active {
        let celox_sv_syntax::BodyItemKind::Covergroup(id) = native.body_item(item.id).kind else {
            continue;
        };
        if covergroups::place(tree, id) != covergroups::Place::Scope {
            continue;
        }
        let name = covergroups::scope_names(tree, session, id, Some(item.scope_name())).class;
        let Some(model) = session.covergroups.get(&name).cloned() else {
            continue;
        };
        if !every && !session.covergroup_requests.contains(&name) {
            continue;
        }
        let tree = &item.scoped(tree);
        let (item_dimensions, literals) = views.get(item);
        let tree = &item.enter(tree);
        subroutines.extend(lower(
            &model,
            tree,
            session,
            &item.env,
            item_dimensions,
            literals,
            state,
            item,
            None,
        )?);
    }
    Ok(subroutines)
}
