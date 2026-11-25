//! Types and ranges of the AST: construction, accessors and conversion to IR types.

use super::*;

impl Type {
    pub(super) fn implicit() -> Self {
        Self::from(TypeShape {
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            spelling: type_names::Spelling::default(),
        })
    }

    pub(super) fn new(kind: TypeKind) -> Self {
        Self::from(TypeShape {
            kind,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            spelling: type_names::Spelling::default(),
        })
    }

    /// Name an enumeration, struct or union type after typedef `name`,
    /// unless an earlier typedef already named it (`typedef A B` names the
    /// type of `A`). `typename` is how `$typename` writes the name.
    /// `scope` is the elaborated scope that declares the typedef (see
    /// [`types::unit_scope`]). An enumerated type the type keeps is named
    /// too.
    pub(super) fn name_declaration(
        &mut self,
        scope: Option<Arc<str>>,
        name: impl FnOnce() -> (String, type_names::TypedefName),
    ) {
        if (self.enumeration().is_some()
            || !self.members.is_empty()
            || self.unpacked_struct().is_some())
            && self.spelling.declaration().is_none()
        {
            let (name, typename) = name();
            let mut enumeration = self.enumeration.binding();
            enumeration.name(&name, scope.as_ref(), &typename);
            // The declaration of a structure or union keeps the typedef
            // that names it.
            let naming = type_names::Naming::named(
                name.as_str().into(),
                Some(typename.clone()),
                scope.clone(),
            );
            let structure = self
                .structure
                .declaration()
                .map(|structure| Arc::new(structure.named(naming.clone())));
            self.reshape_keeping_declarations(|shape| {
                shape.spelling.name(name.into(), typename, scope);
                packed_structs::name_members(&mut shape.members, &naming);
            });
            self.keep_enumeration(enumeration);
            if structure.is_some() {
                self.keep_structure(structure);
            }
        }
    }

    /// The typedef that names this enumeration, struct or union type.
    pub(super) fn declaration_name(&self) -> Option<&str> {
        self.spelling.declaration()
    }

    /// The elaborated scope that declares this enumeration, struct or
    /// union type, if a module, interface or program does.
    pub(super) fn declaration_scope(&self) -> Option<&str> {
        self.spelling.scope()
    }

