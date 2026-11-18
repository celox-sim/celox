//! The enumerated type (IEEE 1800-2023 6.19) a [`Type`] or a
//! [`VariableDimensions`] keeps. Only [`Type::keep_enumeration`],
//! [`Type::reshape_keeping_enumeration`] and
//! [`VariableDimensions::keep_enumeration`] set it, together with the fields
//! that tell the same enumerated type, so that no holder keeps an
//! enumerated type its fields do not tell.
use super::*;
use semantic_types::{EnumBinding, EnumDeclaration};

/// The enumerated type a holder keeps. It is no `Clone`: a holder clones
/// it with itself, whose fields tell it too, and another holder takes it
/// only through its own `keep_enumeration`.
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
    /// way that may change its type. The fields stay: they tell the shape.
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
    /// Keep the enumerated type `enumeration` names, with the fields and
    /// spelling that tell it: a packed array of it if the type has more
    /// packed dimensions than it (7.4.1). The fields only come to agree
    /// with the semantic type, so the type keeps its binding.
    pub(in crate::ast) fn keep_enumeration(&mut self, enumeration: EnumBinding) {
        tell_enumeration(Arc::make_mut(&mut self.shape), enumeration.declaration());
        self.enumeration = KeptEnumeration(enumeration);
    }

    /// Change the shape of the type in a way that keeps the enumerated type
    /// of its elements, such as its packed dimensions (7.4.1): the type
    /// forgets its binding and that of its elements, and its fields tell
    /// the kept enumerated type of the changed shape.
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

/// Make the fields and spelling of `shape` tell the enumerated type
/// `declaration` declares, if any.
fn tell_enumeration(shape: &mut TypeShape, declaration: Option<&EnumDeclaration>) {
    let Some(declaration) = declaration else {
        return;
    };
    let array = shape.packed_ranges.len() > declaration.dims;
    shape.is_enum = true;
    shape.packed_enum = array;
    shape.enum_domain = declaration.domain.clone().filter(|_| !array);
    match &declaration.name {
        Some(name) if declaration.named => shape.spelling.name_spelled(
            name.clone(),
            declaration.typename.clone(),
            declaration.scope.clone(),
        ),
        Some(position) => shape
            .spelling
            .anonymous_enum(position.clone(), declaration.scope.clone()),
        None => {}
    }
}

impl VariableDimensions {
    /// Keep the enumerated type `enumeration` names, with the fields that
    /// tell it: a packed array of it if the shape has more packed
    /// dimensions than it (7.4.1). The declared type of the variable tells
    /// which enumerated type it is, whatever unpacked dimensions are
    /// selected from it; a shape without one is given one that does.
    pub(in crate::ast) fn keep_enumeration(&mut self, enumeration: EnumBinding) {
        if let Some(declaration) = enumeration.declaration() {
            let array = self.shape.packed.len() > declaration.dims;
            self.shape.packed_enum = array;
            self.shape.enum_domain = declaration.domain.clone().filter(|_| !array);
            if type_names::Declared::of_shape(&self.shape).is_none() {
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
                    .map(|dimension| {
                        PackedRange::new(dimension.left.clone(), dimension.right.clone())
                    })
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
        }
        self.enumeration = KeptEnumeration(enumeration);
    }
}
