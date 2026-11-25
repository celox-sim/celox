//! `N::$sample` of a covergroup type (IEEE 1800-2023 19.5, 19.8).
//!
//! In declaration order it evaluates the guard and then the value of each
//! coverpoint into a variable. Only then it hands the values to the heap,
//! in calls that nothing of the design runs between, so that a function of
//! a coverpoint expression may sample too (19.5).

use super::*;
use celox_sv_syntax::Coverpoint;

/// The names of the formals `formals` that the coverpoint expressions,
/// guards and types of `bound` use.
fn names_read(site: &Site<'_>, bound: &covergroups::Bound, formals: &[Formal]) -> HashSet<String> {
    let native = site.tree.declarations();
    let mut read = HashSet::default();
    if formals.is_empty() {
        return read;
    }
    let mut note = |name: String| {
        if formals.iter().any(|formal| formal.name == name) {
            read.insert(name);
        }
    };
    for root in &bound.sampled {
        for (name, _) in covergroups::variable_names(native, *root) {
            note(name);
        }
    }
    // The type of a coverpoint may name one too, as in
    // `bit [$bits(v)-1:0]`.
    for ty in &bound.types {
        let tokens = native.data_type(*ty).tokens;
        for id in site.tree.spans().covergroup_bound.0.within(&[tokens]) {
            let expression = native.expression(id);
            if (tokens.start..tokens.end).contains(&expression.tokens.start)
                && let ExprKind::Name(name) = &expression.kind
            {
                note(instances::normalize_identifier(&name.spelling));
            }
        }
    }
    read
}

/// The variable of the handle of the instance: `first` itself, or for an
/// embedded covergroup the property of object `first` that holds it
/// (19.4), which `body` reads.
fn instance(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    first: String,
    handle: &Type,
    body: &mut Vec<Stmt>,
) -> Converted<String> {
    let model = site.model;
    let Some(class) = site.class else {
        return Ok(first);
    };
    let property = class
        .properties
        .iter()
        .rev()
        .find(|property| {
            Some(&property.owner) == model.owner.as_ref() && property.name == model.identifier
        })
        .ok_or_else(|| {
            unsupported(format!(
                "covergroup `{}` has no property in its class",
                model.display
            ))
        })?;
    let instance = builder.declare(HANDLE, handle.clone());
    body.push(assign(
        &instance,
        expression_binding::objects::load(property, Expr::Ident(first)),
    ));
    Ok(instance)
}

/// The arguments of the covergroup that its coverpoints read: instance
/// `cg` kept them at its construction (19.3), and `sampling` reads them
/// back into variables of their names. These are declared with the
/// arguments of `sample`, so that a name finds them before the members of
/// the class and the names of the scope (19.8.1).
fn stored_formals(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    cg: &Expr,
    sampling: &mut Vec<Stmt>,
) -> Converted<Vec<(usize, Stored)>> {
    let model = site.model;
    let bound = covergroups::Bound::of(site.declaration());
    let read = names_read(site, &bound, &model.new_formals);
    let mut stored = Vec::new();
    let (mut slot, mut offset) = (0, 0);
    for (index, formal) in model.new_formals.iter().enumerate() {
        if !read.contains(&formal.name) {
            continue;
        }
        let ty = &formal.r#type;
        let local = builder.declare(&formal.name, ty.clone());
        let kept = match ty.kind() {
            TypeKind::String | TypeKind::Object => {
                let field =
                    classes::field(&builder.tree.functions, ty, &builder.dims.const_env, offset)?;
                offset = field.offset + field.bytes().next_multiple_of(8);
                sampling.push(assign(
                    &local,
                    classes::object_call(ObjectOp::Load(field), vec![cg.clone()]),
                ));
                Stored::Handle(field)
            }
            TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => {
                let shape = casts::expr_type_from_type(
                    &builder.tree.functions,
                    ty,
                    &builder.dims.const_env,
                )
                .ok_or_else(|| unsupported("covergroup argument type"))?;
                let (width, signed) = (shape.width as u32, shape.signed);
                let four_state = ty.kind() != TypeKind::Bit;
                sampling.push(assign(
                    &local,
                    coverage(
                        CoverageOp::Formal {
                            slot,
                            width,
                            signed,
                            four_state,
                        },
                        vec![cg.clone()],
                    ),
                ));
                slot += 1;
                Stored::Slot {
                    slot: slot - 1,
                    width,
                    signed,
                }
            }
            _ => {
                return Err(unsupported(format!(
                    "covergroup argument `{}` of a type that is not integral, a string or a class, read at a sample (IEEE 1800-2023 19.3)",
                    formal.name
                )));
            }
        };
        stored.push((index, kept));
    }
    Ok(stored)
}

