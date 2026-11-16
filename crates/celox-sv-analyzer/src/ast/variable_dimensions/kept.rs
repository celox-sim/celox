//! The enumerated type (IEEE 1800-2023 6.19) a [`Type`] or a
//! [`VariableDimensions`] keeps. Only [`Type::keep_enumeration`],
//! [`Type::reshape_keeping_enumeration`] and
//! [`VariableDimensions::keep_enumeration`] set it, together with the
//! spelling that names it.
use super::*;
use semantic_types::{EnumBinding, EnumDeclaration};

/// The enumerated type a holder keeps. It is no `Clone`: a holder clones
/// it with itself, and another holder takes it only through its own
/// `keep_enumeration`.
#[derive(Debug, Default)]
pub(in crate::ast) struct KeptEnumeration(EnumBinding);

impl KeptEnumeration {
    /// What declares the kept enumerated type.
    pub(in crate::ast) fn declaration(&self) -> Option<&EnumDeclaration> {
        self.0.declaration()
    }

    /// The kept enumerated type, for another holder to keep.
    pub(in crate::ast) fn binding(&self) -> EnumBinding {
        self.0.clone()
    }

    /// Forget the enumerated type, as the shape of its holder changes in a
    /// way that may change its type.
    pub(in crate::ast) fn forget(&mut self) {
        self.0.forget();
    }
}

impl Clone for Type {
    fn clone(&self) -> Self {
        Self {
            shape: self.shape.clone(),
            binding: self.binding.clone(),
            element: self.element.clone(),
            enumeration: KeptEnumeration(self.enumeration.0.clone()),
        }
    }
}

impl Clone for VariableDimensions {
    fn clone(&self) -> Self {
        Self {
            shape: self.shape.clone(),
            binding: self.binding.clone(),
            element: self.element.clone(),
            enumeration: KeptEnumeration(self.enumeration.0.clone()),
        }
    }
}

impl Type {
    /// Keep the enumerated type `enumeration` names, with the spelling that
    /// names it: a packed array of it if the type has more packed
    /// dimensions than it (7.4.1). The spelling does not change the
    /// semantic type, so the type keeps its binding.
    pub(in crate::ast) fn keep_enumeration(&mut self, enumeration: EnumBinding) {
        tell_enumeration(Arc::make_mut(&mut self.shape), enumeration.declaration());
        self.enumeration = KeptEnumeration(enumeration);
    }

    /// Change the shape of the type in a way that keeps the enumerated type
    /// of its elements, such as its packed dimensions (7.4.1): the type
    /// forgets its binding and that of its elements, and its spelling names
    /// the kept enumerated type.
    pub(in crate::ast) fn reshape_keeping_enumeration(
        &mut self,
        change: impl FnOnce(&mut TypeShape),
    ) {
        self.binding.forget();
        self.element.forget();
        let shape = Arc::make_mut(&mut self.shape);
        change(shape);
        tell_enumeration(shape, self.enumeration.0.declaration());
    }
}

/// Make the spelling of `shape` tell the enumerated type `declaration`
/// declares, if any: the typedef that names it, or the scope that declares
/// an anonymous one.
fn tell_enumeration(shape: &mut TypeShape, declaration: Option<&EnumDeclaration>) {
    let Some(declaration) = declaration else {
        return;
    };
    match &declaration.name {
        Some(name) if declaration.named => shape.spelling.name_spelled(
            name.clone(),
            declaration.typename.clone(),
            declaration.scope.clone(),
        ),
        _ => {
            if let Some(scope) = &declaration.scope {
                shape.spelling.declared_in(scope.clone());
            }
        }
    }
}

impl VariableDimensions {
    /// Keep the enumerated type `enumeration` names: a packed array of it if
    /// the shape has more packed dimensions than it (7.4.1). A shape
    /// without a declared type, such as that of a parameter, is given one
    /// that names it, for `$typename` (20.6.1).
    pub(in crate::ast) fn keep_enumeration(&mut self, enumeration: EnumBinding) {
        if enumeration.declaration().is_some()
            && type_names::Declared::of_shape(&self.shape).is_none()
        {
            let mut declared = Type::new(if self.shape.is_2state {
                TypeKind::Bit
            } else {
                TypeKind::Logic
            });
            declared.is_signed = self.shape.signed;
            declared.packed_ranges = self
                .shape
                .packed
                .iter()
                .map(|dimension| PackedRange::new(dimension.left.clone(), dimension.right.clone()))
                .collect();
            declared.unpacked_ranges = self
                .shape
                .unpacked
                .iter()
                .map(|dimension| {
                    UnpackedRange::new(dimension.left.clone(), dimension.right.clone())
                })
                .collect();
            declared.keep_enumeration(enumeration.clone());
            self.shape.declared = type_names::Declared::new(&declared);
        }
        self.enumeration = KeptEnumeration(enumeration);
    }
}
