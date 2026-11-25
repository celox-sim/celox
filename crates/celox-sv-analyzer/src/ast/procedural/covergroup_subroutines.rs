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
//! Each makes coverage operations only, which touch no simulation state,
//! beside what the expressions of the covergroup do and the reads of the
//! string and class handle arguments an instance keeps.
//!
//! The items of the covergroup are compiled as the subroutines are made:
//! `sample` binds the coverpoint expressions and their guards, which a
//! sample evaluates, and finds the types of the coverpoints; `construct`
//! binds the option values and the bins, which a construction evaluates,
//! and makes the descriptor of the type, which `N::$type` registers.

use super::*;
use crate::ast::covergroups::{self, Covergroup, Formal, HANDLE};
use crate::system_functions::{CoverageOp, ObjectField};
use celox_heap::coverage as heap;
use celox_sv_syntax::{CovergroupDeclaration, ExprId, ExprKind};

mod construct;
mod sample;

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

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidCovergroup(text.into())
}

fn assign(local: &str, rhs: Expr) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(local.to_owned()),
        rhs,
        nonblocking: false,
    }
}

/// A packed vector of `width` bits, of 4-state bits if `four_state`.
fn packed_type(width: u32, signed: bool, four_state: bool) -> Type {
    let mut ty = Type::new(if four_state {
        TypeKind::Logic
    } else {
        TypeKind::Bit
    });
    ty.is_signed = signed;
    if width > 1 {
        ty.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        )];
    }
    ty
}

/// What one subroutine of a covergroup is made of.
struct Parts {
    name: String,
    /// The first formal: the object or the handle.
    first: Option<(&'static str, Type)>,
    formals: Vec<Formal>,
    result: Option<Type>,
}

/// Where the subroutines of a covergroup type are made: the scope that
/// declares it.
struct Site<'x> {
    model: &'x Covergroup,
    tree: &'x AnalysisContext<'x>,
    declaration_env: &'x ConstEnv,
    item_dimensions: &'x PackedDimensions,
    literals: &'x HashMap<String, Expr>,
    item: &'x generate::Item,
    /// The class of an embedded covergroup.
    class: Option<&'x classes::Class>,
}

impl Site<'_> {
    fn declaration(&self) -> &CovergroupDeclaration {
        self.tree.declarations().covergroup(self.model.declaration)
    }

    /// Subroutine `parts` of the type: `body` makes its statements from its
    /// first formal, in the scope of its formals, and returns the variable
    /// of its result, if it has one, and what it found.
    fn subroutine<R>(
        &self,
        session: &mut Session,
        state: &mut BodyState<'_>,
        parts: Parts,
        body: impl FnOnce(
            &mut BodyBuilder<'_, '_, '_>,
            Option<String>,
        ) -> Converted<(Option<String>, Vec<Stmt>, R)>,
    ) -> Converted<(Subroutine, R)> {
        let Self {
            tree,
            declaration_env,
            item,
            literals,
            ..
        } = *self;
        let mut builder = BodyBuilder::new(
            tree,
            session,
            self.item_dimensions,
            state,
            system_functions::Body::Subroutine,
        );
        builder.automatic = true;
        builder.set_owner(parts.name.clone());
        let first_local = builder.state.locals.len();
        builder.push_scope();
        if let Some(class) = self.class {
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
        let (return_var, body, found) = body(&mut builder, first)?;
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
        Ok((subroutine, found))
    }
}

/// How an instance keeps an argument of the covergroup that its samples
/// read (IEEE 1800-2023 19.3).
enum Stored {
    /// An integral value, in the record of the instance: reading it is a
    /// coverage operation.
    Slot { slot: u32, width: u32, signed: bool },
    /// A string or a class handle, in a handle property of the object of
    /// the instance, where the collector finds it.
    Handle(ObjectField),
}

/// What making `N::$sample` found of the type.
struct Sampled {
    /// The arguments of the covergroup that a sample reads, by their index
    /// among its formals.
    stored: Vec<(usize, Stored)>,
    /// The coverpoints, without their bins.
    points: Vec<heap::PointDescriptor>,
}

