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

pub(super) mod constraints;
pub(super) mod foreign;
pub(super) mod specializations;

/// Bit width of a handle, see `celox_design::HANDLE_WIDTH`.
pub(super) const HANDLE_WIDTH: usize = 32;

/// The name of the implicit handle argument of a method.
pub(super) const THIS: &str = "this";

/// A property of a class as laid out in its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Property {
    pub name: String,
    pub owner: String,
    pub constant: bool,
    pub local: bool,
    pub protected: bool,
    pub r#type: Type,
    pub field: ObjectField,
    pub rand: bool,
    pub randc: bool,
    /// The initializer of the declaration, evaluated by the constructor.
    pub initializer: Option<celox_sv_syntax::ExprId>,
}

impl Property {
    pub fn cyclic_domain_size(&self) -> Option<u16> {
        if self.field.handle.is_some() {
            return None;
        }
        if self.r#type.is_enum {
            self.r#type.enum_domain.as_ref().and_then(|domain| {
                let values = &domain.members;
                (1..=256)
                    .contains(&values.len())
                    .then_some(values.len() as u16)
            })
        } else {
            (1..=8)
                .contains(&self.field.width)
                .then(|| 1u16 << self.field.width)
        }
    }
}

/// A property of unpacked structure type, or a member of one that is itself
/// an unpacked structure. It is not laid out: its leaf members are
/// properties named by their member path (see `unpacked_structs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AggregateProperty {
    pub name: String,
    pub owner: String,
    pub local: bool,
    pub protected: bool,
    pub r#type: Type,
    pub initializer: Option<celox_sv_syntax::ExprId>,
    /// How many properties precede it: a later declaration of the same
    /// name hides an earlier one.
    pub position: usize,
}

/// A static property of a class: one variable of the module that declares
/// the class, shared by all of its objects (IEEE 1800-2023 8.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaticProperty {
    pub name: String,
    pub local: bool,
    pub protected: bool,
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
    /// The definition, or the prototype of a pure virtual method, in the
    /// source of the scope that declares the class.
    pub native: celox_sv_syntax::SubroutineId,
    /// The names of its formals, which a class declared in another source
    /// is called with.
    pub formals: Vec<Option<String>>,
    /// A function, not a task.
    pub is_function: bool,
    /// Its return type, resolved in the scope of the class: `None` for a
    /// task, `void` or a constructor, and unknown when it cannot be bound.
    pub result: Option<Option<Type>>,
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
    /// Properties of unpacked structure type of the class and its bases.
    pub aggregates: Vec<AggregateProperty>,
    /// Static properties of the class and its bases.
    pub statics: Vec<StaticProperty>,
    /// Methods visible in the class, overrides replacing inherited ones.
    pub methods: Vec<Method>,
    /// The parameters and typedefs of an interface class, its own and
    /// those it inherits, each with the class that declares it (IEEE
    /// 1800-2023 8.26.3).
    pub declarations: Vec<(String, String)>,
    /// Bytes of an object.
    pub size: u32,
    pub has_constraints: bool,
    pub constraints: Vec<(String, Result<Vec<celox_heap::randomization::Expr>, String>)>,
}

/// An internal unsigned packed argument of a generated method.
pub(super) fn unsigned_type(width: usize) -> Type {
    let mut ty = procedural::int_type();
    ty.is_signed = false;
    ty.packed_ranges = vec![PackedRange::new(
        ConstExpr::Literal((width - 1).to_string()),
        ConstExpr::Literal("0".into()),
    )];
    ty
}

impl Class {
    pub fn selection_type(&self) -> Type {
        unsigned_type(self.properties.len() + 1)
    }