/// A coverpoint in `N::$sample`: the variable of its value, the variable
/// that tells whether its guard held, if it has one, and its type.
struct Point {
    value: String,
    guard: Option<String>,
    width: u32,
    signed: bool,
}

/// The type of coverpoint `coverpoint`, named `name`: the type it declares,
/// which its expression is then assigned to, else the type of its
/// expression (19.5).
fn point_type(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    coverpoint: &Coverpoint,
    name: &str,
) -> Converted<(heap::PointType, Option<Type>)> {
    let model = site.model;
    let native = site.tree.declarations();
    let declared = match coverpoint.data_type {
        Some(data_type) => {
            let ty = type_binding::bind(data_type, &builder.tree, builder.session, &builder.dims)?
                .ok_or_else(|| unsupported("covergroup coverpoint type"))?;
            builder.covergroup_assignable(coverpoint.expression, &ty, || {
                format!("coverpoint `{name}`")
            })?;
            Some(ty)
        }
        None => None,
    };
    let ty = match &declared {
        Some(ty) => semantic_types::of_type(
            &mut builder.session.types,
            &builder.tree.functions,
            ty,
            &builder.dims.const_env,
        )
        .ok_or_else(|| unsupported("covergroup coverpoint type"))?,
        None => expression_types::type_of(
            &builder.tree,
            builder.session,
            coverpoint.expression,
            native,
            &builder.dims,
        )?,
    };
    let packed = match builder.session.types.data(ty) {
        semantic_types::TyData::Packed(packed) => packed.clone(),
        semantic_types::TyData::Real | semantic_types::TyData::Shortreal => {
            return Err(unsupported(
                "covergroup real coverpoint (IEEE 1800-2023 19.5)",
            ));
        }
        _ => {
            return Err(invalid(format!(
                "coverpoint `{name}` of covergroup `{}` is not of an integral type (IEEE 1800-2023 19.5)",
                model.display
            )));
        }
    };
    let width = u32::try_from(packed.bits())
        .ok()
        .filter(|width| *width != 0)
        .ok_or_else(|| unsupported("covergroup coverpoint of this many bits"))?;
    let value_type = heap::ValueType {
        width,
        signed: packed.signed,
    };
    // The automatic bins of an enumeration are its enumerators (19.5.3).
    let members = match &packed.element {
        semantic_types::PackedElement::Enum { array: false, .. } => {
            let domain = builder.session.types.enum_domain(ty).ok_or_else(|| {
                unsupported(
                    "covergroup coverpoint of an enumeration whose members have no known values (IEEE 1800-2023 19.5.3)",
                )
            })?;
            domain
                .members
                .iter()
                .map(|member| {
                    (
                        member.name.clone(),
                        constant(
                            crate::typecheck::IntegralLiteral {
                                width: 128,
                                signed: true,
                                value: num_bigint::BigUint::from(member.value as u128),
                                mask: num_bigint::BigUint::default(),
                            },
                            Some(value_type),
                        ),
                    )
                })
                .collect()
        }
        _ => Vec::new(),
    };
    Ok((
        heap::PointType {
            width,
            signed: packed.signed,
            four_state: packed.four_state,
            members,
        },
        declared,
    ))
}

/// Coverpoint `coverpoint`, item `item` of the type: `sampling` evaluates
/// its guard and, where that holds, its expression. The expression of a
/// coverpoint whose guard is false is not evaluated.
fn point(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    coverpoint: &Coverpoint,
    item: usize,
    sampling: &mut Vec<Stmt>,
) -> Converted<(Point, heap::PointDescriptor)> {
    let native = site.tree.declarations();
    let (name, generated) = covergroups::point_name(native, coverpoint, item);
    let (ty, declared) = point_type(site, builder, coverpoint, &name)?;
    let guard = coverpoint
        .iff
        .map(|iff| builder.native_value(iff))
        .transpose()?;
    let mut value = builder.covergroup_value(coverpoint.expression, declared.as_ref())?;
    builder.rename_expr(&mut value);
    let local = builder.declare(
        &format!("$point{item}"),
        declared.unwrap_or_else(|| packed_type(ty.width, ty.signed, ty.four_state)),
    );
    let guard = match guard {
        Some(condition) => {
            let guard = builder.declare(&format!("$guard{item}"), packed_type(1, false, false));
            sampling.push(assign(&guard, Expr::Literal("1'b0".into())));
            sampling.push(Stmt::If {
                condition,
                then_body: vec![
                    assign(&guard, Expr::Literal("1'b1".into())),
                    assign(&local, value),
                ],
                else_body: Vec::new(),
            });
            Some(guard)
        }
        None => {
            sampling.push(assign(&local, value));
            None
        }
    };
    let point = Point {
        value: local,
        guard,
        width: ty.width,
        signed: ty.signed,
    };
    let mut descriptor = heap::PointDescriptor::new(name, ty);
    descriptor.generated_name = generated;
    Ok((point, descriptor))
}

