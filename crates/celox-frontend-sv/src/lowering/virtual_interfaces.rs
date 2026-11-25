//! Virtual interfaces (IEEE 1800-2023 25.9) across the elaborated design,
//! and the module instances that the exported overrides of module classes
//! select among (see `sv::ir::ExportedOverride`).
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

use super::hierarchy::{ElaboratedModule, HierarchicalReference, OutwardObject};
use super::{LoweredSvInstance, LoweredSvModule, LoweredSvModuleKey};

/// An interface instance of the elaborated design.
struct Instance {
    /// The instance of the module that holds it.
    path: Vec<(String, usize)>,
    /// Its handle there, as the module spells it (empty for a module
    /// instance whose classes' overrides are numbered).
    handle: String,
    module: ModuleId,
    interface: String,
    /// The signal of the module that holds each member.
    members: Vec<(String, String)>,
    /// The objects that the hierarchical names of the interface's tasks and
    /// functions denote in this instance, as members (see
    /// [`Definitions::outward`]): each by the path from the top of the
    /// instance that holds it and its name in each of its specializations.
    outward: Vec<(String, Vec<(String, usize)>, Vec<(ModuleId, String)>)>,
}

/// The definitions whose instances are numbered: the interfaces virtual
/// interfaces name, and the module specializations whose classes' exported
/// overrides read their items, each with its members.
pub(super) struct Definitions<'a> {
    pub(super) interfaces: Option<&'a sv::VirtualInterfaces>,
    pub(super) modules: HashMap<String, Vec<sv::ast::virtual_interfaces::VirtualInterfaceMember>>,
    /// The objects that the hierarchical names of the tasks and functions
    /// of each interface instance, by the module that holds it and its
    /// handle, denote, as the members of the interface they become.
    pub(super) outward: HashMap<(ModuleId, String), Vec<(String, OutwardObject)>>,
}

impl Definitions<'_> {
    fn contains(&self, definition: &str) -> bool {
        self.interfaces
            .is_some_and(|interfaces| interfaces.interfaces.contains_key(definition))
            || self.modules.contains_key(definition)
    }

    fn member(
        &self,
        definition: &str,
        member: &str,
    ) -> Option<&sv::ast::virtual_interfaces::VirtualInterfaceMember> {
        let members = match self
            .interfaces
            .and_then(|interfaces| interfaces.interfaces.get(definition))
        {
            Some(declaration) => &declaration.members,
            None => self.modules.get(definition)?,
        };
        members.iter().find(|declared| declared.name == member)
    }
}

/// The interface instances of the elaborated design, and the instances of
/// the modules of [`Definitions`], numbered from 1 in hierarchy order, and
/// what the interface instances and ports of each module instance denote.
pub(super) struct Instances {
    instances: Vec<Instance>,
    /// The instances of each specialization.
    paths: HashMap<ModuleId, Vec<Vec<(String, usize)>>>,
    /// The id that each interface handle of each module instance denotes.
    identities: HashMap<(Vec<(String, usize)>, String), u32>,
}

