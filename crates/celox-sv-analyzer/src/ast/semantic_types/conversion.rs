//! Semantic types of the [`Type`]s that binding builds and of the
//! [`VariableDimensions`] of variables.
use super::*;
use crate::ast::{
    ConstEnv, ConstExpr, ContainerKind, ContainerType, Type, TypeKind, VariableDimensions,
    const_functions, eval_ast_const_expr, packed_structs::PackedMember, type_names,
    unpacked_structs::UnpackedStruct,
};

/// The semantic type of `ty`, whose ranges name constants of `env`; `None`
/// if a range is not constant or the type has no semantic type yet.
///
/// A type bound where it was declared (see [`bind_declared`]) is that
/// semantic type: the names in a type resolve in the scope that declares it
/// (IEEE 1800-2023 6.18, 23.9), which `env` may not see. Where `env` does
/// tell a type, debug builds check it is the same one.
pub(in crate::ast) fn of_type(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
) -> Option<TyId> {
    if let Some(id) = ty.binding.get(table) {
        #[cfg(debug_assertions)]
        if let Some(fresh) = convert(table, functions, ty, env) {
            assert_eq!(
                fresh, id,
                "a type bound where it was declared converts to another type"
            );
        }
        return Some(id);
    }
    unbound(table, functions, ty, env)
}

/// The semantic type of `ty` where `env` may tell its names apart from the
/// way the scope that declares it does: a signal of a generate block, read
/// with the constants of its module, names a function its block imports
/// where the module imports another of that name (IEEE 1800-2023 26.3). A
/// bound type is its binding, which debug builds do not check against `env`
/// (see [`of_type`]).
pub(in crate::ast) fn of_declared(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
) -> Option<TyId> {
    match ty.binding.get(table) {
        Some(id) => Some(id),
        None => unbound(table, functions, ty, env),
    }
}

/// The semantic type of `ty`, which has no binding, in `env`.
fn unbound(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
) -> Option<TyId> {
    #[cfg(test)]
    {
        table.unbound_reads += 1;
    }
    nested_type(table, functions, ty, env)
}

/// Bind `ty` to its semantic type in `env`, the constants of the scope that
/// declares it (see [`of_type`]), unless it is bound already or has none.
/// Each type a declaration nests is bound before the type that holds it, so
/// that converting one never descends further than its own members.
pub(in crate::ast) fn bind_declared(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &mut Type,
    env: &ConstEnv,
) {
    if ty.binding.get(table).is_none()
        && let Some(id) = convert(table, functions, ty, env)
    {
        ty.bind(table, id);
    }
}

/// The semantic type of `ty`, a type that names no constant or type of a
/// scope, such as a built-in one: the same in every scope.
pub(in crate::ast) fn of_builtin(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
) -> Option<TyId> {
    convert(table, functions, ty, &ConstEnv::default())
}

/// `ty`, a type that names no constant or type of a scope, such as a
/// built-in one or one with literal bounds, bound to its semantic type,
/// which is the same in every scope.
pub(in crate::ast) fn bound_builtin(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    mut ty: Type,
) -> Type {
    bind_declared(table, functions, &mut ty, &ConstEnv::default());
    ty
}

/// The semantic type of `ty` nested in a type being converted: its binding
/// if it has one, unchecked, so that the check of [`of_type`] stays one
/// level deep.
fn nested_type(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
) -> Option<TyId> {
    if let Some(id) = ty.binding.get(table) {
        return Some(id);
    }
    convert(table, functions, ty, env)
}

/// The semantic type of `ty` as its fields tell (see [`of_type`]).
fn convert(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
) -> Option<TyId> {
    let parts = Parts {
        bound_element: ty.element.get(table),
        bound_enumeration: ty.enumeration.declaration(),
        container: ty.kept_container.declaration(),
        unpacked_struct: ty.structure.declaration(),
        kind: ty.kind,
        members: &ty.members,
        declaration: ty.declaration_name(),
        typename: ty.spelling.name_of(),
        aggregate: ty.spelling.aggregate_of(),
        scope: ty.declaration_scope(),
        signed: ty.is_signed,
        signed_element_depth: ty.signed_element_depth,
        packed: ty
            .packed_ranges
            .iter()
            .map(|range| (range.left(), range.right()))
            .collect(),
        keyword: ty.spelling.keyword_of(),
        chandle: ty.is_chandle(),
    };
    let unpacked = ty
        .unpacked_ranges
        .iter()
        .map(|range| (range.left(), range.right()));
    parts.intern(table, functions, env, unpacked)
}

