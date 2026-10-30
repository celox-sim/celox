//! Semantic types of the [`Type`]s that binding builds and of the
//! [`VariableDimensions`] of variables.
use super::*;
use crate::ast::{
    AnalysisState, ConstExpr, ContainerKind, ContainerType, Type, TypeKind, VariableDimensions,
    enums::EnumDomain, eval_ast_const_expr, packed_structs::PackedMember, type_names,
    unpacked_structs::UnpackedStruct, virtual_interfaces::VirtualInterface,
};

/// The semantic type of `ty`, whose ranges name constants of `env`; `None`
/// if a range is not constant or the type has no semantic type yet.
pub(in crate::ast) fn of_type(
    table: &TyTable,
    state: &AnalysisState,
    ty: &Type,
    env: &HashMap<String, i128>,
) -> Option<TyId> {
    let parts = Parts {
        container: ty.container.as_deref(),
        class: ty.class.as_deref(),
        virtual_interface: ty.virtual_interface.as_deref(),
        unpacked_struct: ty.unpacked_struct.as_deref(),
        kind: ty.kind,
        enumeration: ty.is_enum || ty.packed_enum,
        packed_enum: ty.packed_enum,
        enum_domain: ty.enum_domain.as_ref(),
        members: &ty.members,
        declaration: ty.declaration_name(),
        signed: ty.is_signed,
        signed_element_depth: ty.signed_element_depth,
        packed: ty
            .packed_ranges
            .iter()
            .map(|range| (range.left(), range.right()))
            .collect(),
        keyword: ty.spelling.keyword_of(),
    };
    let unpacked = ty
        .unpacked_ranges
        .iter()
        .map(|range| (range.left(), range.right()));
    parts.intern(table, state, env, unpacked)
}

/// The semantic type of a variable of `shape`, whose dimensions name
/// constants of `env`, without the integer type keyword that declared it
/// (which only `$typename` tells apart).
pub(in crate::ast) fn of_shape(
    table: &TyTable,
    state: &AnalysisState,
    shape: &VariableDimensions,
    env: &HashMap<String, i128>,
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
    let parts = Parts {
        container: shape.container.as_deref(),
        class: shape.class.as_deref(),
        virtual_interface: shape.virtual_interface.as_deref(),
        unpacked_struct: shape.unpacked_struct.as_deref(),
        kind,
        enumeration: shape.enum_domain.is_some() || shape.packed_enum,
        packed_enum: shape.packed_enum,
        enum_domain: shape.enum_domain.as_ref(),
        members: &shape.members,
        declaration: type_names::declared_type(shape).and_then(Type::declaration_name),
        signed: shape.signed,
        signed_element_depth: shape.signed_element_depth,
        packed: shape
            .packed
            .iter()
            .map(|dimension| (&dimension.left, &dimension.right))
            .collect(),
        keyword: None,
    };
    let unpacked = shape
        .unpacked
        .iter()
        .map(|dimension| (&dimension.left, &dimension.right));
    parts.intern(table, state, env, unpacked)
}

/// What a type is made of, from a [`Type`] or a [`VariableDimensions`].
struct Parts<'a> {
    container: Option<&'a ContainerType>,
    class: Option<&'a str>,
    virtual_interface: Option<&'a VirtualInterface>,
    unpacked_struct: Option<&'a UnpackedStruct>,
    kind: TypeKind,
    /// An enumeration or a packed array of one.
    enumeration: bool,
    packed_enum: bool,
    enum_domain: Option<&'a Arc<EnumDomain>>,
    members: &'a [PackedMember],
    /// The typedef that names an enumeration, structure or union type.
    declaration: Option<&'a str>,
    signed: bool,
    signed_element_depth: Option<usize>,
    packed: Vec<(&'a ConstExpr, &'a ConstExpr)>,
    keyword: Option<&'static str>,
}

impl Parts<'_> {
    /// The type of fixed-size unpacked dimensions `unpacked`, slowest
    /// varying first (7.4.2), of this element type.
    fn intern<'b>(
        &self,
        table: &TyTable,
        state: &AnalysisState,
        env: &HashMap<String, i128>,
        unpacked: impl DoubleEndedIterator<Item = (&'b ConstExpr, &'b ConstExpr)>,
    ) -> Option<TyId> {
        let range = |left: &ConstExpr, right: &ConstExpr| {
            Some(Range {
                left: eval_ast_const_expr(state, left, env)?,
                right: eval_ast_const_expr(state, right, env)?,
            })
        };
        let mut id = table.intern(self.element(table, state, env, &range)?);
        for (left, right) in unpacked.rev() {
            id = table.intern(TyData::Array {
                kind: ArrayKind::Fixed(range(left, right)?),
                element: id,
            });
        }
        Some(id)
    }

    /// The type of an element of the fixed-size unpacked dimensions.
    fn element(
        &self,
        table: &TyTable,
        state: &AnalysisState,
        env: &HashMap<String, i128>,
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
                    key: of_type(table, state, container.key.as_ref()?, env)?,
                },
            };
            return Some(TyData::Array {
                kind,
                element: of_type(table, state, &container.element, env)?,
            });
        }
        // Events and processes are handles of built-in classes.
        if let Some(class) = self.class {
            return Some(TyData::Class(Decl::Class(class.into())));
        }
        if let Some(interface) = self.virtual_interface {
            return Some(TyData::VirtualInterface {
                interface: Decl::Interface(interface.interface.as_str().into()),
                modport: interface.modport.as_deref().map(Into::into),
            });
        }
        if let Some(aggregate) = self.unpacked_struct {
            return Some(TyData::UnpackedStruct(
                self.aggregate_decl(aggregate.identity()),
            ));
        }
        let four_state = match self.kind {
            TypeKind::Real => return Some(TyData::Real),
            TypeKind::Shortreal => return Some(TyData::Shortreal),
            TypeKind::String => return Some(TyData::String),
            TypeKind::Object | TypeKind::Process | TypeKind::Event => return None,
            TypeKind::Bit => false,
            // `reg` is `logic` (6.11.2), as is an implicit type.
            TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => true,
        };
        let element = if self.enumeration {
            PackedElement::Enum {
                decl: match self.declaration {
                    Some(declaration) => Decl::Named(declaration.into()),
                    // A packed array of an enumeration keeps no members.
                    None => Decl::AnonymousEnum(self.enum_domain?.clone()),
                },
                array: self.packed_enum,
            }
        } else if let Some(member) = self.members.first() {
            PackedElement::Aggregate(self.aggregate_decl(member.owner()))
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
    /// declares it (27.4), or the position of an anonymous declaration.
    fn aggregate_decl(&self, owner: &str) -> Decl {
        Decl::Named(self.declaration.unwrap_or(owner).into())
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
