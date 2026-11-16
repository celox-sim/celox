//! Overrides of virtual methods by classes declared in a module, called
//! through the dispatchers of other scopes (IEEE 1800-2023 8.20).
//!
//! A scope lowers the dispatcher of a virtual method over the classes it
//! sees, so package code that calls the method of an object of a class
//! declared in a module, as UVM's abstract-class BFM pattern does, reaches
//! no override from a scope other than that module. A module whose class
//! overrides a method of a class declared outside it exports each override
//! ([`crate::ir::ExportedOverride`]); expanding a module's instances adds
//! each export to the dispatchers of every scope
//! ([`crate::ir::Module::expand_instances`]).
//!
//! An override reads the items of the module instance its object was
//! created in: such a class holds that instance's id in the hidden
//! property [`INSTANCE_PROPERTY`], which its constructor stores, and the
//! exported override selects the instance by it as a call through a
//! virtual interface selects an interface instance, with the module's
//! items as placeholders the expansion repeats for each instance.

use super::*;

/// The hidden property that holds the id of the module instance an object
/// of a module-local class was created in.
pub const INSTANCE_PROPERTY: &str = "$instance";

/// The handle under which a module denotes its own instances, as an
/// interface handle denotes an interface instance.
pub const MODULE_HANDLE: &str = "$module";

/// The type of [`INSTANCE_PROPERTY`]: an unsigned 32-bit two-state number,
/// the width of an instance id.
pub(super) fn instance_type() -> Type {
    let mut ty = Type::new(TypeKind::Bit);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("31".into()),
        ConstExpr::Literal("0".into()),
    ));
    ty
}

/// Whether a class declared in the scope of `tree` with base `base` needs
/// [`INSTANCE_PROPERTY`]: the scope is a module, and the base is declared
/// outside it and does not hold the property already. Its overrides of the
/// base's virtual methods may then be called from other scopes.
pub(super) fn needs_instance(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    base: Option<&classes::Class>,
) -> bool {
    let Some(base) = base else {
        return false;
    };
    if !tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Module)
    {
        return false;
    }
    if base.properties.iter().any(|property| property.hidden) {
        return false;
    }
    !classes::declared_in_scope(tree, session, &base.name)
}

/// The classes a module declares, for the exports of their overrides (see
/// [`crate::ir::ModuleClasses`]), or `None` for a scope other than a module
/// or a module that declares no class.
pub(super) fn module_classes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    name: &str,
    parameters: &[Parameter],
) -> Option<crate::ir::ModuleClasses> {
    if !tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Module)
    {
        return None;
    }
    let classes = classes::scope_classes(tree, session, env);
    if classes.is_empty() {
        return None;
    }
    let instance_fields = classes
        .iter()
        .filter_map(|class| {
            let property = class.properties.iter().find(|property| property.hidden)?;
            Some((class.name.clone(), property.field))
        })
        .collect();
    // Each specialization of the module is a definition of its own, whose
    // instances an export selects among.
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = fxhash::FxHasher::default();
        format!("{parameters:?}").hash(&mut hasher);
        format!("{name}#{:016x}", hasher.finish())
    };
    Some(crate::ir::ModuleClasses {
        key,
        classes: classes.iter().map(|class| class.name.clone()).collect(),
        instance_fields,
    })
}

/// The signal that holds the id of the module instance, which the
/// constructors of its classes that hold [`INSTANCE_PROPERTY`] store.
pub(super) fn identity_signal() -> Signal {
    Signal {
        name: virtual_interfaces::VirtualInterfaceSignal::Identity {
            handle: MODULE_HANDLE.to_owned(),
        }
        .name(),
        r#type: instance_type(),
        is_net: false,
        unresolved_net: false,
        net_resolution: NetResolution::Wire,
        net_drive: NetDrive::None,
        is_const: false,
        package_variable: None,
        hierarchical: None,
    }
}