/// The semantic type of a variable of `shape`, whose dimensions name
/// constants of `env`, without the integer type keyword that declared it
/// (which only `$typename` tells apart).
pub(in crate::ast) fn of_shape(
    table: &mut TyTable,
    functions: &const_functions::ConstantFunctions,
    shape: &VariableDimensions,
    env: &ConstEnv,
) -> Option<TyId> {
    let kind = if shape.string {
        TypeKind::String
    } else if shape.real {
        match type_names::declared_type(shape).map(Type::kind) {
            Some(TypeKind::Shortreal) => TypeKind::Shortreal,
            _ => TypeKind::Real,
        }
    } else if shape.is_2state {
        TypeKind::Bit
    } else {
        TypeKind::Logic
    };
    let declared = type_names::declared_type(shape);
    let parts = Parts {
        // A shape tells no keyword (see above).
        bound_element: shape
            .element_type(table)
            .map(|element| table.without_keyword(element)),
        bound_enumeration: shape.enumeration_declaration(),
        container: shape.container_declaration(),
        unpacked_struct: shape.structure_declaration(),
        kind,
        members: &shape.members,
        declaration: declared.and_then(Type::declaration_name),
        typename: declared.and_then(|ty| ty.spelling.name_of()),
        aggregate: declared.and_then(|ty| ty.spelling.aggregate_of()),
        scope: declared.and_then(Type::declaration_scope),
        signed: shape.signed,
        signed_element_depth: shape.signed_element_depth,
        packed: shape
            .packed
            .iter()
            .map(|dimension| (&dimension.left, &dimension.right))
            .collect(),
        keyword: None,
        chandle: shape.chandle,
    };
    let unpacked = shape
        .unpacked
        .iter()
        .map(|dimension| (&dimension.left, &dimension.right));
    parts.intern(table, functions, env, unpacked)
}

/// What a type is made of, from a [`Type`] or a [`VariableDimensions`].
struct Parts<'a> {
    /// The semantic type of an element of the unpacked dimensions, where
    /// the type keeps it: the fields below then only cross-check it.
    bound_element: Option<TyId>,
    /// The declaration of the enumerated type the type is or has elements
    /// of.
    bound_enumeration: Option<&'a super::EnumDeclaration>,
    container: Option<&'a Arc<ContainerType>>,
    unpacked_struct: Option<&'a Arc<UnpackedStruct>>,
    kind: TypeKind,
    members: &'a [PackedMember],
    /// The typedef that names an enumeration, structure or union type.
    declaration: Option<&'a str>,
    /// How `$typename` names that typedef.
    typename: Option<&'a type_names::TypedefName>,
    /// `struct` or `union` of a structure or union type other than a soft
    /// or tagged union.
    aggregate: Option<&'static str>,
    /// The elaborated scope that declares the enumeration, structure or
    /// union type, if not a package or compilation unit.
    scope: Option<&'a str>,
    signed: bool,
    signed_element_depth: Option<usize>,
    packed: Vec<(&'a ConstExpr, &'a ConstExpr)>,
    keyword: Option<&'static str>,
    chandle: bool,
}

