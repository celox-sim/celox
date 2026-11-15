//! A declaration's shape and its resolved semantic type. Mutable shape access
//! invalidates the retained ID; declaration-preserving renaming keeps it.
use super::*;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
pub(super) struct VariableDimensions {
    shape: VariableShape,
    /// The semantic type the declaration was bound to where it is declared
    /// (see [`Self::bind_type`]).
    binding: Option<semantic_types::RetainedType>,
}

impl VariableDimensions {
    pub(super) fn new(shape: VariableShape) -> Self {
        Self {
            shape,
            binding: None,
        }
    }

    /// `shape`, of a type bound to `binding`: its semantic type.
    pub(super) fn bound_to(shape: VariableShape, binding: &semantic_types::Binding) -> Self {
        Self {
            shape,
            binding: binding.retained(),
        }
    }

    /// The shape of an element of this unpacked array, bound to the element
    /// type of its semantic type. Selecting an unpacked dimension keeps the
    /// declaration of its element type, including the nominal structure
    /// identity and the class of a handle element.
    pub(super) fn unpacked_element(&self, table: &semantic_types::TyTable) -> VariableDimensions {
        let mut element = VariableDimensions::new(VariableShape {
            container: None,
            real: false,
            unpacked: self.shape.unpacked[1..].to_vec(),
            ..self.clone_shape()
        });
        if let Some(id) = self.bound_element(table) {
            element.bind_to(table, id);
        }
        element
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

    /// Bind the shape to type `id` of `table`, which it was made to have.
    pub(super) fn bind_to(&mut self, table: &semantic_types::TyTable, id: semantic_types::TyId) {
        self.binding = Some(table.retain(id));
    }

    pub(super) fn clone_shape(&self) -> VariableShape {
        self.shape.clone()
    }

    /// This shape with the bits of its type but no enumeration: the place
    /// a member of a union is, which takes any value of them (6.19.3).
    pub(super) fn without_enum(&self) -> VariableDimensions {
        VariableDimensions::new(VariableShape {
            enum_domain: None,
            packed_enum: false,
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
        self.binding = id.map(|id| table.retain(id));
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
        unpacked_structs::is_struct(self.bound(table), table, self.unpacked_struct.is_some())
    }

    /// What the variable's type is of an enumeration, as its semantic type
    /// in `table` tells (see [`enums::Enumeration::of`]).
    pub(super) fn enumeration_in(&self, table: &semantic_types::TyTable) -> enums::Enumeration {
        let fields = enums::Enumeration {
            enumeration: self.enum_domain.is_some()
                || self.packed_enum
                || type_names::declared_type(self).is_some_and(|ty| ty.is_enum),
            packed: self.packed_enum,
            domain: self.enum_domain.clone(),
        };
        enums::Enumeration::of(self.bound(table), table, fields)
    }

    /// The class of a variable of object handle type, or of the elements of
    /// a fixed-size array of them, as its semantic type in `table` tells.
    pub(super) fn class_in(&self, table: &semantic_types::TyTable) -> Option<Arc<str>> {
        let Some(id) = self.bound(table) else {
            debug_assert!(
                self.class.is_none(),
                "a variable of class handle type is told by its semantic type"
            );
            return None;
        };
        let class = table.handle_class(id).cloned();
        debug_assert_eq!(
            class.as_deref(),
            self.class.as_deref(),
            "the semantic type of a variable of handle type"
        );
        class
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
        if !self.shape.is_closed() {
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
    /// the symbolic dimension expressions themselves did not change.
    pub(super) fn same_declaration(&self, other: &Self) -> bool {
        if self.shape != other.shape {
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
        self.shape == other.shape
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
    pub(super) enum_domain: Option<Arc<enums::EnumDomain>>,
    /// See [`Type::packed_enum`].
    pub(super) packed_enum: bool,
    pub(super) real: bool,
    /// A `string`: each element is a heap handle, not packed bits.
    pub(super) string: bool,
    /// The class of an object handle variable.
    pub(super) class: Option<String>,
    /// The interface of a virtual interface variable, which only the
    /// conversion to its semantic type reads (see `TypeShape`).
    pub(super) virtual_interface: Option<Arc<virtual_interfaces::VirtualInterface>>,
    pub(super) packed: Vec<PackedDimension>,
    pub(super) unpacked: Vec<UnpackedDimension>,
    pub(super) signed: bool,
    pub(super) is_2state: bool,
    pub(super) members: Vec<packed_structs::PackedMember>,
    /// See [`Type::signed_element_depth`].
    pub(super) signed_element_depth: Option<usize>,
    /// A variable of unpacked structure type: the names of its members
    /// are variables (see [`unpacked_structs`]).
    pub(super) unpacked_struct: Option<Arc<unpacked_structs::UnpackedStruct>>,
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
        self.enum_domain.is_none()
            && !self.packed_enum
            && type_names::Declared::of_shape(self).is_none_or(|ty| !ty.is_enum)
            && self.container.is_none()
            && self.class.is_none()
            && self.virtual_interface.is_none()
            && self.unpacked_struct.is_none()
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
        self.enum_domain.hash(&mut state);
        (
            self.packed_enum,
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
    !ty.is_enum
        && ty.enum_domain.is_none()
        && !ty.packed_enum
        && ty.container.is_none()
        && ty.class.is_none()
        && ty.unpacked_struct.is_none()
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
