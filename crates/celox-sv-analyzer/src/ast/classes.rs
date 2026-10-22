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

/// A method of a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Method {
    pub name: String,
    /// The class that declares the method.
    pub owner: String,
    pub native: celox_sv_syntax::SubroutineId,
    pub qualifiers: celox_sv_syntax::ClassQualifiers,
}

/// A class with the layout of its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Class {
    pub name: String,
    /// Identifies the class at run time; stable across modules and runs.
    pub id: u32,
    pub base: Option<String>,
    /// Properties of the class and its bases, bases first.
    pub properties: Vec<Property>,
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

    pub fn method(&self, name: &str) -> Option<&Method> {
        self.methods.iter().find(|method| method.name == name)
    }

    /// The subroutine name of `method` of `owner`.
    pub fn subroutine_name(owner: &str, method: &str) -> String {
        format!("{owner}::{method}")
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

/// The classes that body item `item` declares, except interface classes,
/// which have no implementation (IEEE 1800-2023 8.26).
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
            !class.interface
                && item.tokens.start <= class.tokens.start
                && class.tokens.end <= item.tokens.end
        })
        .map(|class| instances::normalize_identifier(&class.name.spelling))
        .collect()
}

/// The methods that class `name` implements, with their definitions: the
/// method itself or, for an `extern` method, its out-of-block definition.
/// A pure virtual method has none (IEEE 1800-2023 8.21).
pub(super) fn declared_methods(
    tree: &AnalysisContext<'_>,
    name: &str,
) -> Converted<Vec<celox_sv_syntax::SubroutineId>> {
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
                return Ok(method.subroutine);
            }
            let method_name = instances::normalize_identifier(&subroutine.name.spelling);
            out_of_block_definition(tree, name, &method_name).ok_or_else(|| {
                unsupported(format!(
                    "extern method `{name}::{method_name}` without a body"
                ))
            })
        })
        .collect()
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
    if declaration.interface {
        return Err(unsupported("interface class"));
    }
    let native = tree.declarations();
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
    let mut size = base.as_ref().map_or(0, |base| base.size);
    for property in &declaration.properties {
        if property.qualifiers.is_static {
            return Err(unsupported("static class property"));
        }
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
            let name = instances::normalize_identifier(&variable.name.spelling);
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
    let mut methods = base
        .as_ref()
        .map(|base| base.methods.clone())
        .unwrap_or_default();
    for method in &declaration.methods {
        let subroutine = native.subroutine(method.subroutine);
        let method_name = instances::normalize_identifier(&subroutine.name.spelling);
        // A pure virtual method has no implementation to call; calls of
        // virtual methods are rejected where they are bound.
        if method.qualifiers.is_pure {
            continue;
        }
        let native_id = if subroutine.prototype {
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
        });
    }
    let class = Rc::new(Class {
        name: name.to_owned(),
        id: class_id(name),
        base: base.map(|base| base.name.clone()),
        properties,
        methods,
        size: size.max(1),
    });
    tree.state
        .classes
        .borrow_mut()
        .insert(name.to_owned(), class.clone());
    Ok(class)
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

/// A call of object operation `op`.
pub(super) fn object_call(op: ObjectOp, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::Object(Box::new(op)),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}
