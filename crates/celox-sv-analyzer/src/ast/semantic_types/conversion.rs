//! Semantic types of the [`Type`]s that binding builds.
use super::*;
use crate::ast::{AnalysisState, ConstExpr, ContainerKind, Type, TypeKind, eval_ast_const_expr};

/// The semantic type of `ty`, whose ranges name constants of `env`; `None`
/// if a range is not constant or the type has no semantic type yet.
pub(in crate::ast) fn of_type(
    table: &TyTable,
    state: &AnalysisState,
    ty: &Type,
    env: &HashMap<String, i128>,
) -> Option<TyId> {
    let range = |left: &ConstExpr, right: &ConstExpr| {
        Some(Range {
            left: eval_ast_const_expr(state, left, env)?,
            right: eval_ast_const_expr(state, right, env)?,
        })
    };
    let mut id = table.intern(element(table, state, ty, env, &range)?);
    // The first unpacked dimension varies slowest (7.4.2).
    for unpacked in ty.unpacked_ranges.iter().rev() {
        id = table.intern(TyData::Array {
            kind: ArrayKind::Fixed(range(&unpacked.left, &unpacked.right)?),
            element: id,
        });
    }
    Some(id)
}

/// The type of an element of the fixed-size unpacked dimensions of `ty`.
fn element(
    table: &TyTable,
    state: &AnalysisState,
    ty: &Type,
    env: &HashMap<String, i128>,
    range: &dyn Fn(&ConstExpr, &ConstExpr) -> Option<Range>,
) -> Option<TyData> {
    if let Some(container) = &ty.container {
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
    if let Some(class) = &ty.class {
        return Some(TyData::Class(Decl::Class(class.clone())));
    }
    if let Some(interface) = &ty.virtual_interface {
        return Some(TyData::VirtualInterface {
            interface: Decl::Interface(interface.interface.as_str().into()),
            modport: interface.modport.as_deref().map(Into::into),
        });
    }
    if let Some(aggregate) = &ty.unpacked_struct {
        return Some(TyData::UnpackedStruct(aggregate_decl(
            ty,
            aggregate.identity(),
        )));
    }
    let four_state = match ty.kind {
        TypeKind::Real => return Some(TyData::Real),
        TypeKind::Shortreal => return Some(TyData::Shortreal),
        TypeKind::String => return Some(TyData::String),
        TypeKind::Object | TypeKind::Process | TypeKind::Event => return None,
        TypeKind::Bit => false,
        // `reg` is `logic` (6.11.2), as is an implicit type.
        TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => true,
    };
    let element = if ty.is_enum || ty.packed_enum {
        PackedElement::Enum {
            decl: match ty.declaration_name() {
                Some(declaration) => Decl::Named(declaration.into()),
                // A packed array of an enumeration keeps no members.
                None => Decl::AnonymousEnum(ty.enum_domain.clone()?),
            },
            array: ty.packed_enum,
        }
    } else if let Some(member) = ty.members.first() {
        PackedElement::Aggregate(aggregate_decl(ty, member.owner()))
    } else {
        PackedElement::Bits
    };
    let dims = ty
        .packed_ranges
        .iter()
        .map(|packed| range(packed.left(), packed.right()))
        .collect::<Option<Box<[_]>>>()?;
    let keyword = ty
        .spelling
        .keyword_of()
        .filter(|keyword| keyword_matches(keyword, ty.kind, &dims));
    Some(TyData::Packed(Packed {
        element,
        four_state,
        signed: ty.is_signed,
        signed_element_depth: ty.signed_element_depth,
        dims,
        keyword,
    }))
}

/// The declaration of structure or union type `ty`, declared at `owner`:
/// its typedef, which is a type of its own in each generate scope that
/// declares it (27.4), or the position of an anonymous declaration.
fn aggregate_decl(ty: &Type, owner: &str) -> Decl {
    Decl::Named(match ty.declaration_name() {
        Some(declaration) => declaration.into(),
        None => owner.into(),
    })
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
