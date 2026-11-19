//! A declaration's shape and its resolved semantic type. Mutable shape access
//! invalidates the retained ID; declaration-preserving renaming keeps it.
use super::*;

pub(super) mod kept;
#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct VariableDimensions {
    shape: VariableShape,
    /// The semantic type the declaration was bound to where it is declared
    /// (see [`Self::bind_type`]).
    binding: Option<semantic_types::RetainedType>,
    /// The semantic type of an element of the shape's unpacked dimensions
    /// (IEEE 1800-2023 7.4), which an element selected from the shape keeps
    /// (see [`Type::unpacked_ranges_mut`]).
    element: Option<semantic_types::RetainedType>,
    /// The enumerated type (6.19) the variable is, or whose elements a
    /// packed or unpacked array of it has, which only
    /// [`Self::keep_enumeration`] sets (see [`kept`]).
    enumeration: kept::KeptEnumeration,
    /// The unpacked structure type (7.2) the variable is, or whose elements
    /// a fixed-size unpacked array of it has, which only
    /// [`Self::keep_structure`] sets (see [`kept`]).
    structure: kept::KeptStructure,
}

impl VariableDimensions {
    pub(super) fn new(shape: VariableShape) -> Self {
        Self {
            shape,
            binding: None,
            element: None,
            enumeration: kept::KeptEnumeration::default(),
            structure: kept::KeptStructure::default(),
        }
    }

    /// `shape`, of type `ty`: bound to its semantic type and that of its
    /// elements, as `ty` is.
    pub(super) fn bound_to(shape: VariableShape, ty: &Type) -> Self {
        let mut bound = Self {
            shape,
            binding: ty.binding.retained(),
            element: ty.element.retained(),
            enumeration: kept::KeptEnumeration::default(),
            structure: kept::KeptStructure::default(),
        };
        bound.keep_enumeration(ty.enumeration.binding());
        if let Some(aggregate) = ty.structure.binding() {
            bound.keep_structure(Some(aggregate));
        }
        bound
    }

    /// The shape of an element of this unpacked array, bound to the element
    /// type of its semantic type. Selecting an unpacked dimension keeps the
    /// declaration of its element type, including the nominal structure
    /// identity and the class of a handle element.
    pub(super) fn unpacked_element(&self, table: &semantic_types::TyTable) -> VariableDimensions {
        let mut element = self.derived(VariableShape {
            container: None,
            real: false,
            unpacked: self.shape.unpacked[1..].to_vec(),
            ..self.clone_shape()
        });
        element.keep_enumeration(self.enumeration.binding());
        if let Some(id) = self.bound_element(table) {
            element.bind_to(table, id);
        } else {
            // The elements of the elements are those of the shape.
            element.element = self.element.clone();
        }
        element
    }

    /// The shape of a subarray of this unpacked array with the slowest
    /// varying dimensions `unpacked` (7.4.6), which has its element type.
    pub(super) fn subarray(&self, unpacked: Vec<UnpackedDimension>) -> VariableDimensions {
        let mut subarray = self.derived(VariableShape {
            container: None,
            unpacked,
            ..self.clone_shape()
        });
        // A container's elements are not those of the subarray.
        if self.shape.container.is_none() {
            subarray.element = self.element.clone();
            subarray.keep_enumeration(self.enumeration.binding());
        }
        subarray
    }

    /// The semantic type of an element of the unpacked array this shape is
    /// bound to.
    pub(super) fn bound_element(
        &self,
        table: &semantic_types::TyTable,
    ) -> Option<semantic_types::TyId> {
        match table.data(self.bound(table)?) {
            semantic_types::TyData::Array { element, .. } => Some(*element),
            _ => None,
        }
    }

    /// The shape of member `name`, of type `ty`, of the structure this shape
    /// is (7.2): bound to the member's type in the structure's semantic type
    /// where `ty` has no binding of its own.
    pub(super) fn member_shape(
        &self,
        table: &semantic_types::TyTable,
        name: &str,
        ty: &Type,
    ) -> VariableDimensions {
        let mut member = procedural::dimensions_from_type(ty);
        if member.bound(table).is_none()
            && let Some(id) = self.bound(table).and_then(|id| table.member(id, name))
        {
            member.bind_to(table, id);
        }
        member
    }

    /// Bind the shape, if it has no binding, to its semantic type in `env`,
    /// the constants of the scope that declares it.
    pub(super) fn bind_in(
        &mut self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
    ) {
        if self.bound(table).is_none()
            && let Some(id) = self.type_id(table, functions, env)
        {
            self.bind_to(table, id);
        }
    }

