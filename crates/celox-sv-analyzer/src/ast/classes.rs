//! Classes (IEEE 1800-2023 clause 8).
//!
//! A class variable holds a handle of an object in the simulator's heap. The
//! analyzer lays out the properties of each class as bytes of the object,
//! in the encoding of state memory, and rewrites property accesses into
//! [`ObjectOp`] calls that carry that layout. Methods become subroutines whose
//! first argument is the handle `this`; a constructor allocates the object
//! and returns its handle.

use std::rc::Rc;

use super::*;
use crate::system_functions::{HandleProperty, ObjectField, ObjectOp};

/// Bit width of a handle, see `celox_design::HANDLE_WIDTH`.
pub(super) const HANDLE_WIDTH: usize = 32;

/// The name of the implicit handle argument of a method.
pub(super) const THIS: &str = "this";

/// A property of a class as laid out in its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Property {
    pub name: String,
    pub r#type: Type,
    pub field: ObjectField,
    /// The initializer of the declaration, evaluated by the constructor.
    pub initializer: Option<celox_sv_syntax::ExprId>,
}

/// A static property of a class: one variable of the module that declares
/// the class, shared by all of its objects (IEEE 1800-2023 8.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaticProperty {
    pub name: String,
    /// The class that declares the property.
    pub owner: String,
    pub r#type: Type,
    /// The name of the variable that holds the property.
    pub variable: String,
    pub initializer: Option<celox_sv_syntax::ExprId>,
}

/// A method of a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Method {
    pub name: String,
    /// The class that declares the method.
    pub owner: String,
    /// The definition, or the prototype of a pure virtual method.
    pub native: celox_sv_syntax::SubroutineId,
    pub qualifiers: celox_sv_syntax::ClassQualifiers,
    /// Declared `virtual` here or in a base: a call through a handle runs
    /// the override of the object's class (IEEE 1800-2023 8.20).
    pub is_virtual: bool,
}

impl Method {
    /// A pure virtual method, which has no implementation (8.21).
    pub fn is_pure(&self) -> bool {
        self.qualifiers.is_pure
    }
}

/// A class with the layout of its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Class {
    pub name: String,
    /// Identifies the class at run time; stable across modules and runs.
    pub id: u32,
    pub base: Option<String>,
    /// The interface classes the class implements, or the interface class
    /// extends (IEEE 1800-2023 8.26).
    pub interfaces: Vec<String>,
    /// A `virtual class` or an interface class, which cannot be constructed
    /// (IEEE 1800-2023 8.21, 8.26).
    pub is_abstract: bool,
    /// An interface class: it has no properties and its methods are pure
    /// virtual (IEEE 1800-2023 8.26).
    pub is_interface: bool,
    /// Properties of the class and its bases, bases first.
    pub properties: Vec<Property>,
    /// How many of `properties` the bases declare.
    pub base_properties: usize,
    /// Static properties of the class and its bases.
    pub statics: Vec<StaticProperty>,
    /// Methods visible in the class, overrides replacing inherited ones.
    pub methods: Vec<Method>,
    /// Bytes of an object.
    pub size: u32,
}

