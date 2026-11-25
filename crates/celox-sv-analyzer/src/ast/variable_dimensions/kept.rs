//! The enumerated type (IEEE 1800-2023 6.19) and the unpacked structure
//! type (7.2) a [`Type`] or a [`VariableDimensions`] keeps. Only
//! [`Type::keep_enumeration`], [`Type::reshape_keeping_enumeration`] and
//! [`VariableDimensions::keep_enumeration`] set the enumerated type,
//! together with the spelling that names it, and only
//! [`Type::keep_structure`] and [`VariableDimensions::keep_structure`] the
//! structure. The container type (7.5, 7.10, 7.8) only
//! [`Type::keep_container`] and [`VariableDimensions::keep_container`] set.
use super::*;
use semantic_types::{EnumBinding, EnumDeclaration};
use unpacked_structs::UnpackedStruct;

/// The container type, a dynamic array, queue or associative array (7.5,
/// 7.10, 7.8), a holder is, or whose elements a fixed-size unpacked array
/// of it has (7.4.2). It is no `Clone`: a holder clones it with itself, and
/// another holder takes it only through its own `keep_container`.
#[derive(Debug, Default)]
pub(in crate::ast) struct KeptContainer(Option<Arc<ContainerType>>);

impl KeptContainer {
    /// The kept container type.
    pub(in crate::ast) fn declaration(&self) -> Option<&Arc<ContainerType>> {
        self.0.as_ref()
    }

    /// The kept container type, for another holder to keep.
    pub(in crate::ast) fn binding(&self) -> Option<Arc<ContainerType>> {
        self.0.clone()
    }

    /// Forget the container type, as the shape of its holder changes in a
    /// way that may change its type.
    pub(in crate::ast) fn forget(&mut self) {
        self.0 = None;
    }
}

/// The declaration of the unpacked structure type (7.2) a holder is, or
/// whose elements a fixed-size unpacked array of it has (7.4.2). It is no
/// `Clone`: a holder clones it with itself, and another holder takes it
/// only through its own `keep_structure`.
#[derive(Debug, Default)]
pub(in crate::ast) struct KeptStructure(Option<Arc<UnpackedStruct>>);

impl KeptStructure {
    /// The declaration of the kept structure type.
    pub(in crate::ast) fn declaration(&self) -> Option<&Arc<UnpackedStruct>> {
        self.0.as_ref()
    }

    /// The kept structure type, for another holder to keep.
    pub(in crate::ast) fn binding(&self) -> Option<Arc<UnpackedStruct>> {
        self.0.clone()
    }

    /// The identity of the kept structure type (6.22.1), which tells two
    /// types of the same shape apart.
    pub(in crate::ast) fn identity(&self) -> Option<&str> {
        self.0.as_deref().map(UnpackedStruct::identity)
    }

    /// Forget the structure type, as the shape of its holder changes in a
    /// way that may change its type.
    pub(in crate::ast) fn forget(&mut self) {
        self.0 = None;
    }
}

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
            structure: KeptStructure(self.structure.0.clone()),
            kept_container: KeptContainer(self.kept_container.0.clone()),
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
            structure: KeptStructure(self.structure.0.clone()),
            kept_container: KeptContainer(self.kept_container.0.clone()),
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

impl Type {
    /// Change the shape of the type in a way that keeps the enumerated and
    /// unpacked structure types it keeps, such as its spelling or the
    /// storage members of a structure: the type forgets its binding and
    /// that of its elements.
    pub(in crate::ast) fn reshape_keeping_declarations(
        &mut self,
        change: impl FnOnce(&mut TypeShape),
    ) {
        self.binding.forget();
        self.element.forget();
        let shape = Arc::make_mut(&mut self.shape);
        change(shape);
    }

    /// Make the type the bits of the unpacked structure type it is (7.2),
    /// such as the flat value of an aggregate store: it forgets its binding
    /// and that of its elements, and the structure.
    pub(in crate::ast) fn forget_structure(&mut self) {
        self.binding.forget();
        self.element.forget();
        self.structure.forget();
    }

    /// Keep the unpacked structure type `aggregate` declares (7.2), or none:
    /// the type is it or a fixed-size unpacked array of it (7.4.2).
    pub(in crate::ast) fn keep_structure(&mut self, aggregate: Option<Arc<UnpackedStruct>>) {
        self.structure = KeptStructure(aggregate);
    }

    /// Keep the container type `container`, or none: the type is it or a
    /// fixed-size unpacked array of it (7.4.2).
    pub(in crate::ast) fn keep_container(&mut self, container: Option<Arc<ContainerType>>) {
        self.kept_container = KeptContainer(container);
    }
}

impl VariableDimensions {
    /// The unpacked dimensions of the shape, for a change that keeps the
    /// type of its elements, such as an element or a slice selected from it
    /// (7.4.6): the shape forgets its binding, but not the type of its
    /// elements or the enumerated and structure types they are.
    pub(in crate::ast) fn unpacked_mut(&mut self) -> &mut Vec<UnpackedDimension> {
        self.binding = None;
        &mut self.shape.unpacked
    }

    /// A shape made of `shape`, a changed copy of this one, such as that of
    /// an element or a subarray (7.4.6): it keeps the structure this shape
    /// keeps.
    pub(in crate::ast) fn derived(&self, shape: VariableShape) -> VariableDimensions {
        let mut derived = VariableDimensions::new(shape);
        derived.keep_structure(self.structure.binding());
        derived
    }

    /// [`Self::derived`] for a shape of the same container type as this
    /// one, such as the place of a member of a union: it keeps the
    /// container this shape keeps too.
    pub(in crate::ast) fn derived_keeping_container(
        &self,
        shape: VariableShape,
    ) -> VariableDimensions {
        let mut derived = self.derived(shape);
        derived.keep_container(self.kept_container.binding());
        derived
    }

    /// Keep the unpacked structure type `aggregate` declares (7.2), or none,
    /// as [`Type::keep_structure`] does.
    pub(in crate::ast) fn keep_structure(&mut self, aggregate: Option<Arc<UnpackedStruct>>) {
        self.structure = KeptStructure(aggregate);
    }

    /// Keep the container type `container`, or none, as
    /// [`Type::keep_container`] does.
    pub(in crate::ast) fn keep_container(&mut self, container: Option<Arc<ContainerType>>) {
        self.kept_container = KeptContainer(container);
    }
}