/// The calls that hand the values of `points` of instance `cg` to the
/// heap. Coverpoints that follow each other share a call while their values
/// fit 64 bits, with a bit for each that tells whether its guard held where
/// one of them has a guard; a coverpoint of more than 64 bits has a call of
/// its own.
fn hand_over(points: &[Point], cg: &Expr) -> Vec<Stmt> {
    // A concatenation lists the highest bits first.
    let concat = |mut parts: Vec<Expr>| {
        if parts.len() == 1 {
            parts.remove(0)
        } else {
            parts.reverse();
            Expr::Concat(parts)
        }
    };
    let mut calls = Vec::new();
    let mut next = 0;
    while next < points.len() {
        let point = &points[next];
        if point.width > 64 {
            let call = Stmt::Eval(coverage(
                CoverageOp::Point {
                    item: next as u32 + 1,
                    width: point.width,
                    signed: point.signed,
                },
                vec![cg.clone(), Expr::Ident(point.value.clone())],
            ));
            calls.push(match &point.guard {
                Some(guard) => Stmt::If {
                    condition: Expr::Ident(guard.clone()),
                    then_body: vec![call],
                    else_body: Vec::new(),
                },
                None => call,
            });
            next += 1;
            continue;
        }
        let first = next;
        let mut width = 0;
        while next < points.len() && points[next].width <= 64 - width {
            width += points[next].width;
            next += 1;
        }
        let pack = &points[first..next];
        let mut args = vec![
            cg.clone(),
            concat(
                pack.iter()
                    .map(|point| Expr::Ident(point.value.clone()))
                    .collect(),
            ),
        ];
        if pack.iter().any(|point| point.guard.is_some()) {
            args.push(concat(
                pack.iter()
                    .map(|point| match &point.guard {
                        Some(guard) => Expr::Ident(guard.clone()),
                        None => Expr::Literal("1'b1".into()),
                    })
                    .collect(),
            ));
        }
        calls.push(Stmt::Eval(coverage(
            CoverageOp::Points {
                first: first as u32 + 1,
                count: (next - first) as u32,
                width,
            },
            args,
        )));
    }
    calls
}

/// The body of `N::$sample` of the type of `site`, from `first`, the
/// object or the handle, and what it found of the type.
pub(super) fn body(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    first: String,
    handle: &Type,
) -> Converted<(Vec<Stmt>, Sampled)> {
    let mut body = Vec::new();
    let cg = Expr::Ident(instance(site, builder, first, handle, &mut body)?);
    // What runs when the instance samples.
    let mut sampling = Vec::new();
    let stored = stored_formals(site, builder, &cg, &mut sampling)?;
    let mut points = Vec::new();
    let mut descriptors = Vec::new();
    for (index, coverpoint) in covergroups::coverpoints(site.declaration()).enumerate() {
        let (point, descriptor) = point(site, builder, coverpoint, index + 1, &mut sampling)?;
        points.push(point);
        descriptors.push(descriptor);
    }
    sampling.extend(hand_over(&points, &cg));
    sampling.push(Stmt::Eval(coverage(
        CoverageOp::SampleEnd,
        vec![cg.clone()],
    )));
    // An instance that is not built or was stopped, and a `null` handle,
    // sample nothing and evaluate nothing (19.4, 19.8).
    body.push(Stmt::If {
        condition: coverage(CoverageOp::Sampling, vec![cg]),
        then_body: sampling,
        else_body: Vec::new(),
    });
    Ok((
        body,
        Sampled {
            stored,
            points: descriptors,
        },
    ))
}
