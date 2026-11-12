//! Types and ranges of the AST: construction, accessors and conversion to IR types.

use super::*;

impl Type {
    pub(super) fn implicit() -> Self {
        Self {
            is_enum: false,
            enum_domain: None,
            packed_enum: false,
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            class: None,
            virtual_interface: None,
            container: None,
            unpacked_struct: None,
            spelling: type_names::Spelling::default(),
        }
    }

    pub(super) fn new(kind: TypeKind) -> Self {
        Self {
            kind,
            is_enum: false,
            enum_domain: None,
            packed_enum: false,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            class: None,
            virtual_interface: None,
            container: None,
            unpacked_struct: None,
            spelling: type_names::Spelling::default(),
        }
    }

    /// Name an enumeration, struct or union type after typedef `name`,
    /// unless an earlier typedef already named it (`typedef A B` names the
    /// type of `A`). `typename` is how `$typename` writes the name.
    pub(super) fn name_declaration(
        &mut self,
        name: impl FnOnce() -> (String, type_names::TypedefName),
    ) {
        if (self.is_enum || !self.members.is_empty() || self.unpacked_struct.is_some())
            && self.spelling.declaration().is_none()
        {
            let (name, typename) = name();
            self.spelling.name(name.into(), typename);
        }
    }

    /// The typedef that names this enumeration, struct or union type.
    pub(super) fn declaration_name(&self) -> Option<&str> {
        self.spelling.declaration()
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

    /// The members of an unpacked structure type.
    pub(in crate::ast) fn unpacked_struct(&self) -> Option<&Arc<unpacked_structs::UnpackedStruct>> {
        self.unpacked_struct.as_ref()
    }

    pub(crate) fn structure_identity(&self) -> Option<&str> {
        self.unpacked_struct.as_ref().map(|ty| ty.identity())
    }

    /// Non-storage aggregate paths, including the root, retained for member typing.
    pub(crate) fn structure_aggregates(&self, base: &str) -> Vec<(String, Self)> {
        self.unpacked_struct
            .as_ref()
            .map_or_else(Vec::new, |_| unpacked_structs::aggregates(base, self))
    }

    pub(crate) fn structure_leaves(&self) -> Vec<(String, Self)> {
        self.unpacked_struct.as_ref().map_or_else(Vec::new, |ty| {
            ty.leaves()
                .into_iter()
                .map(|leaf| (leaf.path.join("."), leaf.r#type))
                .collect()
        })
    }

    /// The class of an object handle type.
    pub fn class(&self) -> Option<&str> {
        self.class.as_deref()
    }

    /// The interface of a virtual interface type.
    pub fn virtual_interface(&self) -> Option<&virtual_interfaces::VirtualInterface> {
        self.virtual_interface.as_deref()
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
        Self {
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
            ..Self::new(kind)
        }
    }

    /// Whether the type is a `chandle`.
    pub(crate) fn is_chandle(&self) -> bool {
        self.kind == TypeKind::Chandle
    }

    /// Whether the type is that of an associative or dynamic array or of a
    /// queue: a heap handle with the type of the container it denotes.
    pub(crate) fn is_container(&self) -> bool {
        self.container.is_some()
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
        for range in &mut resolved.packed_ranges {
            let left = eval(&range.left().clone().into())?;
            let right = eval(&range.right().clone().into())?;
            *range = PackedRange::new(literal(left), literal(right));
        }
        for range in &mut resolved.unpacked_ranges {
            let left = eval(&range.left().clone().into())?;
            let right = eval(&range.right().clone().into())?;
            *range = UnpackedRange::new(literal(left), literal(right));
        }
        if let Some(container) = &self.container {
            resolved.container = Some(Arc::new(ContainerType {
                key: match &container.key {
                    Some(key) => Some(key.with_resolved_container_bounds(eval)?),
                    None => None,
                },
                layout: container.layout,
                element: container.element.with_resolved_container_bounds(eval)?,
                record: container.record.clone(),
            }));
        }
        Some(resolved)
    }

    /// The type, a `chandle` if `chandle` (see [`crate::ir::Type::is_chandle`]):
    /// the bits of a chandle are two-state.
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
    crate::ir::Type::from_ast(
        ty,
        &const_functions::Environment {
            constants: env,
            functions,
        },
    )
    .with_packed_bounds(bounds)
}