impl Parts<'_> {
    /// The type of fixed-size unpacked dimensions `unpacked`, slowest
    /// varying first (7.4.2), of this element type.
    fn intern<'b>(
        &self,
        table: &mut TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
        unpacked: impl DoubleEndedIterator<Item = (&'b ConstExpr, &'b ConstExpr)>,
    ) -> Option<TyId> {
        let range = |left: &ConstExpr, right: &ConstExpr| {
            Some(Range {
                left: eval_ast_const_expr(functions, left, env)?,
                right: eval_ast_const_expr(functions, right, env)?,
            })
        };
        let mut id = match self.bound_element {
            Some(element) => {
                // The fields tell no virtual interface or class: only its
                // kept type does (25.9, 8.4).
                #[cfg(debug_assertions)]
                if !matches!(
                    table.data(element),
                    TyData::VirtualInterface { .. } | TyData::Class(_)
                ) && let Some(fields) = self.element_of_fields(table, functions, env, &range)
                {
                    assert_eq!(
                        fields, element,
                        "the element type a type keeps converts to another type"
                    );
                }
                element
            }
            None => self.element_of_fields(table, functions, env, &range)?,
        };
        for (left, right) in unpacked.rev() {
            id = table.intern(TyData::Array {
                kind: ArrayKind::Fixed(range(left, right)?),
                element: id,
            });
        }
        Some(id)
    }

    /// The type of an element of the fixed-size unpacked dimensions, as
    /// the fields tell.
    fn element_of_fields(
        &self,
        table: &mut TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
        range: &dyn Fn(&ConstExpr, &ConstExpr) -> Option<Range>,
    ) -> Option<TyId> {
        let element = self.element(table, functions, env, range)?;
        let id = table.intern(element);
        if let Some(container) = self.container {
            table.record_container(id, container);
        }
        if let Some(aggregate) = self.unpacked_struct
            && table.is_unpacked_struct(id)
        {
            table.record_struct(id, aggregate);
        }
        Some(id)
    }

    /// The type of an element of the fixed-size unpacked dimensions.
    fn element(
        &self,
        table: &mut TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
        range: &dyn Fn(&ConstExpr, &ConstExpr) -> Option<Range>,
    ) -> Option<TyData> {
        if let Some(container) = self.container {
            let kind = match container.layout.kind {
                ContainerKind::Dynamic => ArrayKind::Dynamic,
                // A capacity of `bound + 1` elements, or none (7.10).
                ContainerKind::Queue { capacity } => ArrayKind::Queue {
                    bound: capacity.checked_sub(1),
                },
                ContainerKind::Associative { .. } => ArrayKind::Associative {
                    key: nested_type(table, functions, container.key.as_ref()?, env)?,
                },
            };
            return Some(TyData::Array {
                kind,
                element: nested_type(table, functions, &container.element, env)?,
            });
        }
        if let Some(aggregate) = self.unpacked_struct {
            let members = aggregate
                .members()
                .iter()
                .map(|member| {
                    Some((
                        Arc::<str>::from(member.name.as_str()),
                        nested_type(table, functions, &member.r#type, env)?,
                    ))
                })
                .collect::<Option<Arc<[_]>>>()?;
            return Some(TyData::UnpackedStruct(self.aggregate_decl(
                table,
                aggregate.identity(),
                members,
                (aggregate.naming(), None),
            )));
        }
        if self.chandle {
            return Some(TyData::Chandle);
        }
        let four_state = match self.kind {
            TypeKind::Real => return Some(TyData::Real),
            TypeKind::Shortreal => return Some(TyData::Shortreal),
            TypeKind::String => return Some(TyData::String),
            TypeKind::Object | TypeKind::Process | TypeKind::Event => return None,
            TypeKind::Chandle => return Some(TyData::Chandle),
            TypeKind::Bit => false,
            // `reg` is `logic` (6.11.2), as is an implicit type.
            TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => true,
        };
        // An enumerated type, or a packed array of one, is told apart by
        // its declaration (6.19, 6.22.1 c), which the type keeps.
        let element = if let Some(enumeration) = self.bound_enumeration {
            enumeration.element(table, self.packed.len())
        } else if let Some(member) = self.members.first() {
            let owner = member.owner().clone();
            let declaration = member.declaration().clone();
            let members = self
                .members
                .iter()
                .map(|member| {
                    Some((
                        Arc::<str>::from(member.name()),
                        nested_type(table, functions, member.r#type(), env)?,
                    ))
                })
                .collect::<Option<Arc<[_]>>>()?;
            PackedElement::Aggregate(self.aggregate_decl(
                table,
                &owner,
                members,
                (declaration.naming(), declaration.keyword()),
            ))
        } else {
            PackedElement::Bits
        };
        let dims = self
            .packed
            .iter()
            .map(|&(left, right)| range(left, right))
            .collect::<Option<Box<[_]>>>()?;
        let keyword = self
            .keyword
            .filter(|keyword| keyword_matches(keyword, self.kind, &dims));
        Some(TyData::Packed(Packed {
            element,
            four_state,
            signed: self.signed,
            signed_element_depth: self.signed_element_depth,
            dims,
            keyword,
        }))
    }

    /// The declaration of a structure or union type declared at `owner`:
    /// its typedef, which is a type of its own in each generate scope that
    /// declares it (27.4), or the position of an anonymous declaration,
    /// with the types of its `members`, which tell the specializations of a
    /// parameterized scope apart.
    fn aggregate_decl(
        &self,
        table: &mut TyTable,
        owner: &str,
        members: Arc<[(Arc<str>, TyId)]>,
        (naming, keyword): (&type_names::Naming, Option<&'static str>),
    ) -> Decl {
        // The declaration keeps the spelling that names it. A type may keep
        // the declaration of its members without their spelling, such as a
        // type rebuilt from a shape.
        debug_assert!(
            (self.declaration.is_none()
                && self.typename.is_none()
                && self.scope.is_none()
                && self.aggregate.is_none())
                || (self.declaration == naming.declaration()
                    && self.typename == naming.name_of()
                    && self.scope == naming.scope()
                    && self.aggregate == keyword),
            "the declaration {owner} is named {:?} {:?} {:?} {:?}, but spelled {:?} {:?} {:?} {:?}",
            naming.declaration(),
            naming.name_of(),
            naming.scope(),
            keyword,
            self.declaration,
            self.typename,
            self.scope,
            self.aggregate,
        );
        let decl = table.declare(self.declaration.unwrap_or(owner), self.scope);
        let typename = self.declaration.and(self.typename);
        table.record_spelling(decl, typename, self.aggregate);
        Decl::Aggregate { decl, members }
    }
}

/// Whether an integer type with a predefined width (6.11) still declares
/// a type of kind `kind` and packed dimensions `dims`: it spells the type
/// while the type keeps its one range.
fn keyword_matches(keyword: &str, kind: TypeKind, dims: &[Range]) -> bool {
    let (expected, width) = match keyword {
        "byte" => (TypeKind::Bit, 8),
        "shortint" => (TypeKind::Bit, 16),
        "int" => (TypeKind::Bit, 32),
        "longint" => (TypeKind::Bit, 64),
        "integer" => (TypeKind::Logic, 32),
        "time" => (TypeKind::Logic, 64),
        _ => return false,
    };
    kind == expected
        && dims
            == [Range {
                left: width - 1,
                right: 0,
            }]
}