impl Class {
    /// The property `name`; one the class declares hides one of a base
    /// (IEEE 1800-2023 8.14).
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .rev()
            .find(|property| property.name == name)
    }

    pub fn static_property(&self, name: &str) -> Option<&StaticProperty> {
        self.statics.iter().find(|property| property.name == name)
    }

    pub fn method(&self, name: &str) -> Option<&Method> {
        self.methods.iter().find(|method| method.name == name)
    }

    /// The properties the class itself declares.
    pub fn own_properties(&self) -> &[Property] {
        &self.properties[self.base_properties..]
    }

    /// The number of formals of the constructor, which takes the handle of
    /// the object it initializes before them.
    pub fn constructor_formals(&self, native: &celox_sv_syntax::Declarations) -> usize {
        self.method("new").map_or(0, |method| {
            native
                .subroutine(method.native)
                .ports
                .as_ref()
                .map_or(0, Vec::len)
        })
    }

    /// The subroutine name of `method` of `owner`, and the variable name of
    /// its static property `method`.
    pub fn subroutine_name(owner: &str, method: &str) -> String {
        format!("{owner}::{method}")
    }

    /// The subroutine that calls the override of virtual method `method`
    /// of `owner` for the class of its `this` (IEEE 1800-2023 8.20).
    pub fn dispatcher_name(owner: &str, method: &str) -> String {
        format!("{owner}::{method}$virtual")
    }

    /// The virtual methods the class itself declares, each with a
    /// dispatcher.
    pub fn dispatched_methods(&self) -> impl Iterator<Item = &Method> {
        self.methods
            .iter()
            .filter(|method| method.owner == self.name && method.is_virtual)
    }

    /// Whether the class is `ancestor`, derives from it or implements it,
    /// so that a handle of the class may be assigned to a variable of
    /// `ancestor` (IEEE 1800-2023 8.16, 8.26.5).
    pub fn derives_from(
        &self,
        tree: &AnalysisContext<'_>,
        ancestor: &str,
        env: &HashMap<String, i128>,
        aliases: &HashMap<String, Type>,
    ) -> bool {
        let mut pending = vec![self.name.clone()];
        let mut seen = HashSet::default();
        while let Some(name) = pending.pop() {
            if name == ancestor {
                return true;
            }
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Ok(class) = self::class(tree, &name, env, aliases) {
                pending.extend(class.base.iter().chain(&class.interfaces).cloned());
            }
        }
        false
    }

    /// The subroutine that `$cast` to a variable of `class` calls (IEEE
    /// 1800-2023 8.16).
    pub fn cast_name(class: &str) -> String {
        format!("{class}::$cast")
    }

    /// An allocation of an uninitialized object of the class.
    pub fn allocation(&self) -> Expr {
        let handles = self
            .properties
            .iter()
            .filter_map(|property| {
                property
                    .field
                    .handle
                    .map(|handle| (property.field.offset, handle))
            })
            .collect();
        object_call(
            ObjectOp::New {
                class: self.id,
                size: self.size,
                handles,
            },
            Vec::new(),
        )
    }
}

/// The declaration of class `name`, if one exists.
fn declaration<'a>(
    tree: &'a AnalysisContext<'_>,
    name: &str,
) -> Option<&'a celox_sv_syntax::ClassDeclaration> {
    tree.declarations()
        .classes
        .iter()
        .find(|class| instances::normalize_identifier(&class.name.spelling) == name)
}

/// Whether `name` is an interface class (IEEE 1800-2023 8.26).
pub(super) fn is_interface(tree: &AnalysisContext<'_>, name: &str) -> bool {
    declaration(tree, name).is_some_and(|declaration| declaration.interface)
}

/// The classes and interface classes that body item `item` declares.
pub(super) fn item_classes(
    item: celox_sv_syntax::BodyItemId,
    tree: &AnalysisContext<'_>,
) -> Vec<String> {
    let native = tree.declarations();
    let item = native.body_item(item);
    if !matches!(&item.kind, celox_sv_syntax::BodyItemKind::Unconverted { category, .. } if category == "class")
    {
        return Vec::new();
    }
    native
        .classes
        .iter()
        .filter(|class| {
            item.tokens.start <= class.tokens.start && class.tokens.end <= item.tokens.end
        })
        .map(|class| instances::normalize_identifier(&class.name.spelling))
        .collect()
}

/// The methods that class `name` implements, with their definitions: the
/// method itself or, for an `extern` method, its out-of-block definition.
/// A pure virtual method has none (IEEE 1800-2023 8.21). Each comes with
/// whether it is static.
pub(super) fn declared_methods(
    tree: &AnalysisContext<'_>,
    name: &str,
) -> Converted<Vec<(celox_sv_syntax::SubroutineId, bool)>> {
    let Some(declaration) = declaration(tree, name) else {
        return Ok(Vec::new());
    };
    let native = tree.declarations();
    declaration
        .methods
        .iter()
        .filter(|method| !method.qualifiers.is_pure)
        .map(|method| {
            let subroutine = native.subroutine(method.subroutine);
            if !subroutine.prototype {
                return Ok((method.subroutine, method.qualifiers.is_static));
            }
            let method_name = instances::normalize_identifier(&subroutine.name.spelling);
            out_of_block_definition(tree, name, &method_name)
                .map(|id| (id, method.qualifiers.is_static))
                .ok_or_else(|| {
                    unsupported(format!(
                        "extern method `{name}::{method_name}` without a body"
                    ))
                })
        })
        .collect()
}

