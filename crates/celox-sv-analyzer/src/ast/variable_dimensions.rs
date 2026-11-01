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

    /// Resolve a declaration only in its own, evaluated parameter environment.
    /// An unresolved range leaves no provisional ID behind.
    pub(super) fn bind_type(
        &self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
        env: &HashMap<String, i128>,
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
        env: &HashMap<String, i128>,
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
        semantic_types::of_shape(table, functions, self, env)
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
