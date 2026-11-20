//! Turn interface members captured by modport-imported functions into ports.
//!
//! A modport that imports a function exposes the function, not the interface
//! members its body reads. The analyzer therefore keeps those members out of
//! the module's variables and ports: the child module lists them in
//! `Module::interface_members`, and each instance names the parent variable
//! behind every member in `InstDeclaration::interface_bindings`.
//!
//! Lowering only knows ordinary variables and port connections. Each captured
//! member is read-only inside the child (an imported function cannot write
//! it), so it is exactly an input port driven by the bound parent variable.
//! This pass rewrites the IR into that form before lowering.

use std::sync::Arc;

use veryl_analyzer::ir::{
    Component, Comptime, Declaration, Expression, Factor, InstDeclaration, InstInput, Ir, Module,
    Shape, VarId, VarKind, Variable,
};
use veryl_analyzer::symbol::ClockDomain;

use crate::HashMap;

/// Rewrite every module reachable from `ir` so that captured interface members
/// are input ports and their instance bindings are input connections.
pub fn lower_interface_captures(ir: &mut Ir) {
    if !ir.components.iter().any(component_has_captures) {
        return;
    }
    // Instances of one module share its `Arc<Component>`; rewrite each shared
    // component once so repeated instances do not deep-clone it again.
    let mut rewritten = HashMap::default();
    for component in &mut ir.components {
        if let Component::Module(module) = component {
            lower_module(module, &mut rewritten);
        }
    }
}

fn component_has_captures(component: &Component) -> bool {
    let Component::Module(module) = component else {
        return false;
    };
    !module.interface_members.is_empty()
        || module.declarations.iter().any(|declaration| {
            matches!(declaration, Declaration::Inst(inst)
                if !inst.interface_bindings.is_empty() || component_has_captures(&inst.component))
        })
}

fn lower_module(module: &mut Module, rewritten: &mut HashMap<*const Component, Arc<Component>>) {
    for (id, mut variable) in std::mem::take(&mut module.interface_members) {
        variable.kind = VarKind::Input;
        module.variables.insert(id, variable);
    }
    for declaration in &mut module.declarations {
        if let Declaration::Inst(inst) = declaration {
            lower_instance(&module.variables, inst, rewritten);
        }
    }
}

fn lower_instance(
    parent_variables: &HashMap<VarId, Variable>,
    inst: &mut InstDeclaration,
    rewritten: &mut HashMap<*const Component, Arc<Component>>,
) {
    let key = Arc::as_ptr(&inst.component);
    if let Some(component) = rewritten.get(&key) {
        inst.component = component.clone();
    } else if component_has_captures(&inst.component) {
        let mut component = (*inst.component).clone();
        if let Component::Module(child) = &mut component {
            lower_module(child, rewritten);
        }
        let component = Arc::new(component);
        rewritten.insert(key, component.clone());
        inst.component = component;
    }

    for binding in std::mem::take(&mut inst.interface_bindings) {
        let Some(parent) = parent_variables.get(&binding.parent) else {
            // Analysis reported the unresolved member; keep the instance
            // unchanged so lowering reports the missing child variable.
            continue;
        };
        let mut r#type = parent.r#type.clone();
        r#type.array = Shape::new(
            r#type
                .array
                .iter()
                .skip(binding.index.indices.len())
                .copied()
                .collect(),
        );
        let comptime = Comptime::from_type(r#type, ClockDomain::None, parent.token);
        inst.inputs.push(InstInput {
            id: binding.child,
            expr: Expression::Term(Box::new(Factor::Variable(
                binding.parent,
                binding.index,
                binding.select,
                comptime,
            ))),
        });
    }
}