impl BodyBuilder<'_, '_, '_> {
    /// Native expression `id` as a value, with the names of the body, bound
    /// as the value of a place of `target` if one is given.
    fn covergroup_value(&mut self, id: ExprId, target: Option<&Type>) -> Converted<Expr> {
        let shape = target.map(|ty| {
            bound_dimensions_from_type(
                ty,
                &self.tree.functions,
                &mut self.session.types,
                &self.dims.const_env,
            )
        });
        let value = expression_binding::bind_value(
            id,
            self.syntax().declarations(),
            &self.tree,
            self.session,
            &self.dims,
            shape.as_ref(),
        )?;
        Ok(
            if shape
                .as_ref()
                .is_some_and(|shape| shape.string && shape.unpacked.is_empty())
            {
                expression_binding::string_operand(value, &self.dims)
            } else {
                value
            },
        )
    }

    /// Check that native expression `id` may be assigned to a place of type
    /// `target` (IEEE 1800-2023 6.22.3), which `context` names.
    fn covergroup_assignable(
        &mut self,
        id: ExprId,
        target: &Type,
        context: impl Fn() -> String,
    ) -> Converted<()> {
        let Some(target) = semantic_types::of_type(
            &mut self.session.types,
            &self.tree.functions,
            target,
            &self.dims.const_env,
        ) else {
            return Ok(());
        };
        assignments::check_assign(
            &self.tree,
            self.session,
            id,
            self.syntax().declarations(),
            &self.dims,
            target,
            context,
        )
    }

    /// `value`, bound in the body, as the constant it folds to.
    fn covergroup_constant(&mut self, value: &Expr) -> Option<crate::typecheck::IntegralLiteral> {
        match fold_const_integral_expr_preserving_mask(
            &self.tree,
            self.session,
            value.clone(),
            &self.dims.const_env,
        ) {
            Expr::Literal(text) => crate::typecheck::parse_integral_literal(&text),
            _ => None,
        }
    }
}

/// `literal` as a constant of a descriptor, of type `ty` if one is given:
/// its bits are then extended by the sign of the literal or cut to the
/// width of the type.
fn constant(
    literal: crate::typecheck::IntegralLiteral,
    ty: Option<heap::ValueType>,
) -> heap::Constant {
    use num_bigint::BigUint;
    let own = heap::ValueType {
        width: literal.width as u32,
        signed: literal.signed,
    };
    let ty = ty.unwrap_or(own);
    let ones = |width: u32| (BigUint::from(1u8) << width) - 1u8;
    let resize = |plane: &BigUint| {
        let mut plane = plane & ones(own.width);
        if ty.width > own.width
            && own.signed
            && own.width > 0
            && plane.bit(u64::from(own.width) - 1)
        {
            plane |= ones(ty.width) ^ ones(own.width);
        }
        plane & ones(ty.width)
    };
    heap::Constant {
        ty,
        value: resize(&literal.value),
        mask: resize(&literal.mask),
    }
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
    let site = Site {
        model,
        tree,
        declaration_env,
        item_dimensions,
        literals,
        item,
        class,
    };
    let handle = classes::handle(&mut session.types, &model.name);
    let object = class.map(|class| classes::handle(&mut session.types, &class.name));
    let key = Expr::Literal(format!("64'd{}", model.key()));
    // `N::$sample` first: it finds the types of the coverpoints and the
    // arguments an instance keeps, which `N::$new` and the descriptor need.
    let (sample, sampled) = site.subroutine(
        session,
        state,
        Parts {
            name: model.sample_subroutine(),
            first: Some(match &object {
                Some(object) => (classes::THIS, object.clone()),
                None => (HANDLE, handle.clone()),
            }),
            formals: model.sample_formals.clone(),
            result: None,
        },
        |builder, first| {
            let first = first.expect("the object or the handle");
            let (body, sampled) = sample::body(&site, builder, first, &handle)?;
            Ok((None, body, sampled))
        },
    )?;
    let (new, descriptor) = site.subroutine(
        session,
        state,
        Parts {
            name: model.new_subroutine(),
            first: object.clone().map(|object| (classes::THIS, object)),
            formals: model.new_formals.clone(),
            result: Some(handle.clone()),
        },
        |builder, _| {
            let (instance, body, descriptor) = construct::body(&site, builder, &handle, sampled)?;
            Ok((Some(instance), body, descriptor))
        },
    )?;
    let (register, ()) = site.subroutine(
        session,
        state,
        Parts {
            name: model.type_subroutine(),
            first: None,
            formals: Vec::new(),
            result: Some(classes::unsigned_type(64)),
        },
        |builder, _| {
            Ok((
                Some(builder.declare("$type", classes::unsigned_type(64))),
                vec![
                    Stmt::If {
                        condition: coverage(CoverageOp::Registered, vec![key.clone()]),
                        then_body: Vec::new(),
                        else_body: vec![Stmt::Eval(coverage(
                            CoverageOp::Register(Arc::new(descriptor)),
                            vec![key.clone()],
                        ))],
                    },
                    Stmt::Return(Some(key.clone())),
                ],
                (),
            ))
        },
    )?;
    Ok(vec![register, new, sample])
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
        let locals = state.locals.len();
        match lower(
            &model,
            tree,
            session,
            &item.env,
            item_dimensions,
            literals,
            state,
            item,
            None,
        ) {
            Ok(lowered) => subroutines.extend(lowered),
            // What the analysis does not support in a type that nothing of
            // the scope uses is rejected where another scope uses the type:
            // the type has no subroutines, and its model says why.
            Err(error @ AnalyzerError::Unsupported(_))
                if !session.covergroup_requests.contains(&name) =>
            {
                // The variables of the subroutines that are not made.
                state.locals.truncate(locals);
                let mut model = (*model).clone();
                model.rejection = Some(covergroups::rejected(error));
                session.covergroups.insert(name, Arc::new(model));
            }
            Err(error) => return Err(covergroups::rejected(error)),
        }
    }
    Ok(subroutines)
}
