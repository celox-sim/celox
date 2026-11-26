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
//! - `N::$spawn([this,] handle)`, of a type with a clocking event, starts
//!   the process that samples an instance at the event and returns the
//!   handle (see `spawn`).
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
use celox_sv_syntax::{
    Bins, BinsKind, BinsValue, CoverageRange, CoverageRangeKind, CovergroupDeclaration, CrossItem,
    ExprId, ExprKind,
};
use num_bigint::BigUint;

mod construct;
mod sample;
mod spawn;

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
            result_leaves: Vec::new(),
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

/// The predicate bits of a bin declaration among those of its coverpoint,
/// which a sample hands over (IEEE 1800-2023 19.5.1, 19.5.1.1).
#[derive(Clone, Copy, Default)]
struct BinBits {
    /// The bit of the guard of the declaration.
    iff: Option<u32>,
    /// The bit of its `with` filter, where a sample evaluates it for the
    /// sampled value: a construction is not asked about its values then.
    filter: Option<u32>,
}

/// What making `N::$sample` found of the type.
struct Sampled {
    /// The arguments of the covergroup that a sample reads, by their index
    /// among its formals.
    stored: Vec<(usize, Stored)>,
    /// The coverpoints, without their bins.
    points: Vec<heap::PointDescriptor>,
    /// The type of the variable that holds the value of each coverpoint,
    /// which `item` has in a `with` expression of its bins (19.5.1.1).
    items: Vec<Type>,
    /// The predicate bits of the bin declarations of each coverpoint, in
    /// declaration order.
    bins: Vec<Vec<BinBits>>,
    /// The predicate bits of each cross.
    crosses: Vec<CrossBits>,
}

/// The predicate bits of a cross, which a sample hands over for it: its
/// guard and those of its bin declarations (19.6), in that order.
#[derive(Clone, Default)]
struct CrossBits {
    iff: Option<u32>,
    /// The bit of each bin declaration, in declaration order.
    bins: Vec<Option<u32>>,
    count: u32,
}

impl CrossBits {
    fn of(cross: &celox_sv_syntax::Cross) -> Self {
        let mut count = 0;
        let mut bit = || {
            count += 1;
            count - 1
        };
        let iff = cross.iff.map(|_| bit());
        let bins = cross
            .body_items
            .iter()
            .filter_map(|item| match item {
                CrossItem::Bins(bins) => Some(bins.iff.map(|_| bit())),
                _ => None,
            })
            .collect();
        Self { iff, bins, count }
    }
}

/// How many values the `with` filter of a bin is asked about (19.5.1.1),
/// as far as the analysis knows.
enum Candidates {
    /// A value or bound of the bin is known only when an instance is
    /// constructed.
    Unknown,
    /// This many, each value as often as the bin lists it.
    Listed(BigUint),
    /// A wildcard value of the bin is matched as a pattern: its values are
    /// not listed (see `heap::MAX_WILDCARD_EXPANSION`).
    Pattern,
}

impl Candidates {
    /// Whether a construction can be asked about every value: there are
    /// no more than `heap::MAX_FILTER_CANDIDATES`, or only a construction
    /// knows how many there are.
    fn enumerable(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::Listed(count) => *count <= BigUint::from(heap::MAX_FILTER_CANDIDATES),
            Self::Pattern => false,
        }
    }
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