    /// Validate the supported unconstrained subset before binding a call.
    pub fn check_randomize(&self) -> Converted<()> {
        if self.has_constraints {
            self.constraint_program()?;
        }
        for property in &self.properties {
            if property.randc && property.cyclic_domain_size().is_none() {
                return Err(unsupported(
                    "randc requires a 1-8 bit integer or an enum with at most 256 declared values",
                ));
            }
            if (property.rand || property.randc) && property.field.four_state {
                return Err(unsupported("randomization of a 4-state class property"));
            }
            if (property.rand || property.randc)
                && (property.field.handle.is_some()
                    || (property.r#type.is_enum && property.r#type.enum_domain.is_none()))
            {
                return Err(unsupported(
                    "class randomize requires integral rand properties with a supported enum domain",
                ));
            }
        }
        for hook in ["pre_randomize", "post_randomize"] {
            if let Some(method) = self.method(hook) {
                if !method.is_function
                    || !method.formals.is_empty()
                    || method.qualifiers.is_static
                    || method.qualifiers.is_virtual
                {
                    return Err(unsupported(
                        "randomization hooks require non-static, non-virtual functions without arguments",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The property `name`; one the class declares hides one of a base
    /// (IEEE 1800-2023 8.14).
    pub fn property(&self, name: &str) -> Option<&Property> {
        let (index, property) = self
            .properties
            .iter()
            .enumerate()
            .rev()
            .find(|(_, property)| property.name == name)?;
        (!self.hidden(name, index + 1, index)).then_some(property)
    }

    /// The property or member of unpacked structure type `name`.
    pub fn aggregate(&self, name: &str) -> Option<&AggregateProperty> {
        let aggregate = self
            .aggregates
            .iter()
            .rev()
            .find(|aggregate| aggregate.name == name)?;
        (!self.hidden(name, aggregate.position, aggregate.position)).then_some(aggregate)
    }

    /// Whether a later declaration hides the property or member `name`: a
    /// property from position `properties` on, or a property of unpacked
    /// structure type after position `aggregates`, of that name or of a
    /// structure that contains it (`p` for `p.a`).
    fn hidden(&self, name: &str, properties: usize, aggregates: usize) -> bool {
        let paths = name
            .match_indices('.')
            .map(|(end, _)| &name[..end])
            .chain([name])
            .collect::<Vec<_>>();
        self.properties
            .iter()
            .enumerate()
            .any(|(index, property)| index >= properties && paths.contains(&property.name.as_str()))
            || self.aggregates.iter().any(|aggregate| {
                aggregate.position > aggregates && paths.contains(&aggregate.name.as_str())
            })
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

    /// The actuals that `arguments` of a constructor call pass to each
    /// formal of the constructor, by position or by name (IEEE 1800-2023
    /// 13.5.4), `None` for an omitted one, which takes its default; and the
    /// formals in the order their actuals are written, then the rest, which
    /// is the order they are evaluated in.
    pub fn constructor_arguments(
        &self,
        arguments: &[celox_sv_syntax::Argument],
    ) -> Converted<(Vec<Option<celox_sv_syntax::ExprId>>, Vec<usize>)> {
        let names = self
            .method("new")
            .map_or_else(Vec::new, |method| method.formals.clone());
        let mut actuals = vec![None; names.len()];
        let mut order = Vec::new();
        let mut named = false;
        for (index, argument) in arguments.iter().enumerate() {
            let index = match &argument.name {
                Some(name) => {
                    named = true;
                    let name = instances::normalize_identifier(&name.spelling);
                    names
                        .iter()
                        .position(|formal| formal.as_ref() == Some(&name))
                        .ok_or_else(|| {
                            unsupported(format!(
                                "constructor of class `{}` without an argument `{name}`",
                                self.name
                            ))
                        })?
                }
                None if named => {
                    return Err(unsupported("positional argument after named argument"));
                }
                None => index,
            };
            let Some(actual) = actuals.get_mut(index) else {
                return Err(unsupported(format!(
                    "constructor of class `{}` with {} arguments",
                    self.name,
                    arguments.len()
                )));
            };
            // An omitted actual `.x()` binds its formal too.
            if order.contains(&index) {
                return Err(unsupported("constructor argument passed twice"));
            }
            order.push(index);
            *actual = argument.value;
        }
        let unbound = (0..names.len())
            .filter(|index| !order.contains(index))
            .collect::<Vec<_>>();
        order.extend(unbound);
        Ok((actuals, order))
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

/// The declaration of class `name`, or of the class it specializes, if one
/// exists.
fn declaration<'a>(
    tree: &'a AnalysisContext<'_>,
    name: &str,
) -> Option<&'a celox_sv_syntax::ClassDeclaration> {
    let name = specializations::generic(tree, name);
    tree.declarations()
        .classes
        .iter()
        .find(|class| declared_name(tree, class) == name)
}

/// The name of class declaration `declaration`: qualified by the package
/// that declares it, or by `$unit` when it is declared in the compilation
/// unit outside any module, interface, program or package, so that each
/// names one class of the design (IEEE 1800-2023 3.12.1, 26.3).
pub(super) fn declared_name(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> String {
    let name = instances::normalize_identifier(&declaration.name.spelling);
    match enclosing_unit(tree.declarations(), declaration.tokens) {
        Some(unit) if unit.kind == celox_sv_syntax::UnitKind::Package => format!(
            "{}::{name}",
            instances::normalize_identifier(&unit.name.spelling)
        ),
        Some(_) => name,
        None => {
            use std::hash::{Hash, Hasher};
            let mut hasher = fxhash::FxHasher::default();
            tree.compilation_path().hash(&mut hasher);
            format!("{}::{name}@{:016x}", packages::UNIT, hasher.finish())
        }
    }
}

/// The class declarations of the current scope, not those of the scopes
/// nested in it.
pub(super) fn scope_declarations<'a>(
    tree: &'a AnalysisContext<'_>,
) -> impl Iterator<Item = &'a celox_sv_syntax::ClassDeclaration> {
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    native
        .classes
        .iter()
        .filter(move |class| enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope)
}

/// The classes the current scope declares, as they have been laid out, for
/// the scopes that use it (see `scope::ScopeSymbols::classes`).
pub(super) fn scope_classes(tree: &AnalysisContext<'_>, env: &HashMap<String, i128>) -> Vec<Class> {
    let laid_out = tree.state.classes.borrow();
    scope_declarations(tree)
        .flat_map(|declaration| specializations_of(tree, declaration))
        .filter_map(|name| {
            let mut class = Class::clone(laid_out.get(&name)?);
            // The types are fixed in the scope that declares the class,
            // whose names another scope does not see.
            let (env, _) = specializations::scope(tree, &name, env, &HashMap::default());
            let resolve = |ty: &mut Type| {
                if let Some(resolved) = type_parameters::resolve(&tree.state, ty.clone(), &env) {
                    *ty = resolved;
                }
            };
            for property in &mut class.properties {
                resolve(&mut property.r#type);
            }
            for property in &mut class.statics {
                resolve(&mut property.r#type);
            }
            Some(class)
        })
        .collect()
}

/// The classes the current scope declares as types of the scope, by the
/// names it declares them with. A parameterized class is a type only with
/// a specialization.
pub(super) fn scope_class_types(tree: &AnalysisContext<'_>) -> Vec<(String, Type)> {
    scope_declarations(tree)
        .filter(|declaration| declaration.parameter_ports.is_none())
        .map(|declaration| {
            (
                instances::normalize_identifier(&declaration.name.spelling),
                handle(&declared_name(tree, declaration)),
            )
        })
        .collect()
}

/// The class that the unqualified name `name` refers to where the current
/// scope sees it: one the scope declares, or else one the compilation unit
/// declares (IEEE 1800-2023 3.12.1). Classes of packages are seen through
/// imports, as types.
pub(super) fn resolve(tree: &AnalysisContext<'_>, name: &str) -> Option<String> {
    if tree.state.foreign.contains(name) {
        return Some(name.to_owned());
    }
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    let mut unit_class = None;
    let package_scope = tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Package);
    for class in &native.classes {
        if instances::normalize_identifier(&class.name.spelling) != name {
            continue;
        }
        let enclosing = enclosing_unit(native, class.tokens).map(|unit| unit.tokens);
        if enclosing == scope {
            return Some(declared_name(tree, class));
        }
        if enclosing.is_none() && !package_scope {
            unit_class = Some(declared_name(tree, class));
        }
    }
    // A class an import makes visible (IEEE 1800-2023 26.3).
    let imported = scope::imported(&tree.state);
    if let Some(alias) = imported.aliases.get(name) {
        let class = imported
            .type_aliases
            .get(alias)
            .and_then(Type::class)
            .unwrap_or(alias);
        if imported.classes.iter().any(|known| known.name == class)
            || tree.state.foreign.contains(class)
        {
            return Some(class.to_owned());
        }
    }
    unit_class
        .or_else(|| {
            if package_scope {
                return None;
            }
            let alias = format!("{}::{name}", packages::UNIT);
            imported
                .type_aliases
                .get(&alias)
                .and_then(Type::class)
                .map(str::to_owned)
        })
        .or_else(|| (name == "process").then(|| super::processes::CLASS.to_owned()))
}

/// Whether class `name` is declared in the source of the current scope.
pub(super) fn is_declared(tree: &AnalysisContext<'_>, name: &str) -> bool {
    declaration(tree, name).is_some()
}

/// The innermost module, interface, program or package of `native` whose
/// declaration contains `tokens`.
pub(super) fn enclosing_unit(
    native: &celox_sv_syntax::Declarations,
    tokens: celox_sv_syntax::TokenRange,
) -> Option<&celox_sv_syntax::Unit> {
    native
        .all_units()
        .filter(|unit| unit.tokens.start <= tokens.start && tokens.end <= unit.tokens.end)
        .min_by_key(|unit| unit.tokens.end - unit.tokens.start)
}

/// Whether `name` is an interface class (IEEE 1800-2023 8.26).
pub(super) fn is_interface(tree: &AnalysisContext<'_>, name: &str) -> bool {
    declaration(tree, name).is_some_and(|declaration| declaration.interface)
}

/// Whether `name` is a class declared with a parameter port list (IEEE
/// 1800-2023 8.25).
pub(super) fn is_parameterized(tree: &AnalysisContext<'_>, name: &str) -> bool {
    declaration(tree, name).is_some_and(|class| class.parameter_ports.is_some())
        || tree.state.foreign.contains(name)
}

/// The classes and interface classes that body item `item` declares: of a
/// parameterized class, the specializations made of it (IEEE 1800-2023
/// 8.25).
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
        .flat_map(|class| specializations_of(tree, class))
        .collect()
}

/// The specializations of class declaration `declaration`: the class itself
/// when it has no parameter port list.
pub(super) fn specializations_of(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> Vec<String> {
    let name = declared_name(tree, declaration);
    if declaration.parameter_ports.is_some() {
        specializations::of(tree, &name)
    } else {
        vec![name]
    }
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
            out_of_block_definition(tree, declaration, &method_name)
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
    (name == super::processes::CLASS
        || declaration(tree, name).is_some()
        || imported(tree, name).is_some())
    .then(|| handle(name))
}

/// Class `name` as the scope that declares it laid it out, when the current
/// scope uses that scope (see `scope::ScopeSymbols::classes`).
fn imported(tree: &AnalysisContext<'_>, name: &str) -> Option<Rc<Class>> {
    scope::imported(&tree.state)
        .classes
        .iter()
        .find(|class| class.name == name)
        .map(|class| Rc::new(class.clone()))
}

/// The classes whose objects the current scope may see: those it declares
/// and those of the scopes it uses.
pub(super) fn visible_classes(tree: &AnalysisContext<'_>) -> Vec<String> {
    let mut names = scope_declarations(tree)
        .flat_map(|declaration| specializations_of(tree, declaration))
        .collect::<Vec<_>>();
    for class in &scope::imported(&tree.state).classes {
        if !names.contains(&class.name) {
            names.push(class.name.clone());
        }
    }
    names
}

/// The handle type of class `name`.
pub(super) fn handle(name: &str) -> Type {
    let mut ty = Type::new(if name == super::processes::CLASS {
        TypeKind::Process
    } else {
        TypeKind::Object
    });
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal((HANDLE_WIDTH - 1).to_string()),
        ConstExpr::Literal("0".into()),
    ));
    ty.class = Some(Arc::from(name));
    ty
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
    // A class of another scope is laid out in its own scope, where the
    // names it uses are declared.
    let declaration = declaration(tree, name).filter(|declaration| {
        let native = tree.declarations();
        enclosing_unit(native, declaration.tokens).map(|unit| unit.tokens)
            == tree.root_unit().ok().map(|unit| unit.tokens)
    });
    let Some(declaration) = declaration else {
        let class = imported(tree, name).ok_or_else(|| unsupported(format!("class `{name}`")))?;
        tree.state
            .classes
            .borrow_mut()
            .insert(name.to_owned(), class.clone());
        return Ok(class);
    };
    let (env, aliases) = specializations::scope(tree, name, env, aliases);
    let (env, aliases) = (&env, &aliases);
    let native = tree.declarations();
    // The specialization of an interface class named the interface classes
    // it extends where its header is, as a class names its base below.
    let recorded = declaration
        .interface
        .then(|| specializations::lookup(tree, name))
        .flatten()
        .map(|specialization| specialization.bases.clone())
        .filter(|bases| {
            bases.len() == declaration.interfaces.len()
                && bases.iter().all(|base| {
                    self::declaration(tree, base).is_some_and(|declaration| declaration.interface)
                })
        });
    let interfaces = match recorded {
        Some(bases) => bases,
        None => declaration
            .interfaces
            .iter()
            .map(|&interface| interface_name(tree, interface, env, aliases))
            .collect::<Converted<Vec<_>>>()?,
    };
    if declaration.interface {
        return interface_class(tree, name, declaration, interfaces, env, aliases);
    }
    let base = match &declaration.base {
        None => None,
        Some(base) => {
            // The specialization named its base where the class header is,
            // before the declarations it inherits could hide those names.
            let recorded = specializations::lookup(tree, name)
                .and_then(|specialization| specialization.bases.first().cloned());
            let ty = match recorded {
                Some(ty) => ty,
                None => type_binding::procedural_type(base.data_type, native, tree, env, aliases)?
                    .and_then(|ty| ty.class().map(str::to_owned))
                    .ok_or_else(|| unsupported("base class type"))?,
            };
            Some(class(tree, &ty, env, aliases)?)
        }
    };
    let mut properties = base
        .as_ref()
        .map(|base| base.properties.clone())
        .unwrap_or_default();
    let base_properties = properties.len();
    let mut aggregates = base
        .as_ref()
        .map(|base| base.aggregates.clone())
        .unwrap_or_default();
    let mut statics = base
        .as_ref()
        .map(|base| base.statics.clone())
        .unwrap_or_default();
    let mut size = base.as_ref().map_or(0, |base| base.size);
    for property in &declaration.properties {
        if property.declaration.constant && (property.qualifiers.rand || property.qualifiers.randc)
        {
            return Err(unsupported("const random class property"));
        }
        if property.qualifiers.is_static && (property.qualifiers.rand || property.qualifiers.randc)
        {
            return Err(unsupported("static random class property"));
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
            let ty = containers::with_dimensions(
                ty.clone(),
                &variable.dimensions,
                tree,
                &PackedDimensions::new(HashMap::default(), env, aliases),
            )?;
            if !ty.unpacked_ranges.is_empty() {
                return Err(unsupported("class property with unpacked dimensions"));
            }
            if ty.container.is_some() && property.declaration.constant {
                return Err(unsupported("const dynamic container class property"));
            }
            let property_name = instances::normalize_identifier(&variable.name.spelling);
            if let Some(aggregate) = ty.unpacked_struct() {
                if property.qualifiers.is_static {
                    return Err(unsupported(
                        "static class property of unpacked structure type",
                    ));
                }
                if property.qualifiers.rand || property.qualifiers.randc {
                    return Err(unsupported(
                        "random class property of unpacked structure type",
                    ));
                }
                if property.declaration.constant {
                    return Err(unsupported(
                        "const class property of unpacked structure type",
                    ));
                }
                for (aggregate_name, aggregate_type) in
                    unpacked_structs::aggregates(&property_name, &ty)
                {
                    aggregates.push(AggregateProperty {
                        initializer: (aggregate_name == property_name)
                            .then_some(variable.value)
                            .flatten(),
                        name: aggregate_name,
                        owner: name.to_owned(),
                        local: property.qualifiers.local,
                        protected: property.qualifiers.protected,
                        r#type: aggregate_type,
                        position: properties.len(),
                    });
                }
                for leaf in aggregate.leaves() {
                    let field = field(&tree.state, &leaf.r#type, env, size)?;
                    size = field.offset + field.storage();
                    properties.push(Property {
                        name: leaf.name(&property_name),
                        owner: name.to_owned(),
                        constant: false,
                        local: property.qualifiers.local,
                        protected: property.qualifiers.protected,
                        r#type: leaf.r#type,
                        field,
                        rand: false,
                        randc: false,
                        initializer: None,
                    });
                }
                continue;
            }
            if property.qualifiers.is_static {
                statics.retain(|known| known.name != property_name);
                statics.push(StaticProperty {
                    local: property.qualifiers.local,
                    protected: property.qualifiers.protected,
                    variable: Class::subroutine_name(name, &property_name),
                    name: property_name,
                    owner: name.to_owned(),
                    r#type: ty.clone(),
                    initializer: variable.value,
                });
                continue;
            }
            let owner = name.to_owned();
            let name = property_name;
            let field = field(&tree.state, &ty, env, size)?;
            size = field.offset + field.storage();
            properties.push(Property {
                name,
                owner,
                constant: property.declaration.constant,
                local: property.qualifiers.local,
                protected: property.qualifiers.protected,
                r#type: ty.clone(),
                field,
                rand: property.qualifiers.rand,
                randc: property.qualifiers.randc,
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
        if matches!(
            method_name.as_str(),
            "randomize" | "srandom" | "rand_mode" | "constraint_mode"
        ) {
            return Err(unsupported(
                "overriding built-in class randomization methods",
            ));
        }
        // A method overriding a virtual method is virtual (8.20).
        let is_virtual = method.qualifiers.is_virtual
            || methods
                .iter()
                .any(|known| known.name == method_name && known.is_virtual);
        let native_id = if method.qualifiers.is_pure {
            method.subroutine
        } else if subroutine.prototype {
            out_of_block_definition(tree, declaration, &method_name).ok_or_else(|| {
                unsupported(format!(
                    "extern method `{name}::{method_name}` without a body"
                ))
            })?
        } else {
            method.subroutine
        };
        methods.retain(|known| known.name != method_name);
        let (formals, result) = signature(tree, native_id, env, aliases);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: native_id,
            formals,
            is_function: native.subroutine(native_id).kind
                == celox_sv_syntax::SubroutineKind::Function,
            result,
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
                .map(|method| (method.name.clone(), method.result.clone())),
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
                let (Some(implemented), Some(prototype)) = (&method.result, &prototype) else {
                    return Err(unsupported(format!(
                        "return type of interface class method `{method_name}`"
                    )));
                };
                if implemented != prototype {
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
    let mut constraints = base
        .as_ref()
        .map(|base| base.constraints.clone())
        .unwrap_or_default();
    for block in &declaration.constraints {
        let block_name = block.name.spelling.clone();

        let expressions = block
            .expressions
            .as_ref()
            .ok_or_else(|| "unsupported constraint block form".to_owned())
            .and_then(|expressions| {
                expressions
                    .iter()
                    .map(|&id| constraints::bind(id, tree, &properties, env))
                    .collect()
            });
        // Constraint indices survive overrides, including calls via base handles.
        if let Some((_, inherited)) = constraints.iter_mut().find(|(name, _)| *name == block_name) {
            *inherited = expressions;
        } else {
            constraints.push((block_name, expressions));
        }
    }
    let class = Rc::new(Class {
        name: name.to_owned(),
        id: class_id(name),
        constraints,
        has_constraints: declaration.has_constraints
            || base.as_ref().is_some_and(|base| base.has_constraints),
        base: base.map(|base| base.name.clone()),
        interfaces,
        is_abstract: declaration.virtual_class,
        is_interface: false,
        properties,
        base_properties,
        aggregates,
        statics,
        methods,
        declarations: Vec::new(),
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
        .and_then(|ty| ty.class().map(str::to_owned))
        .ok_or_else(|| unsupported("implemented interface class type"))?;
    // An interface class of another scope is known by its layout.
    let interface = match imported(tree, &name) {
        Some(class) => class.is_interface,
        None => declaration(tree, &name).is_some_and(|declaration| declaration.interface),
    };
    if !interface {
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
    // The parameters and typedefs it declares hide inherited ones; those it
    // inherits from one interface class in several ways are one (8.26.6.3).
    let mut declarations = own_declarations(native, declaration)
        .into_iter()
        .map(|own| (own, name.to_owned()))
        .collect::<Vec<_>>();
    let own = declarations.len();
    let mut methods: Vec<Method> = Vec::new();
    for interface in &interfaces {
        let interface = class(tree, interface, env, aliases)?;
        for method in &interface.methods {
            methods.retain(|known| known.name != method.name);
            methods.push(method.clone());
        }
        for (inherited, origin) in &interface.declarations {
            if declarations[..own]
                .iter()
                .any(|(known, _)| known == inherited)
            {
                continue;
            }
            match declarations.iter().find(|(known, _)| known == inherited) {
                Some((_, first)) if first != origin => {
                    return Err(AnalyzerError::ConflictingInterfaceClassDeclaration {
                        class: name.to_owned(),
                        name: inherited.clone(),
                        first: first.clone(),
                        second: origin.clone(),
                    });
                }
                Some(_) => {}
                None => declarations.push((inherited.clone(), origin.clone())),
            }
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
        let (formals, result) = signature(tree, method.subroutine, env, aliases);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: method.subroutine,
            formals,
            is_function: subroutine.kind == celox_sv_syntax::SubroutineKind::Function,
            result,
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
        has_constraints: false,
        constraints: Vec::new(),
        properties: Vec::new(),
        base_properties: 0,
        aggregates: Vec::new(),
        statics: Vec::new(),
        methods,
        declarations,
        size: 1,
    });
    tree.state
        .classes
        .borrow_mut()
        .insert(name.to_owned(), class.clone());
    Ok(class)
}

/// The names of the parameters and typedefs that class `declaration`
/// declares.
fn own_declarations(
    native: &celox_sv_syntax::Declarations,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> Vec<String> {
    let parameters = declaration
        .parameter_ports
        .iter()
        .chain(&declaration.parameters)
        .flat_map(|&id| &native.parameter_declaration(id).parameters)
        .map(|parameter| &parameter.name);
    let aliases = declaration
        .type_aliases
        .iter()
        .map(|&id| &native.type_alias(id).name);
    parameters
        .chain(aliases)
        .map(|name| instances::normalize_identifier(&name.spelling))
        .collect()
}

/// The formal names and the return type of method `id`, see [`Method`].
fn signature(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::SubroutineId,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> (Vec<Option<String>>, Option<Option<Type>>) {
    let formals = tree
        .declarations()
        .subroutine(id)
        .ports
        .iter()
        .flatten()
        .map(|port| {
            port.name
                .as_ref()
                .map(|name| instances::normalize_identifier(&name.spelling))
        })
        .collect();
    let result = return_type(tree, id, env, aliases)
        .ok()
        .map(|ty| ty.and_then(|ty| type_parameters::resolve(&tree.state, ty, env)));
    (formals, result)
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

/// The definition `function ... class::method` outside class declaration
/// `declaration`, in the scope that declares the class (IEEE 1800-2023 8.24).
fn out_of_block_definition(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::ClassDeclaration,
    method: &str,
) -> Option<celox_sv_syntax::SubroutineId> {
    let native = tree.declarations();
    let class = instances::normalize_identifier(&declaration.name.spelling);
    let scope = enclosing_unit(native, declaration.tokens).map(|unit| unit.tokens);
    native.subroutine_ids().find(|&id| {
        let subroutine = native.subroutine(id);
        !subroutine.prototype
            && instances::normalize_identifier(&subroutine.name.spelling) == method
            && matches!(&subroutine.scope[..], [name]
                if instances::normalize_identifier(&name.name.spelling) == class)
            && enclosing_unit(native, subroutine.tokens).map(|unit| unit.tokens) == scope
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
    if ty.unpacked_struct().is_some() {
        return Err(unsupported("class property of unpacked structure type"));
    }
    let (width, signed, handle) = match ty.kind() {
        _ if ty.container.is_some() => (HANDLE_WIDTH, false, Some(HandleProperty::Object)),
        TypeKind::String => (HANDLE_WIDTH, false, Some(HandleProperty::String)),
        TypeKind::Object => (HANDLE_WIDTH, false, Some(HandleProperty::Object)),
        TypeKind::Process => (HANDLE_WIDTH, false, None),
        TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => {
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
    };
    Ok(ObjectField {
        offset: offset.next_multiple_of(8),
        width: width as u32,
        signed,
        handle,
        // A 4-state property keeps its X and Z bits (IEEE 1800-2023 6.3.1).
        four_state: handle.is_none()
            && matches!(
                ty.kind(),
                TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
            ),
    })
}

/// A run-time identifier of class `name`: an FNV-1a hash, never 0, which
/// `null` reads as.
fn class_id(name: &str) -> u32 {
    // Compilation-unit declaration names already contain their source identity.
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
    for name in visible_classes(tree) {
        // A class that cannot be laid out has no objects.
        let Ok(class) = class(tree, &name, env, aliases) else {
            continue;
        };
        if class.is_abstract || class.is_interface {
            continue;
        }
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
