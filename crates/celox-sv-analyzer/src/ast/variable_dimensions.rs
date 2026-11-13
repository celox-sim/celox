//! A declaration's shape and its resolved semantic type. Mutable shape access
//! invalidates the retained ID; declaration-preserving renaming keeps it.
use super::*;
use std::sync::Mutex;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct VariableDimensions {
    shape: VariableShape,
    binding: Mutex<Option<semantic_types::RetainedType>>,
}

impl VariableDimensions {
    pub(super) fn new(shape: VariableShape) -> Self {
        Self {
            shape,
            binding: Mutex::new(None),
        }
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
        &self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
    ) -> Option<semantic_types::TyId> {
        if let Some(id) = self
            .binding
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|binding| binding.in_table(table))
        {
            return Some(id);
        }
        let id = semantic_types::of_shape(table, functions, self, env);
        *self.binding.lock().unwrap() = id.map(|id| table.retain(id));
        id
    }

    /// A bound declaration keeps its type when a caller shadows its parameter
    /// names. Temporary or edited shapes still resolve in the supplied context.
    pub(super) fn type_id(
        &self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &ConstEnv,
    ) -> Option<semantic_types::TyId> {
        if let Some(id) = self
            .binding
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|binding| binding.in_table(table))
        {
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
        let left = self.binding.lock().unwrap().clone();
        let right = other.binding.lock().unwrap();
        match (left.as_ref(), right.as_ref()) {
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
        self
    }
}

impl Clone for VariableDimensions {
    fn clone(&self) -> Self {
        Self {
            shape: self.shape.clone(),
            binding: Mutex::new(self.binding.lock().unwrap().clone()),
        }
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
        *self.binding.get_mut().unwrap() = None;
        &mut self.shape
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VariableShape {
    pub(super) container: Option<Arc<ContainerType>>,
    pub(super) randomizable: bool,
    pub(super) enum_domain: Option<Arc<enums::EnumDomain>>,
    /// See [`Type::packed_enum`].
    pub(super) packed_enum: bool,
    pub(super) real: bool,
    /// A `string`: each element is a heap handle, not packed bits.
    pub(super) string: bool,
    /// The class of an object handle variable.
    pub(super) class: Option<String>,
    /// The interface of a virtual interface variable.
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
fn type_is_closed(ty: &Type) -> bool {
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