    /// Bind the shape to type `id` of `table`, which it was made to have,
    /// and its elements to the type of an element of its unpacked
    /// dimensions.
    pub(super) fn bind_to(&mut self, table: &semantic_types::TyTable, id: semantic_types::TyId) {
        self.binding = Some(table.retain(id));
        let element = table.unpacked_element(id, self.shape.unpacked.len());
        self.element = element.map(|element| table.retain(element));
        match element.map(|element| (element, table.enumeration(element))) {
            Some((element, Some(false))) => {
                let enumeration = semantic_types::EnumBinding::of(table, element);
                self.keep_enumeration(enumeration.unwrap_or_default());
            }
            Some((_, Some(true))) => {}
            _ => self.enumeration.forget(),
        }
        match table.unpacked_struct(id) {
            Some(aggregate) => self.keep_structure(Some(aggregate.clone())),
            None => self.structure.forget(),
        }
    }

    /// This shape with the enumerated type `enumeration`, which it was made
    /// to have, such as that of the constant whose shape it is.
    pub(super) fn with_enumeration(mut self, enumeration: semantic_types::EnumBinding) -> Self {
        self.keep_enumeration(enumeration);
        self
    }

    /// The enumerated type the variable is, or whose elements it has, as
    /// it can be given to another shape of the same variable.
    pub(super) fn retained_enumeration(&self) -> semantic_types::EnumBinding {
        self.enumeration.binding()
    }

    /// The declaration of the unpacked structure type the variable is, or
    /// whose elements it has, as the variable keeps it.
    pub(super) fn structure_declaration(&self) -> Option<&Arc<unpacked_structs::UnpackedStruct>> {
        self.structure.declaration()
    }

    /// What declares the enumerated type the variable is, or whose
    /// elements it has.
    pub(super) fn enumeration_declaration(&self) -> Option<&semantic_types::EnumDeclaration> {
        self.enumeration.declaration()
    }

    /// The semantic type of an element of the shape's unpacked dimensions,
    /// if it keeps one.
    pub(super) fn element_type(
        &self,
        table: &semantic_types::TyTable,
    ) -> Option<semantic_types::TyId> {
        self.element.as_ref()?.in_table(table)
    }

    /// What tells the element type apart where the shape does not, such
    /// as the class of a handle (see [`semantic_types::Identity`]).
    pub(super) fn element_identity(&self) -> Option<semantic_types::Identity> {
        self.element.as_ref()?.identity()
    }

    pub(super) fn clone_shape(&self) -> VariableShape {
        self.shape.clone()
    }

    /// This shape with the bits of its type but no enumeration: the place
    /// a member of a union is, which takes any value of them (6.19.3).
    pub(super) fn without_enum(&self) -> VariableDimensions {
        self.derived(VariableShape {
            declared: type_names::Declared::default(),
            ..self.clone_shape()
        })
    }

    /// Resolve a declaration only in its own, evaluated parameter environment.
    /// An unresolved range leaves no provisional ID behind.
    pub(super) fn bind_type(
        &mut self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
    ) -> Option<semantic_types::TyId> {
        if let Some(id) = self.bound(table) {
            return Some(id);
        }
        let id = semantic_types::of_shape(table, functions, self, env);
        match id {
            Some(id) => self.bind_to(table, id),
            None => self.binding = None,
        }
        id
    }

    /// The semantic type the declaration was bound to, if it was.
    pub(super) fn bound(&self, table: &semantic_types::TyTable) -> Option<semantic_types::TyId> {
        let id = self.binding.as_ref()?.in_table(table)?;
        debug_assert!(
            self.has_dimensions_of(table, id),
            "a shape is bound to a type with its unpacked dimensions"
        );
        Some(id)
    }

    /// Whether semantic type `id` has the unpacked dimensions of this shape:
    /// one fixed-size array for each of its unpacked ranges, around a
    /// container if the shape is one.
    fn has_dimensions_of(
        &self,
        table: &semantic_types::TyTable,
        mut id: semantic_types::TyId,
    ) -> bool {
        for _ in &self.shape.unpacked {
            match table.data(id) {
                semantic_types::TyData::Array {
                    kind: semantic_types::ArrayKind::Fixed(_),
                    element,
                } => id = *element,
                _ => return false,
            }
        }
        match (&self.shape.container, table.data(id)) {
            // A fixed-size array class property, held in a container, is of
            // the fixed-size array type.
            (Some(container), _) if container.fixed.is_some() => true,
            (Some(_), semantic_types::TyData::Array { kind, .. }) => {
                !matches!(kind, semantic_types::ArrayKind::Fixed(_))
            }
            (Some(_), _) => false,
            (None, semantic_types::TyData::Array { .. }) => false,
            (None, _) => true,
        }
    }