/// The static properties that the classes of body item `item` declare,
/// which are variables of the module (IEEE 1800-2023 8.9). A class that
/// cannot be laid out declares none here; its uses report why.
pub(super) fn item_statics(
    item: celox_sv_syntax::BodyItemId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Vec<StaticProperty> {
    item_classes(item, tree)
        .into_iter()
        .filter_map(|name| {
            let class = class(tree, &name, env, aliases).ok()?;
            Some(
                class
                    .statics
                    .iter()
                    .filter(|property| property.owner == name)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}

/// The arguments that the `extends` clause of class `name` passes to the
/// constructor of its base (IEEE 1800-2023 8.17).
pub(super) fn base_arguments<'a>(
    tree: &'a AnalysisContext<'_>,
    name: &str,
) -> Option<&'a [celox_sv_syntax::Argument]> {
    declaration(tree, name)?.base.as_ref()?.arguments.as_deref()
}

/// The handle type of class `name`, if it is a class.
pub(super) fn handle_type(tree: &AnalysisContext<'_>, name: &str) -> Option<Type> {
    declaration(tree, name)?;
    let mut ty = Type::new(TypeKind::Object);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal((HANDLE_WIDTH - 1).to_string()),
        ConstExpr::Literal("0".into()),
    ));
    ty.class = Some(name.to_owned());
    Some(ty)
}

/// The class `name`, laid out with its bases.
pub(super) fn class(
    tree: &AnalysisContext<'_>,
    name: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Rc<Class>> {
    if let Some(class) = tree.state.classes.borrow().get(name) {
        return Ok(class.clone());
    }
    let declaration =
        declaration(tree, name).ok_or_else(|| unsupported(format!("class `{name}`")))?;
    let native = tree.declarations();
    let interfaces = declaration
        .interfaces
        .iter()
        .map(|&interface| interface_name(tree, interface, env, aliases))
        .collect::<Converted<Vec<_>>>()?;
    if declaration.interface {
        return interface_class(tree, name, declaration, interfaces, env, aliases);
    }
    let base = match &declaration.base {
        None => None,
        Some(base) => {
            let ty = type_binding::procedural_type(base.data_type, native, tree, env, aliases)?
                .and_then(|ty| ty.class.clone())
                .ok_or_else(|| unsupported("base class type"))?;
            Some(class(tree, &ty, env, aliases)?)
        }
    };
    let mut properties = base
        .as_ref()
        .map(|base| base.properties.clone())
        .unwrap_or_default();
    let base_properties = properties.len();
    let mut statics = base
        .as_ref()
        .map(|base| base.statics.clone())
        .unwrap_or_default();
    let mut size = base.as_ref().map_or(0, |base| base.size);
    for property in &declaration.properties {
        let ty = type_binding::procedural_type(
            property.declaration.data_type,
            native,
            tree,
            env,
            aliases,
        )?
        .ok_or_else(|| unsupported("class property type"))?;
        for variable in &property.declaration.variables {
            if !variable.dimensions.is_empty() {
                return Err(unsupported("class property with unpacked dimensions"));
            }
            let property_name = instances::normalize_identifier(&variable.name.spelling);
            if property.qualifiers.is_static {
                statics.retain(|known| known.name != property_name);
                statics.push(StaticProperty {
                    variable: Class::subroutine_name(name, &property_name),
                    name: property_name,
                    owner: name.to_owned(),
                    r#type: ty.clone(),
                    initializer: variable.value,
                });
                continue;
            }
            let name = property_name;
            let field = field(&tree.state, &ty, env, size)?;
            size = field.offset + field.bytes();
            properties.push(Property {
                name,
                r#type: ty.clone(),
                field,
                initializer: variable.value,
            });
        }
    }
    // A constructor is not inherited: a class without one has an implicit
    // constructor (IEEE 1800-2023 8.7).
    let mut methods = base
        .as_ref()
        .map(|base| base.methods.clone())
        .unwrap_or_default();
    methods.retain(|method| method.name != "new");
    for method in &declaration.methods {
        let subroutine = native.subroutine(method.subroutine);
        let method_name = instances::normalize_identifier(&subroutine.name.spelling);
        // A method overriding a virtual method is virtual (8.20).
        let is_virtual = method.qualifiers.is_virtual
            || methods
                .iter()
                .any(|known| known.name == method_name && known.is_virtual);
        let native_id = if method.qualifiers.is_pure {
            method.subroutine
        } else if subroutine.prototype {
            out_of_block_definition(tree, name, &method_name).ok_or_else(|| {
                unsupported(format!(
                    "extern method `{name}::{method_name}` without a body"
                ))
            })?
        } else {
            method.subroutine
        };
        methods.retain(|known| known.name != method_name);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: native_id,
            qualifiers: method.qualifiers,
            is_virtual,
        });
    }
    // A method of an implemented interface class is virtual, and the class
    // implements it with the same return type (8.26.2, 8.26.6.1). An
    // abstract class may instead re-declare it pure (8.26.7).
    let mut required = Vec::new();
    for interface in &interfaces {
        required.extend(
            class(tree, interface, env, aliases)?
                .methods
                .iter()
                .map(|method| (method.name.clone(), method.native)),
        );
    }
    for (method_name, prototype) in required {
        match methods.iter_mut().find(|method| method.name == method_name) {
            Some(method) => {
                if !method.is_virtual && method.owner != name {
                    return Err(unsupported(format!(
                        "interface class method `{method_name}` implemented by a non-virtual method of a base class"
                    )));
                }
                if return_type(tree, method.native, env, aliases)?
                    != return_type(tree, prototype, env, aliases)?
                {
                    return Err(unsupported(format!(
                        "method `{method_name}` of class `{name}` whose return type differs from its interface class"
                    )));
                }
                method.is_virtual = true;
            }
            None => {
                return Err(unsupported(format!(
                    "class `{name}` that does not implement interface class method `{method_name}`"
                )));
            }
        }
    }
    // A class that is not abstract implements every pure virtual method
    // (8.21).
    if !declaration.virtual_class
        && let Some(method) = methods.iter().find(|method| method.is_pure())
    {
        return Err(unsupported(format!(
            "class `{name}` that does not implement pure virtual method `{}`",
            method.name
        )));
    }
    let class = Rc::new(Class {
        name: name.to_owned(),
        id: class_id(name),
        base: base.map(|base| base.name.clone()),
        interfaces,
        is_abstract: declaration.virtual_class,
        is_interface: false,
        properties,
        base_properties,
        statics,
        methods,
        size: size.max(1),
    });
    tree.state
        .classes
        .borrow_mut()
        .insert(name.to_owned(), class.clone());
    Ok(class)
}