impl BodyBuilder<'_, '_, '_> {
    /// The type of operand `root` of a bin, which is integral (19.5.1).
    fn covergroup_operand_type(&mut self, root: ExprId) -> Converted<heap::ValueType> {
        let native = self.syntax().declarations();
        let ty = expression_types::type_of(&self.tree, self.session, root, native, &self.dims)?;
        let packed = match self.session.types.data(ty) {
            semantic_types::TyData::Packed(packed) => packed.clone(),
            _ => {
                return Err(unsupported(
                    "covergroup bin value that is not integral (IEEE 1800-2023 19.5.1)",
                ));
            }
        };
        Ok(heap::ValueType {
            width: u32::try_from(packed.bits())
                .ok()
                .filter(|width| *width != 0)
                .ok_or_else(|| unsupported("covergroup bin value of this many bits"))?,
            signed: packed.signed,
        })
    }

    /// Operand `root` of a bin as a constant of its type, where the body
    /// binds it to one.
    fn covergroup_operand_constant(&mut self, root: ExprId) -> Option<heap::Constant> {
        let ty = self.covergroup_operand_type(root).ok()?;
        let value = self.covergroup_value(root, None).ok()?;
        let literal = self.covergroup_constant(&value)?;
        Some(constant(literal, Some(ty)))
    }

    /// The bounds of the tolerance range `[center +/- tolerance]`, or
    /// `[center +%- tolerance]` if `relative` (11.4.13), where both fold
    /// to constants: of the type of `center`, the lower first.
    fn covergroup_tolerance_constants(
        &mut self,
        center: ExprId,
        tolerance: ExprId,
        relative: bool,
    ) -> Option<(heap::Constant, heap::Constant)> {
        let ty = self.covergroup_operand_type(center).ok()?;
        let tolerance_type = self.covergroup_operand_type(tolerance).ok()?;
        let center = self.covergroup_value(center, None).ok()?;
        let tolerance = self.covergroup_value(tolerance, None).ok()?;
        let tolerance = tolerance_value(&center, ty, tolerance, tolerance_type, relative);
        let (low, high) = tolerance_bounds(&center, &tolerance, ty);
        let low = self.covergroup_constant(&low)?;
        let high = self.covergroup_constant(&high)?;
        Some((constant(low, Some(ty)), constant(high, Some(ty))))
    }

    /// Element `range` of the range list of a bin with its operands as
    /// constants, where the body binds every one of them to one.
    fn covergroup_constant_range(
        &mut self,
        range: &CoverageRange,
    ) -> Option<heap::RangeDescriptor> {
        let operand = heap::Operand::Constant;
        Some(match &range.kind {
            CoverageRangeKind::Value(value) => {
                heap::RangeDescriptor::Value(operand(self.covergroup_operand_constant(*value)?))
            }
            CoverageRangeKind::Range { low, high } => {
                let mut bound = |bound: &Option<ExprId>| {
                    Some(match bound {
                        Some(bound) => {
                            heap::Bound::Operand(operand(self.covergroup_operand_constant(*bound)?))
                        }
                        None => heap::Bound::Open,
                    })
                };
                heap::RangeDescriptor::Range {
                    low: bound(low)?,
                    high: bound(high)?,
                }
            }
            CoverageRangeKind::Tolerance {
                center,
                tolerance,
                relative,
            } => {
                let (low, high) =
                    self.covergroup_tolerance_constants(*center, *tolerance, *relative)?;
                heap::RangeDescriptor::Range {
                    low: heap::Bound::Operand(operand(low)),
                    high: heap::Bound::Operand(operand(high)),
                }
            }
        })
    }
}

/// `value` of type `from` as a value of type `to`: extended by its own
/// signedness, or cut, to the width of `to`, as a cast to that type does
/// (6.24.1).
fn cast(value: Expr, from: heap::ValueType, to: heap::ValueType) -> Expr {
    let resize = |expr: Expr, signed: bool| Expr::Resize {
        expr: Box::new(expr),
        width: to.width as usize,
        signed,
    };
    let sized = resize(value, from.signed);
    if from.signed == to.signed {
        sized
    } else {
        resize(sized, to.signed)
    }
}

/// The tolerance of a tolerance range of constants as a value of the type
/// `ty` of its center (11.4.13): `type(A)'(B)` of `[A +/- B]`, and `type(A)'(A*B/100.0)`
/// of `[A +%- B]`, where the conversion truncates. The operands are
/// integral and keep their own signedness: each is extended by it to a
/// signed type that holds the product, which is divided as integers are,
/// towards zero.
fn tolerance_value(
    center: &Expr,
    ty: heap::ValueType,
    tolerance: Expr,
    tolerance_type: heap::ValueType,
    relative: bool,
) -> Expr {
    if !relative {
        return cast(tolerance, tolerance_type, ty);
    }
    let wide = heap::ValueType {
        width: (ty.width + tolerance_type.width + 2).max(16),
        signed: true,
    };
    let binary = |left: Expr, op: BinaryOp, right: Expr| Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    cast(
        binary(
            binary(
                cast(center.clone(), ty, wide),
                BinaryOp::Mul,
                cast(tolerance, tolerance_type, wide),
            ),
            BinaryOp::Div,
            Expr::Literal(format!("{}'sd100", wide.width)),
        ),
        wide,
        ty,
    )
}

/// The bounds of the tolerance range of the constants `center` and
/// `tolerance`, both of type `ty` (11.4.13): `center - tolerance` and `center + tolerance` in
/// that type, the lower of the two first.
fn tolerance_bounds(center: &Expr, tolerance: &Expr, ty: heap::ValueType) -> (Expr, Expr) {
    let bound = |op: BinaryOp| Expr::Resize {
        expr: Box::new(Expr::Binary {
            left: Box::new(center.clone()),
            op,
            right: Box::new(tolerance.clone()),
        }),
        width: ty.width as usize,
        signed: ty.signed,
    };
    let (below, above) = (bound(BinaryOp::Sub), bound(BinaryOp::Add));
    let ordered = Expr::Binary {
        left: Box::new(below.clone()),
        op: BinaryOp::Le,
        right: Box::new(above.clone()),
    };
    let choose = |then_expr: &Expr, else_expr: &Expr| Expr::Mux {
        condition: Box::new(ordered.clone()),
        then_expr: Box::new(then_expr.clone()),
        else_expr: Box::new(else_expr.clone()),
    };
    (choose(&below, &above), choose(&above, &below))
}