    /// Whether the variable's type is an unpacked structure type, or a
    /// fixed-size array of them, as its semantic type in `table` tells.
    pub(super) fn is_unpacked_struct_in(&self, table: &semantic_types::TyTable) -> bool {
        unpacked_structs::is_struct(
            self.bound(table),
            table,
            self.structure_declaration().is_some(),
        )
    }

    /// The declaration of the variable's unpacked structure type, or of the
    /// elements of a fixed-size array of them, as its semantic type in
    /// `table` tells.
    pub(super) fn unpacked_struct_in<'a>(
        &'a self,
        table: &'a semantic_types::TyTable,
    ) -> Option<&'a Arc<unpacked_structs::UnpackedStruct>> {
        unpacked_structs::declaration_of(self.bound(table), table, self.structure_declaration())
    }

    /// What the variable's type is of an enumeration, as its semantic type
    /// in `table` tells (see [`enums::Enumeration::of`]), or the enumerated
    /// type it keeps while it has none.
    pub(super) fn enumeration_in(&self, table: &semantic_types::TyTable) -> enums::Enumeration {
        match (self.bound(table), self.enumeration.declaration()) {
            (None, Some(declaration)) => enums::Enumeration::kept(declaration, self.packed.len()),
            (id, _) => enums::Enumeration::of(id, table),
        }
    }

    /// The class of a variable of object handle type, or of the elements of
    /// a fixed-size array of them, as its semantic type in `table` tells.
    pub(super) fn class_in(&self, table: &semantic_types::TyTable) -> Option<Arc<str>> {
        debug_assert!(
            [&self.binding, &self.element]
                .into_iter()
                .flatten()
                .all(|retained| !retained.class_outside(table)),
            "a class handle shape is read in a table that does not hold it: {self:?}"
        );
        self.bound(table)
            .or_else(|| self.element_type(table))
            .and_then(|id| table.handle_class(id))
            .cloned()
    }

    /// A bound declaration keeps its type when a caller shadows its parameter
    /// names. Temporary or edited shapes still resolve in the supplied context.
    pub(super) fn type_id(
        &self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
    ) -> Option<semantic_types::TyId> {
        if let Some(id) = self.bound(table) {
            return Some(id);
        }
        // A shape whose bounds are all literals has the same type in every
        // context: a parameter of a structure type is typed once, not at
        // each access of one of its members.
        // Nor does the shape tell a virtual interface from the number that
        // holds it, an enumerated type from its base type or an unpacked
        // structure from its bits: its element type and the enumerated and
        // structure types it keeps do.
        if !self.shape.is_closed()
            || self.enumeration.declaration().is_some()
            || self.structure.declaration().is_some()
            || self
                .element
                .as_ref()
                .is_some_and(|e| e.identity().is_some())
        {
            return semantic_types::of_shape(table, functions, self, env);
        }
        let key = self.shape.closed_hash();
        if let Some(id) = table.closed_shape(key, &self.shape) {
            return Some(id);
        }
        let id = semantic_types::of_shape(table, functions, self, env)?;
        table.insert_closed_shape(key, self.shape.clone(), id);
        Some(id)
    }

    /// Signature publication must observe a newly resolved type even when
    /// the symbolic dimension expressions themselves did not change, and
    /// another element, enumerated or structure type of the same shape
    /// (see the `PartialEq` of this type).
    pub(super) fn same_declaration(&self, other: &Self) -> bool {
        if self != other {
            return false;
        }
        match (self.binding.as_ref(), other.binding.as_ref()) {
            (Some(left), Some(right)) => left.same_node(right),
            (None, None) => true,
            _ => false,
        }
    }

    /// Renaming references preserves the declaration and its evaluated type.
    pub(super) fn renamed(mut self, renamer: &scope::Renamer<'_>) -> Self {
        for dimension in &mut self.shape.packed {
            dimension.left = renamer.const_expr(dimension.left.clone());
            dimension.right = renamer.const_expr(dimension.right.clone());
            dimension.width = renamer.const_expr(dimension.width.clone());
        }
        for dimension in &mut self.shape.unpacked {
            dimension.left = renamer.const_expr(dimension.left.clone());
            dimension.right = renamer.const_expr(dimension.right.clone());
            dimension.width = renamer.const_expr(dimension.width.clone());
        }
        self.shape.members = self
            .shape
            .members
            .into_iter()
            .map(|member| member.with_type(|ty| renamer.r#type(ty)))
            .collect();
        self.shape.container = self
            .shape
            .container
            .map(|container| renamer.container(&container));
        self
    }
}

impl PartialEq for VariableDimensions {
    fn eq(&self, other: &Self) -> bool {
        // Enriching a shape is not a lexical edit. Signature publishers use
        // same_declaration when a newly resolved binding must be published.
        // A virtual interface is told by its element type, and an
        // enumerated type by its declaration (see `Type`).
        self.shape == other.shape
            && self.element.as_ref().and_then(|element| element.identity())
                == other
                    .element
                    .as_ref()
                    .and_then(|element| element.identity())
            && self.enumeration.declaration() == other.enumeration.declaration()
            && self.structure.identity() == other.structure.identity()
    }
}
impl Eq for VariableDimensions {}
impl Deref for VariableDimensions {
    type Target = VariableShape;
    fn deref(&self) -> &Self::Target {
        &self.shape
    }
}
impl DerefMut for VariableDimensions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.binding = None;
        self.element = None;
        self.enumeration.forget();
        self.structure.forget();
        &mut self.shape
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VariableShape {
    pub(super) container: Option<Arc<ContainerType>>,
    /// Whether a scope randomize may write the variable: no container, no
    /// chandle, no net or constant (18.12). Whether its type is an
    /// enumeration whose members are not resolved its semantic type tells
    /// (see [`VariableDimensions::enumeration_in`]).
    pub(super) randomizable: bool,
    pub(super) real: bool,
    /// A `string`: each element is a heap handle, not packed bits.
    pub(super) string: bool,
    pub(super) packed: Vec<PackedDimension>,
    pub(super) unpacked: Vec<UnpackedDimension>,
    pub(super) signed: bool,
    pub(super) is_2state: bool,
    pub(super) members: Vec<packed_structs::PackedMember>,
    /// See [`Type::signed_element_depth`].
    pub(super) signed_element_depth: Option<usize>,
    /// The declared type, for `$typename`.
    pub(super) declared: type_names::Declared,
    /// A `chandle` variable (see [`Type::chandle`]).
    pub(super) chandle: bool,
}

impl VariableShape {
    /// Whether every bound of the shape, and of the types of its members, is
    /// a literal, so that no constant of a context changes its type.
    pub(super) fn is_closed(&self) -> bool {
        // An enumeration is named by its declaration, which a shape of a
        // class specialization shares with the other specializations.
        type_names::Declared::of_shape(self).is_none_or(|ty| ty.enumeration().is_none())
            && self.container.is_none()
            // The width of a dimension is computed from its bounds.
            && self
                .packed
                .iter()
                .all(|dimension| literal(&dimension.left) && literal(&dimension.right))
            && self
                .unpacked
                .iter()
                .all(|dimension| literal(&dimension.left) && literal(&dimension.right))
            && self
                .members
                .iter()
                .all(|member| type_is_closed(member.r#type()))
    }

    /// The hash of the parts of a closed shape that its type depends on.
    pub(super) fn closed_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut state = fxhash::FxHasher::default();
        for dimension in &self.packed {
            (&dimension.left, &dimension.right).hash(&mut state);
        }
        for dimension in &self.unpacked {
            (&dimension.left, &dimension.right).hash(&mut state);
        }
        self.members.hash(&mut state);
        (
            self.real,
            self.string,
            self.signed,
            self.is_2state,
            self.signed_element_depth,
            self.chandle,
        )
            .hash(&mut state);
        self.declared.hash_type(&mut state);
        state.finish()
    }

    /// Whether `other` is the same closed shape, its declared type included.
    pub(super) fn same_closed(&self, other: &Self) -> bool {
        self == other && self.declared.same(&other.declared)
    }
}

/// Whether every bound of `ty`, and of the types of its members, is a
/// literal (see [`VariableShape::is_closed`]).
pub(super) fn type_is_closed(ty: &Type) -> bool {
    ty.enumeration().is_none()
        && ty.container.is_none()
        && ty.unpacked_struct().is_none()
        && ty
            .packed_ranges
            .iter()
            .all(|range| literal(&range.left) && literal(&range.right))
        && ty.unpacked_ranges.iter().all(|range| {
            literal(&range.left) && literal(&range.right) && range.size.as_ref().is_none_or(literal)
        })
        && ty
            .members
            .iter()
            .all(|member| type_is_closed(member.r#type()))
}

fn literal(expr: &ConstExpr) -> bool {
    matches!(expr, ConstExpr::Literal(_))
}