/// The interface class that type `interface` of an `implements` or an
/// interface class's `extends` list names (IEEE 1800-2023 8.26).
fn interface_name(
    tree: &AnalysisContext<'_>,
    interface: celox_sv_syntax::TypeId,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<String> {
    let native = tree.declarations();
    let name = type_binding::procedural_type(interface, native, tree, env, aliases)?
        .and_then(|ty| ty.class.clone())
        .ok_or_else(|| unsupported("implemented interface class type"))?;
    if !declaration(tree, &name).is_some_and(|declaration| declaration.interface) {
        return Err(unsupported(format!(
            "`implements` of `{name}`, not an interface class"
        )));
    }
    Ok(name)
}

/// Interface class `name`, whose methods are the pure virtual methods it
/// declares and those of the interface classes it extends (IEEE 1800-2023
/// 8.26). A handle of it refers to an object of a class implementing it.
fn interface_class(
    tree: &AnalysisContext<'_>,
    name: &str,
    declaration: &celox_sv_syntax::ClassDeclaration,
    interfaces: Vec<String>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Rc<Class>> {
    let native = tree.declarations();
    let mut methods: Vec<Method> = Vec::new();
    for interface in &interfaces {
        for method in &class(tree, interface, env, aliases)?.methods {
            methods.retain(|known| known.name != method.name);
            methods.push(method.clone());
        }
    }
    for method in &declaration.methods {
        let subroutine = native.subroutine(method.subroutine);
        // A method whose types cannot be resolved, such as one using a
        // type parameter or a typedef of the interface class, leaves the
        // interface class without a layout; it is lowered only when used.
        type_binding::subroutine_formals(
            subroutine,
            tree,
            &PackedDimensions::new(HashMap::default(), env, aliases),
        )?;
        return_type(tree, method.subroutine, env, aliases)?;
        let method_name = instances::normalize_identifier(&subroutine.name.spelling);
        methods.retain(|known| known.name != method_name);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: method.subroutine,
            qualifiers: method.qualifiers,
            is_virtual: true,
        });
    }
    let class = Rc::new(Class {
        name: name.to_owned(),
        id: class_id(name),
        base: None,
        interfaces,
        is_abstract: true,
        is_interface: true,
        properties: Vec::new(),
        base_properties: 0,
        statics: Vec::new(),
        methods,
        size: 1,
    });
    tree.state
        .classes
        .borrow_mut()
        .insert(name.to_owned(), class.clone());
    Ok(class)
}