impl Instances {
    /// Number the instances below `root` of the `definitions`.
    pub(super) fn elaborate(
        root: ModuleId,
        module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
        elaborated: &HashMap<ModuleId, ElaboratedModule>,
        definitions: &Definitions<'_>,
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
                if !definitions.contains(&handle.interface) {
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
                    let outward = definitions
                        .outward
                        .get(&(module_id, handle.handle.clone()))
                        .into_iter()
                        .flatten()
                        .filter_map(|(member, object)| {
                            Some((
                                member.clone(),
                                object.rooted(&path)?,
                                object.targets.clone(),
                            ))
                        })
                        .collect();
                    found.instances.push(Instance {
                        path: path.clone(),
                        handle: handle.handle.clone(),
                        module: module_id,
                        interface: handle.interface.clone(),
                        members: handle.members.clone(),
                        outward,
                    });
                    found.instances.len() as u32
                };
                found
                    .identities
                    .insert((path.clone(), handle.handle.clone()), id);
            }
            let mut positions: HashMap<&str, usize> = HashMap::default();
            for instance in module.instances() {
                // Hierarchy indices follow listing order.
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
    /// `instance` of the module instance `path` denotes.
    fn actual(
        &self,
        path: &[(String, usize)],
        parent: &ElaboratedModule,
        instance: &LoweredSvInstance,
        handle: &sv::ir::InterfaceHandle,
    ) -> Option<u32> {
        actual_handle(
            path,
            &parent.interface_handles,
            instance,
            handle,
            &self.identities,
        )
        .copied()
    }

    /// What the homes of the design's sites denote
    /// ([`celox_design::SiteHomes`]): each numbered instance, and the one
    /// each interface handle of each module instance denotes.
    /// What [`celox_design::SiteHome::Instance`] denotes: the module
    /// instance that holds each instance and its handle there, in id order.
    pub(super) fn site_homes(&self) -> Vec<(Vec<(String, usize)>, String)> {
        self.instances
            .iter()
            .map(|instance| {
                // A module instance numbered for its classes' overrides is
                // the module instance itself.
                let handle = if instance.handle == sv::ir::MODULE_HANDLE {
                    String::new()
                } else {
                    instance.handle.clone()
                };
                (instance.path.clone(), handle)
            })
            .collect()
    }

    /// The ids of the instances of each interface, in id order, which the
    /// analyzed modules' virtual interface accesses are expanded for (see
    /// `sv::ir::Module::expand_virtual_interface_instances`).
    pub(super) fn ids(&self) -> HashMap<sv::ast::virtual_interfaces::DefinitionId, Vec<u32>> {
        let mut ids: HashMap<sv::ast::virtual_interfaces::DefinitionId, Vec<u32>> =
            HashMap::default();
        for (index, instance) in self.instances.iter().enumerate() {
            ids.entry(sv::ast::virtual_interfaces::DefinitionId::of(
                &instance.interface,
            ))
            .or_default()
            .push(index as u32 + 1);
        }
        ids
    }

    /// The references of the signals of `module` that denote a member of an
    /// instance, with the types the analyses of `design` read.
    pub(super) fn references(
        &self,
        module: &sv::ir::Module,
        definitions: &Definitions<'_>,
        design: &mut sv::Design,
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
                let declared = definitions.member(&instance.interface, &member)?;
                // An object a hierarchical name of a task or function of the
                // interface denotes in this instance.
                if let Some((_, path, targets)) =
                    instance.outward.iter().find(|(name, ..)| *name == member)
                {
                    return Some(HierarchicalReference::rooted(
                        sv::ast::HierarchicalSignal {
                            paths: Vec::new(),
                            name: signal.name().to_string(),
                            r#type: declared.declared_type(design),
                            is_net: declared.is_net,
                            local: false,
                            interface_instance: None,
                            aliased: None,
                            constant: None,
                        },
                        path.clone(),
                        targets.clone(),
                    ));
                }
                let (_, target) = instance.members.iter().find(|(name, _)| *name == member)?;
                Some(HierarchicalReference::rooted(
                    sv::ast::HierarchicalSignal {
                        paths: Vec::new(),
                        name: signal.name().to_string(),
                        r#type: declared.declared_type(design),
                        is_net: declared.is_net,
                        local: false,
                        interface_instance: None,
                        aliased: None,
                        constant: None,
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

/// What `table` records for the handle among `parent`, those of module
/// instance `path`, that the interface port `handle` of its child `instance`
/// denotes: the one that the actuals of the port's members belong to.
fn actual_handle<'t, T>(
    path: &[(String, usize)],
    parent: &[sv::ir::InterfaceHandle],
    instance: &LoweredSvInstance,
    handle: &sv::ir::InterfaceHandle,
    table: &'t HashMap<(Vec<(String, usize)>, String), T>,
) -> Option<&'t T> {
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
        let actual = parent.iter().find(|candidate| {
            candidate.interface == handle.interface
                && candidate
                    .members
                    .iter()
                    .any(|(name, signal)| name == member && signal == actual)
        })?;
        table.get(&(path.to_vec(), actual.handle.clone()))
    })
}

/// The interface instance, as the module instance that holds it and its
/// handle there, that each interface port of each module instance below
/// `root` denotes (see [`celox_design::SiteHomes::ports`]).
#[allow(clippy::type_complexity)]
pub(super) fn port_homes(
    root: ModuleId,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    elaborated: &HashMap<ModuleId, ElaboratedModule>,
) -> Vec<(
    (Vec<(String, usize)>, String),
    (Vec<(String, usize)>, String),
)> {
    // Only an interface port denotes another instance.
    if !elaborated
        .values()
        .any(|module| module.home_handles.iter().any(|handle| handle.port))
    {
        return Vec::new();
    }
    let mut homes: HashMap<(Vec<(String, usize)>, String), (Vec<(String, usize)>, String)> =
        HashMap::default();
    // Parents come before their children, which follow the ports of their
    // interface handles to them. Each entry carries the modules above it: a
    // module that instantiates itself, directly or not, is an error that its
    // elaboration reports, so the walk does not follow it.
    type Parent<'a> = Option<(Vec<(String, usize)>, ModuleId, &'a LoweredSvInstance)>;
    type Pending<'a> = (ModuleId, Vec<(String, usize)>, Parent<'a>, Vec<ModuleId>);
    let mut pending: std::collections::VecDeque<Pending<'_>> =
        std::collections::VecDeque::from([(root, Vec::new(), None, Vec::new())]);
    while let Some((module_id, path, parent, above)) = pending.pop_front() {
        let Some(module) = elaborated.get(&module_id) else {
            continue;
        };
        for handle in &module.home_handles {
            let home = if handle.port {
                let Some((parent_path, parent_module, instance)) = &parent else {
                    continue;
                };
                match actual_handle(
                    parent_path,
                    &elaborated[parent_module].home_handles,
                    instance,
                    handle,
                    &homes,
                ) {
                    Some(home) => home.clone(),
                    None => continue,
                }
            } else {
                (path.clone(), handle.handle.clone())
            };
            homes.insert((path.clone(), handle.handle.clone()), home);
        }
        let mut positions: HashMap<&str, usize> = HashMap::default();
        for instance in module.instances() {
            // Hierarchy indices follow listing order.
            let position = positions.entry(&instance.instance_name).or_default();
            let index = instance.array_index_base + *position;
            *position += 1;
            let Some(&child) = module_ids.get(&LoweredSvModuleKey::instance_key(instance)) else {
                continue;
            };
            if child == module_id || above.contains(&child) {
                continue;
            }
            let mut child_path = path.clone();
            child_path.push((instance.instance_name.clone(), index));
            let mut child_above = above.clone();
            child_above.push(module_id);
            pending.push_back((
                child,
                child_path,
                Some((path.clone(), module_id, instance)),
                child_above,
            ));
        }
    }
    let mut ports: Vec<_> = homes
        .into_iter()
        .filter(|((path, handle), home)| home != &(path.clone(), handle.clone()))
        .collect();
    ports.sort();
    ports
}