/// The element `range` of a range list whose operands are constants as
/// the heap resolves one.
fn range_value(range: &heap::RangeDescriptor) -> Option<heap::RangeValue<'_>> {
    fn constant(operand: &heap::Operand) -> Option<&heap::Constant> {
        match operand {
            heap::Operand::Constant(constant) => Some(constant),
            heap::Operand::Input(_) => None,
        }
    }
    fn bound(bound: &heap::Bound) -> Option<Option<&heap::Constant>> {
        match bound {
            heap::Bound::Open => Some(None),
            heap::Bound::Operand(value) => constant(value).map(Some),
        }
    }
    Some(match range {
        heap::RangeDescriptor::Value(value) => heap::RangeValue::Value(constant(value)?),
        heap::RangeDescriptor::Range { low, high } => heap::RangeValue::Range {
            low: bound(low)?,
            high: bound(high)?,
        },
    })
}

/// How many values the `with` filter of bin declaration `bins` of a
/// coverpoint of type `ty` is asked about, as the body of `builder` binds
/// the operands of the bin. An operand that names an argument of the
/// covergroup is known only to a construction, whatever the body binds the
/// name to.
fn candidates(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    bins: &Bins,
    ty: &heap::PointType,
) -> Candidates {
    let native = site.tree.declarations();
    let ranges = match &bins.value {
        // Every value of the coverpoint: of an enumeration its named
        // values.
        BinsValue::CoverpointWith { .. } => {
            return Candidates::Listed(if ty.members.is_empty() {
                BigUint::from(1u8) << ty.width
            } else {
                BigUint::from(ty.members.len())
            });
        }
        BinsValue::Ranges { ranges, .. } => ranges,
        _ => return Candidates::Unknown,
    };
    let formal = |root: ExprId| {
        covergroups::variable_names(native, root)
            .into_iter()
            .any(|(name, _)| {
                site.model
                    .new_formals
                    .iter()
                    .any(|formal| formal.name == name)
            })
    };
    let mut total = BigUint::ZERO;
    for range in ranges {
        let operands: Vec<ExprId> = match &range.kind {
            CoverageRangeKind::Value(value) => vec![*value],
            CoverageRangeKind::Range { low, high } => low.iter().chain(high).copied().collect(),
            CoverageRangeKind::Tolerance {
                center, tolerance, ..
            } => vec![*center, *tolerance],
        };
        if operands.into_iter().any(formal) {
            return Candidates::Unknown;
        }
        let Some(range) = builder.covergroup_constant_range(range) else {
            return Candidates::Unknown;
        };
        let Some(value) = range_value(&range) else {
            return Candidates::Unknown;
        };
        match heap::value_count(ty, bins.wildcard, value) {
            Some(count) => total += count,
            None => return Candidates::Pattern,
        }
    }
    Candidates::Listed(total)
}

/// Whether a sample, not a construction, evaluates the `with` filter of
/// bin declaration `bins` of a coverpoint of type `ty` (19.5.1.1): the bin
/// is one `bins` bin and its values are known to be more than a
/// construction is asked about. The bin then has every value of its range
/// list, and a sample whose value the filter does not keep is none of it.
fn filtered_at_each_sample(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    bins: &Bins,
    ty: &heap::PointType,
) -> bool {
    covergroups::filter(bins).is_some()
        && bins.kind == BinsKind::Bins
        && bins.array.is_none()
        && !candidates(site, builder, bins, ty).enumerable()
}

/// The statements that run coverage operation `op` of instance `cg`, which
/// returns how many messages the heap has for it, and report those with
/// severity `severity`: the warnings of a construction, or the illegal bins
/// a sample hit. `count` names the variable of the number.
fn reported(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    cg: &Expr,
    op: CoverageOp,
    severity: system_functions::Severity,
    count: &str,
) -> Vec<Stmt> {
    let count = builder.declare(count, classes::unsigned_type(32));
    vec![
        assign(&count, coverage(op, vec![cg.clone()])),
        Stmt::If {
            condition: Expr::Binary {
                left: Box::new(Expr::Ident(count)),
                op: BinaryOp::Ne,
                right: Box::new(Expr::Literal("32'd0".into())),
            },
            then_body: vec![Stmt::SystemTask {
                task: SystemTask::Severity(severity),
                args: vec![
                    SystemTaskArg::Str("%s".into()),
                    SystemTaskArg::Expr(coverage(CoverageOp::Message, vec![cg.clone()])),
                ],
                site: source_span(site.declaration().header, site.tree),
                assertion: None,
                home: None,
            }],
            else_body: Vec::new(),
        },
    ]
}

