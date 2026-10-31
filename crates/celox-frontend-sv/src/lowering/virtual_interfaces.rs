//! Virtual interfaces (IEEE 1800-2023 25.9) across the elaborated design.
//!
//! The analyzer represents a virtual interface value as the id of an
//! interface instance and a member access through it as a selection among
//! signals that denote the member of each instance the value may denote (see
//! `celox_sv_analyzer::ast::virtual_interfaces`). This module provides what
//! the analyzer cannot know: the ids of the instances of the elaborated
//! design, the binding of those
//! signals to the instances' members, and the id each module instance's
//! interface instances and ports denote.

use celox_design::ModuleId;
use celox_frontend_core::symbolic::artifact::InstanceValue;
use celox_sv_analyzer as sv;
use fxhash::FxHashMap as HashMap;
use num_bigint::BigUint;

use super::hierarchy::{ElaboratedModule, HierarchicalReference};
use super::{LoweredSvInstance, LoweredSvModule, LoweredSvModuleKey};

/// An interface instance of the elaborated design.
struct Instance {
    /// The instance of the module that holds it.
    path: Vec<(String, usize)>,
    module: ModuleId,
    interface: String,
    /// The signal of the module that holds each member.
    members: Vec<(String, String)>,
}

/// The interface instances of the elaborated design, numbered from 1 in
/// hierarchy order, and what the interface instances and ports of each
/// module instance denote.
pub(super) struct Instances {
    instances: Vec<Instance>,
    /// The instances of each specialization.
    paths: HashMap<ModuleId, Vec<Vec<(String, usize)>>>,
    /// The id that each interface handle of each module instance denotes.
    identities: HashMap<(Vec<(String, usize)>, String), u32>,
}

impl Instances {
    /// Number the interface instances below `root`, whose interfaces
    /// `declarations` lists.
    pub(super) fn elaborate(
        root: ModuleId,
        module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
        elaborated: &HashMap<ModuleId, ElaboratedModule>,
        declarations: &sv::VirtualInterfaces,
    ) -> Self {
        let mut found = Self {
            instances: Vec::new(),
            paths: HashMap::default(),
            identities: HashMap::default(),
        };
        // Parents come before their children, which follow the ports of
        // their interface handles to them.
        type Parent<'a> = Option<(Vec<(String, usize)>, ModuleId, &'a LoweredSvInstance)>;
        let mut pending: std::collections::VecDeque<(ModuleId, Vec<(String, usize)>, Parent<'_>)> =
            std::collections::VecDeque::from([(root, Vec::new(), None)]);
        while let Some((module_id, path, parent)) = pending.pop_front() {
            let Some(module) = elaborated.get(&module_id) else {
                continue;
            };
            found.paths.entry(module_id).or_default().push(path.clone());
            for handle in &module.interface_handles {
                if !declarations.interfaces.contains_key(&handle.interface) {
                    continue;
                }
                let id = if handle.port {
                    let Some((parent_path, parent_module, instance)) = &parent else {
                        continue;
                    };
                    match found.actual(parent_path, &elaborated[parent_module], instance, handle) {
                        Some(id) => id,
                        None => continue,
                    }
                } else {
                    found.instances.push(Instance {
                        path: path.clone(),
                        module: module_id,
                        interface: handle.interface.clone(),
                        members: handle.members.clone(),
                    });
                    found.instances.len() as u32
                };
                found
                    .identities
                    .insert((path.clone(), handle.handle.clone()), id);
            }
            let mut positions: HashMap<&str, usize> = HashMap::default();
            for instance in module.instances() {
                // Glue blocks, and so hierarchy indices, follow listing order.
                let position = positions.entry(&instance.instance_name).or_default();
                let index = instance.array_index_base + *position;
                *position += 1;
                let Some(&child) = module_ids.get(&LoweredSvModuleKey::instance_key(instance))
                else {
                    continue;
                };
                let mut child_path = path.clone();
                child_path.push((instance.instance_name.clone(), index));
                pending.push_back((child, child_path, Some((path.clone(), module_id, instance))));
            }
        }
        found
    }

    /// The id of the instance that the interface port `handle` of a child
    /// `instance` of the module instance `path` denotes: the one that the
    /// actuals of its members belong to.
    fn actual(
        &self,
        path: &[(String, usize)],
        parent: &ElaboratedModule,
        instance: &LoweredSvInstance,
        handle: &sv::ir::InterfaceHandle,
    ) -> Option<u32> {
        handle.members.iter().find_map(|(member, formal)| {
            let connection = instance
                .port_connections
                .iter()
                .find(|connection| &connection.formal == formal)?;
            let actual = match &connection.actual_expr {
                Some(sv::ir::Expr::Ident(name)) => name.as_str(),
                Some(_) => return None,
                None => connection.actual.as_str(),
            };
            let actual = parent.interface_handles.iter().find(|candidate| {
                candidate.interface == handle.interface
                    && candidate
                        .members
                        .iter()
                        .any(|(name, signal)| name == member && signal == actual)
            })?;
            self.identities
                .get(&(path.to_vec(), actual.handle.clone()))
                .copied()
        })
    }

    /// `declarations` with the ids of the instances of each interface.
    pub(super) fn table(&self, declarations: &sv::VirtualInterfaces) -> sv::VirtualInterfaces {
        let mut table = declarations.clone();
        for declaration in table.interfaces.values_mut() {
            declaration.instances.clear();
        }
        for (index, instance) in self.instances.iter().enumerate() {
            if let Some(declaration) = table.interfaces.get_mut(&instance.interface) {
                declaration.instances.push(index as u32 + 1);
            }
        }
        table.elaborated = true;
        table
    }

    /// The references of the signals of `module` that denote a member of an
    /// interface instance.
    pub(super) fn references(
        &self,
        module: &sv::ir::Module,
        declarations: &sv::VirtualInterfaces,
    ) -> Vec<HierarchicalReference> {
        module
            .signals()
            .iter()
            .filter_map(|signal| {
                let sv::VirtualInterfaceSignal::Member { id, member } =
                    sv::VirtualInterfaceSignal::of(signal.name())?
                else {
                    return None;
                };
                let instance = self
                    .instances
                    .get(usize::try_from(id).ok()?.checked_sub(1)?)?;
                let (_, target) = instance.members.iter().find(|(name, _)| *name == member)?;
                let declared = declarations.interfaces[&instance.interface]
                    .members
                    .iter()
                    .find(|declared| declared.name == member)?;
                Some(HierarchicalReference::rooted(
                    sv::ast::HierarchicalSignal {
                        paths: Vec::new(),
                        name: signal.name().to_string(),
                        r#type: declared.r#type.clone(),
                        is_net: declared.is_net,
                        local: false,
                    },
                    instance.path.clone(),
                    vec![(instance.module, target.clone())],
                ))
            })
            .collect()
    }

    /// The ids that the identity signals of specialization `module_id`
    /// hold in each of its instances.
    pub(super) fn values(
        &self,
        module_id: ModuleId,
        lowered: &LoweredSvModule,
    ) -> Vec<InstanceValue> {
        let mut values = Vec::new();
        for (name, &variable) in &lowered.signal_names {
            let Some(sv::VirtualInterfaceSignal::Identity { handle }) =
                sv::VirtualInterfaceSignal::of(name)
            else {
                continue;
            };
            for path in self.paths.get(&module_id).into_iter().flatten() {
                if let Some(&id) = self.identities.get(&(path.clone(), handle.clone())) {
                    values.push(InstanceValue {
                        path: path.clone(),
                        variable,
                        value: BigUint::from(id),
                    });
                }
            }
        }
        values
    }
}