    /// Whether the type is a chandle or an unpacked structure with a
    /// chandle member, which no port may have and no randomization fill
    /// (IEEE 1800-2023 6.14).
    pub(in crate::ast) fn holds_chandle(&self) -> bool {
        self.is_chandle()
            || self.unpacked_struct().is_some_and(|aggregate| {
                aggregate
                    .leaves()
                    .iter()
                    .any(|leaf| leaf.r#type.is_chandle())
            })
    }

    /// The declaration of the unpacked structure type the type is, or
    /// whose elements a fixed-size unpacked array of it has (IEEE 1800-2023
    /// 7.2, 7.4.2), as the type keeps it.
    pub(in crate::ast) fn unpacked_struct(&self) -> Option<&Arc<unpacked_structs::UnpackedStruct>> {
        self.structure.declaration()
    }

    /// The container type (IEEE 1800-2023 7.5, 7.10, 7.8) the type is, or
    /// whose elements a fixed-size unpacked array of it has (7.4.2), as the
    /// type keeps it.
    pub(in crate::ast) fn container(&self) -> Option<&Arc<ContainerType>> {
        self.kept_container.declaration()
    }

    pub(crate) fn structure_identity(&self) -> Option<&str> {
        self.unpacked_struct().map(|ty| ty.identity())
    }

    /// Non-storage aggregate paths, including the root, retained for member typing.
    pub(crate) fn structure_aggregates(&self, base: &str) -> Vec<(String, Self)> {
        self.unpacked_struct()
            .map_or_else(Vec::new, |_| unpacked_structs::aggregates(base, self))
    }

    pub(crate) fn structure_leaves(&self) -> Vec<(String, Self)> {
        self.unpacked_struct().map_or_else(Vec::new, |ty| {
            ty.leaves()
                .into_iter()
                .map(|leaf| (leaf.path.join("."), leaf.r#type))
                .collect()
        })
    }

    /// The class of an object handle type, or of the elements of a
    /// fixed-size array of them, as its semantic type in `table` tells.
    pub(in crate::ast) fn class_in(&self, table: &semantic_types::TyTable) -> Option<Arc<str>> {
        debug_assert!(
            !self.binding.class_outside(table) && !self.element.class_outside(table),
            "a class handle type is read in a table that does not hold it: {self:?}"
        );
        let class = self
            .binding
            .get(table)
            .or_else(|| self.element.get(table))
            .and_then(|id| table.handle_class(id));
        class.cloned()
    }

    /// Whether the type is an unpacked structure type, or a fixed-size array
    /// of them, as its semantic type in `table` tells (IEEE 1800-2023 7.2).
    pub(in crate::ast) fn is_unpacked_struct_in(&self, table: &semantic_types::TyTable) -> bool {
        unpacked_structs::is_struct(
            self.binding.get(table),
            table,
            self.unpacked_struct().is_some(),
        )
    }

    /// The declaration of an unpacked structure type, or of the elements of
    /// a fixed-size array of them, as its semantic type in `table` tells.
    pub(in crate::ast) fn unpacked_struct_in<'a>(
        &'a self,
        table: &'a semantic_types::TyTable,
    ) -> Option<&'a Arc<unpacked_structs::UnpackedStruct>> {
        unpacked_structs::declaration_of(self.binding.get(table), table, self.unpacked_struct())
    }

    /// What the type is of an enumeration, as its semantic type in `table`
    /// tells (see [`enums::Enumeration::of`]), or the enumerated type it
    /// keeps while it has none.
    pub(in crate::ast) fn enumeration_in(
        &self,
        table: &semantic_types::TyTable,
    ) -> enums::Enumeration {
        match (self.binding.get(table), self.enumeration.declaration()) {
            (None, Some(declaration)) => {
                enums::Enumeration::kept(declaration, self.packed_ranges.len())
            }
            (id, _) => enums::Enumeration::of(id, table),
        }
    }

    pub fn kind(&self) -> TypeKind {
        self.kind
    }

    pub fn is_signed(&self) -> bool {
        self.is_signed
    }

    pub fn packed_ranges(&self) -> &[PackedRange] {
        &self.packed_ranges
    }

    pub fn unpacked_ranges(&self) -> &[UnpackedRange] {
        &self.unpacked_ranges
    }

    /// How many leading packed dimensions select an element of a signed
    /// named type (IEEE 1800-2023 7.4.1).
    pub fn signed_element_depth(&self) -> Option<usize> {
        self.signed_element_depth
    }

    /// A type with constant `(left, right)` packed and unpacked bounds.
    pub fn resolved(
        kind: TypeKind,
        is_signed: bool,
        signed_element_depth: Option<usize>,
        packed: &[(i128, i128)],
        unpacked: &[(i128, i128)],
    ) -> Self {
        let literal = |value: i128| ConstExpr::Literal(value.to_string());
        Self::from(TypeShape {
            is_signed,
            signed_element_depth,
            packed_ranges: packed
                .iter()
                .map(|&(left, right)| PackedRange::new(literal(left), literal(right)))
                .collect(),
            unpacked_ranges: unpacked
                .iter()
                .map(|&(left, right)| UnpackedRange::new(literal(left), literal(right)))
                .collect(),
            ..(*Self::new(kind).shape).clone()
        })
    }

    /// Whether the type is a `chandle`.
    pub(crate) fn is_chandle(&self) -> bool {
        self.kind == TypeKind::Chandle
    }

    /// Whether the type is that of an associative or dynamic array or of a
    /// queue: a heap handle with the type of the container it denotes.
    pub(crate) fn is_container(&self) -> bool {
        self.container().is_some()
    }

    /// This container type as a scope other than the one that declares it
    /// names it (IEEE 1800-2023 23.6): the bounds of its index and element
    /// types as `eval` gives them in the declaring scope. `None` if one has
    /// no value there.
    pub fn with_resolved_container_bounds(
        &self,
        eval: &dyn Fn(&crate::ir::ConstExpr) -> Option<i128>,
    ) -> Option<Self> {
        let literal = |value: i128| ConstExpr::Literal(value.to_string());
        let mut resolved = self.clone();
        // The bounds take their values in the declaring scope, so the type
        // keeps its semantic type.
        let shape = resolved.shape_keeping_binding();
        for range in &mut shape.packed_ranges {
            let left = eval(&range.left().clone().into())?;
            let right = eval(&range.right().clone().into())?;
            *range = PackedRange::new(literal(left), literal(right));
        }
        for range in &mut shape.unpacked_ranges {
            let left = eval(&range.left().clone().into())?;
            let right = eval(&range.right().clone().into())?;
            *range = UnpackedRange::new(literal(left), literal(right));
        }
        if let Some(container) = self.container() {
            // The type keeps its container, with the bounds resolved.
            resolved.keep_container(Some(Arc::new(ContainerType {
                key: match &container.key {
                    Some(key) => Some(key.with_resolved_container_bounds(eval)?),
                    None => None,
                },
                layout: container.layout.clone(),
                element: container.element.with_resolved_container_bounds(eval)?,
                record: container.record.clone(),
                fixed: container.fixed,
                // The bounds take their values where the type is declared:
                // the container type stays the same.
                binding: container.binding.clone(),
            })));
        }
        Some(resolved)
    }

    /// The type, a `chandle` if `chandle` (see [`crate::ir::Type::is_chandle`]):
    /// the bits of a chandle are two-state.
    /// Whether the type is a packed structure or union, or a packed array of
    /// them.
    pub(crate) fn has_packed_members(&self) -> bool {
        !self.members.is_empty()
    }

    /// The type with the members of the packed structure or union `other`:
    /// a name of a variable of another scope, whose type is resolved there,
    /// selects them (IEEE 1800-2023 23.6).
    pub fn with_members_of(mut self, other: &Type) -> Self {
        self.members = other.members.clone();
        self
    }

    /// Whether the type is an enumeration or a packed array of them.
    pub(crate) fn has_enumeration(&self) -> bool {
        self.enumeration().is_some()
    }

    /// The type with the enumeration of `other`, its named values and its
    /// typedef name: a name of a variable or a subroutine of another scope,
    /// whose type is resolved there, has them (IEEE 1800-2023 6.19, 23.6).
    pub fn with_enumeration_of(mut self, other: &Type) -> Self {
        self.spelling = other.spelling.clone();
        // The type keeps the enumerated type `other` keeps.
        self.keep_enumeration(other.enumeration.binding());
        self
    }

    pub fn with_chandle(mut self, chandle: bool) -> Self {
        if chandle {
            self.kind = TypeKind::Chandle;
        } else if self.kind == TypeKind::Chandle {
            self.kind = TypeKind::Bit;
        }
        self
    }
}

impl PackedRange {
    pub(super) fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self { left, right }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }
}