/// `literal` as a constant of a descriptor, of type `ty` if one is given:
/// its bits are then extended by the sign of the literal or cut to the
/// width of the type.
fn constant(
    literal: crate::typecheck::IntegralLiteral,
    ty: Option<heap::ValueType>,
) -> heap::Constant {
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

/// `N::$counts` of an instance if `instance`, else `N::$type_counts` of the
/// type of `site` (IEEE 1800-2023 19.8): they assign the covered bins and
/// the bins of the item to their `ref int` formals and return its coverage,
/// as `get_inst_coverage` and `get_coverage` with two arguments do. The
/// latter does not read a handle, so a `null` one is no error.
fn counts(
    site: &Site<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    handle: &Type,
    instance: bool,
) -> Converted<Subroutine> {
    let model = site.model;
    let key = Expr::Call {
        callee: Callee::Subroutine(model.type_subroutine()),
        args: Vec::new(),
        arg_order: Vec::new(),
    };
    let real = crate::ast::type_binding::builtin_type("real", None).expect("`real` is a type");
    let (subroutine, ()) = site.subroutine(
        session,
        state,
        Parts {
            name: if instance {
                model.counts_subroutine()
            } else {
                model.type_counts_subroutine()
            },
            first: instance.then(|| (HANDLE, handle.clone())),
            formals: covergroups::count_formals(),
            result: Some(real.clone()),
        },
        |builder, first| {
            let target = match first {
                Some(first) => Expr::Ident(first),
                None => key.clone(),
            };
            let mut item = Expr::Ident(covergroups::ITEM_FORMAL.to_owned());
            builder.rename_expr(&mut item);
            let query = |what: system_functions::CoverageQuery| {
                coverage(
                    if instance {
                        CoverageOp::InstCoverage { what }
                    } else {
                        CoverageOp::TypeCoverage { what }
                    },
                    vec![target.clone(), item.clone()],
                )
            };
            let result = builder.declare("$coverage", real);
            let mut body = Vec::new();
            for (formal, what) in [
                ("covered", system_functions::CoverageQuery::Covered),
                ("total", system_functions::CoverageQuery::Total),
            ] {
                let mut lhs = LValue::Ident(formal.to_owned());
                builder.rename_lvalue(&mut lhs);
                body.push(Stmt::Assign {
                    lhs,
                    rhs: query(what),
                    nonblocking: false,
                });
            }
            body.push(assign(
                &result,
                query(system_functions::CoverageQuery::Real),
            ));
            Ok((Some(result), body, ()))
        },
    )?;
    Ok(subroutine)
}

/// The subroutines of covergroup type `model`, which `item` declares, in
/// class `class` when it is embedded, with `N::$counts` and
/// `N::$type_counts` if `counts`, and `N::$spawn` if `spawn` and the type
/// has a clocking event. Nothing is made for a type that cannot be used:
/// its uses report why.
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
    counts: bool,
    spawn: bool,
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
    let mut subroutines = vec![register, new, sample];
    if counts {
        for instance in [true, false] {
            subroutines.push(self::counts(&site, session, state, &handle, instance)?);
        }
    }
    if spawn && let Some(event) = model.event {
        // The instances of a type of a module or a program go to the list
        // its sampler walks; those of an embedded type, and of a type of a
        // package or the compilation unit, each get a process.
        let list = class
            .is_none()
            .then(|| session.covergroup_instance_lists.get(&model.name).cloned())
            .flatten();
        let (spawned, ()) = site.subroutine(
            session,
            state,
            Parts {
                name: model.spawn_subroutine(),
                first: object.map(|object| (classes::THIS, object)),
                formals: vec![Formal {
                    name: HANDLE.to_owned(),
                    direction: ParamDirection::Input,
                    r#type: handle.clone(),
                    default: None,
                }],
                result: Some(handle.clone()),
            },
            |builder, first| {
                let (result, body) = match &list {
                    Some(list) => spawn::listed(builder, list, &handle),
                    None => spawn::body(&site, builder, first, &handle, event)?,
                };
                Ok((Some(result), body, ()))
            },
        )?;
        subroutines.push(spawned);
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
            every || session.covergroup_count_requests.contains(&name),
            every || session.covergroup_spawn_requests.contains(&name),
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