/// The return type of subroutine `id`, `None` for a task or `void`.
fn return_type(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::SubroutineId,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let native = tree.declarations();
    match native.subroutine(id).return_type {
        Some(ty) => type_binding::procedural_type(ty, native, tree, env, aliases),
        None => Ok(None),
    }
}

/// The definition `function ... class::method` outside class `class`.
fn out_of_block_definition(
    tree: &AnalysisContext<'_>,
    class: &str,
    method: &str,
) -> Option<celox_sv_syntax::SubroutineId> {
    let native = tree.declarations();
    native.subroutine_ids().find(|&id| {
        let subroutine = native.subroutine(id);
        !subroutine.prototype
            && instances::normalize_identifier(&subroutine.name.spelling) == method
            && matches!(&subroutine.scope[..], [scope]
                if instances::normalize_identifier(&scope.name.spelling) == class)
    })
}

/// The layout of a property of type `ty` at the first aligned offset at or
/// after `offset`.
fn field(
    state: &AnalysisState,
    ty: &Type,
    env: &HashMap<String, i128>,
    offset: u32,
) -> Converted<ObjectField> {
    let (width, signed, handle) = match ty.kind() {
        TypeKind::String => (HANDLE_WIDTH, false, Some(HandleProperty::String)),
        TypeKind::Object => (HANDLE_WIDTH, false, Some(HandleProperty::Object)),
        TypeKind::Bit => {
            let shape = casts::expr_type_from_type(state, ty, env)
                .ok_or_else(|| unsupported("class property type"))?;
            if shape.width > 64 {
                return Err(unsupported("class property wider than 64 bits"));
            }
            (shape.width, shape.signed, None)
        }
        TypeKind::Real | TypeKind::Shortreal => {
            return Err(unsupported("real class property"));
        }
        TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => {
            return Err(unsupported("4-state class property"));
        }
    };
    Ok(ObjectField {
        offset: offset.next_multiple_of(8),
        width: width as u32,
        signed,
        handle,
    })
}

/// A run-time identifier of class `name`: an FNV-1a hash, never 0, which
/// `null` reads as.
fn class_id(name: &str) -> u32 {
    let hash = name.bytes().fold(0x811c_9dc5u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    hash.max(1)
}

/// The classes whose objects a handle of class `ancestor` may refer to:
/// those deriving from it or implementing it that are not abstract (IEEE
/// 1800-2023 8.16), laid out.
fn object_classes(
    tree: &AnalysisContext<'_>,
    ancestor: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<Rc<Class>>> {
    let mut classes = Vec::new();
    let mut ids = HashMap::default();
    for declaration in &tree.declarations().classes {
        if declaration.interface || declaration.virtual_class {
            continue;
        }
        let name = instances::normalize_identifier(&declaration.name.spelling);
        // A class that cannot be laid out has no objects.
        let Ok(class) = class(tree, &name, env, aliases) else {
            continue;
        };
        if !class.derives_from(tree, ancestor, env, aliases) {
            continue;
        }
        if let Some(other) = ids.insert(class.id, name.clone()) {
            return Err(unsupported(format!(
                "classes `{other}` and `{name}` with the same run-time identifier"
            )));
        }
        classes.push(class);
    }
    Ok(classes)
}

/// The overrides that dispatcher `Class::dispatcher_name(owner, method)`
/// calls: each implementation of the method with the classes, deriving from
/// or implementing `owner` and not abstract, whose objects run it.
pub(super) fn dispatch_targets(
    tree: &AnalysisContext<'_>,
    owner: &str,
    method: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<(String, Vec<u32>)>> {
    let mut targets: Vec<(String, Vec<u32>)> = Vec::new();
    for class in object_classes(tree, owner, env, aliases)? {
        let Some(implementation) = class.method(method) else {
            continue;
        };
        let target = Class::subroutine_name(&implementation.owner, method);
        match targets.iter_mut().find(|(known, _)| *known == target) {
            Some((_, classes)) => classes.push(class.id),
            None => targets.push((target, vec![class.id])),
        }
    }
    Ok(targets)
}

/// The run-time identifiers of the classes whose objects `$cast` may assign
/// to a variable of class `name` (IEEE 1800-2023 8.16).
pub(super) fn cast_targets(
    tree: &AnalysisContext<'_>,
    name: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<u32>> {
    Ok(object_classes(tree, name, env, aliases)?
        .iter()
        .map(|class| class.id)
        .collect())
}

/// A call of object operation `op`.
pub(super) fn object_call(op: ObjectOp, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::Object(Box::new(op)),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}