impl UnpackedRange {
    pub(super) fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self {
            left,
            right,
            size: None,
        }
    }

    pub(super) fn sized(size: ConstExpr) -> Self {
        Self {
            left: ConstExpr::Literal("0".to_string()),
            right: ConstExpr::Binary {
                left: Box::new(size.clone()),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
            size: Some(size),
        }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }

    pub(super) fn size(&self) -> Option<&ConstExpr> {
        self.size.as_ref()
    }
}

/// An [`LValue::Object`] past the statement that assigns it: the body
/// builder lowers each place with its statement, into a load and a store of
/// the object (`BodyBuilder::place_assignment`), so no other pass sees it.
/// `env` with the declared width and signedness of each parameter of
/// `types`, by which the type table evaluates the ranges that name them
/// (IEEE 1800-2023 6.20.2): `~P` of a 4-bit `P` is 4 bits wide.
pub(crate) fn with_parameter_types<'a>(
    env: &ConstEnv,
    types: impl IntoIterator<Item = (&'a str, usize, bool)>,
) -> ConstEnv {
    let mut env = env.clone();
    for (name, width, signed) in types {
        env.insert(dimensions::parameter_width_marker(name), width as i128);
        env.insert(
            dimensions::parameter_signed_marker(name),
            i128::from(signed),
        );
    }
    env
}

/// The IR type of `ty`, whose ranges name constants of `env`, with the
/// bounds of its packed dimensions as the session's type table resolves
/// them (see [`crate::ir::Type::packed_bounds`]).
pub(crate) fn ir_type(
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    ty: Type,
    env: &ConstEnv,
) -> crate::ir::Type {
    let bounds = semantic_types::packed_bounds(&mut session.types, functions, &ty, env);
    ir_type_with_bounds(
        &mut session.types,
        functions,
        ty,
        env,
        bounds,
        semantic_types::of_type,
    )
}

/// [`ir_type`] for a type `env` may not name the way its declaring scope
/// does: a signal of a generate block published with the constants of its
/// module (see [`semantic_types::of_declared`]).
pub(crate) fn declared_ir_type(
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    ty: Type,
    env: &ConstEnv,
) -> crate::ir::Type {
    let bounds = semantic_types::declared_packed_bounds(&mut session.types, functions, &ty, env);
    ir_type_with_bounds(
        &mut session.types,
        functions,
        ty,
        env,
        bounds,
        semantic_types::of_declared,
    )
}

fn ir_type_with_bounds(
    table: &mut semantic_types::TyTable,
    functions: &const_functions::ConstantFunctions,
    ty: Type,
    env: &ConstEnv,
    bounds: Option<Vec<(i128, i128)>>,
    semantic: fn(
        &mut semantic_types::TyTable,
        &const_functions::ConstantFunctions,
        &Type,
        &ConstEnv,
    ) -> Option<semantic_types::TyId>,
) -> crate::ir::Type {
    crate::ir::Type::from_ast(
        ty,
        &const_functions::Environment {
            constants: env,
            functions,
        },
        &mut |ty| {
            let id = semantic(table, functions, ty, env);
            let class = ty.class_in(table).map(|class| class.to_string());
            (class, id)
        },
    )
    .with_packed_bounds(bounds)
}
