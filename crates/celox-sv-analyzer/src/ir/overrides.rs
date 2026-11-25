//! The overrides of a module's classes that the dispatchers of every scope
//! call (IEEE 1800-2023 8.20; see `crate::ast::module_overrides`).

use super::*;
use crate::ast::module_overrides::MODULE_HANDLE;
use crate::ast::virtual_interfaces::DefinitionId;
use crate::ast::virtual_interfaces::VirtualInterfaceSignal;
use crate::procedural::{CaseItemBase, CaseKind, CaseLabel};
use crate::system_functions::{Callee, ObjectField, ObjectOp};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::collections::BTreeMap;

/// The classes a module declares, which analysis records for
/// `Module::export_overrides`. Those that override a method of a class
/// declared outside the module hold the instance id in a hidden property
/// (`crate::ast::module_overrides`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleClasses {
    /// The definition that the module's specialization is: the name its
    /// instances are numbered under, as those of an interface are.
    pub key: String,
    /// The names of the classes.
    pub classes: Vec<String>,
    /// The field of the hidden property `$instance` of each class
    /// that holds it.
    pub instance_fields: Vec<(String, ObjectField)>,
}

/// An override of a virtual method by a class of a module, which the
/// dispatchers of every scope call for the objects of the classes that run
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedOverride {
    /// The dispatchers that call it, each with the ids of the classes whose
    /// objects run it.
    pub dispatchers: Vec<(String, Vec<u32>)>,
    /// The override as its module lowered it: its name, formals and result,
    /// whose bodies `arms` give.
    pub template: Subroutine,
    /// The hidden property of the class that holds the id of the module
    /// instance its object was created in, if it has one.
    pub instance: Option<ObjectField>,
    /// The body for each definition of the module (see
    /// [`ModuleClasses::key`]), with the module's items as placeholders.
    pub arms: Vec<OverrideArm>,
    /// The subroutines it calls, directly or not, which a scope may lack.
    pub callees: Vec<Subroutine>,
    /// The names of those of `callees` that its module declares, which
    /// merging gives names of their own (see [`merge_overrides`]).
    pub helpers: Vec<String>,
    /// The locals of it, renamed for each definition, and of its callees.
    pub locals: Vec<LocalVariable>,
    /// The placeholders of the module's items it reads, and the package
    /// variables it and its callees name, which a scope binds to the
    /// packages' objects as one that imports their packages does.
    pub signals: Vec<Signal>,
    /// The name of the subroutine that selects among `arms`, which no
    /// declared subroutine of the design has (see [`merge_overrides`]).
    pub wrapper: Option<String>,
    /// The items of the module it reads, each with the definition key of
    /// the module's specialization; none for an override that reads no
    /// item.
    pub members: Vec<(
        String,
        crate::ast::virtual_interfaces::VirtualInterfaceMember,
    )>,
}

/// The body of an [`ExportedOverride`] for one definition of its module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideArm {
    /// The definition key (see [`ModuleClasses::key`]).
    pub module: String,
    pub body: Vec<Stmt>,
    /// The `@*` sensitivity body (see `SubroutineBase::sensitivity_body`).
    pub sensitivity_body: Vec<Stmt>,
    /// The automatic locals of the body, renamed for the definition.
    pub automatic_locals: Vec<String>,
}

impl ExportedOverride {
    /// The subroutine that the dispatchers call. The override of one
    /// definition that reads no item of its module is that subroutine.
    /// Otherwise it selects the arm of the definition and the instance the
    /// object was created in, as a call through a virtual interface selects
    /// an interface instance: the objects of the classes of every
    /// definition share the class id.
    pub fn subroutine(&self) -> Subroutine {
        let mut subroutine = self.template.clone();
        if let [arm] = self.arms.as_slice()
            && self.members.is_empty()
        {
            subroutine.body = arm.body.clone();
            subroutine.sensitivity_body = arm.sensitivity_body.clone();
            subroutine.automatic_locals = arm.automatic_locals.clone();
            return subroutine;
        }
        let field = self
            .instance
            .expect("an override of several definitions has its instance");
        subroutine.name = self
            .wrapper
            .clone()
            .expect("merging names the subroutine of an override of several definitions");
        subroutine.automatic_locals = self
            .arms
            .iter()
            .flat_map(|arm| arm.automatic_locals.iter().cloned())
            .collect();
        let this = Expr::Ident(self.template.params[0].name.clone());
        // The body, and the `@*` sensitivity body, of the instance's arm.
        let select = |part: &dyn Fn(&OverrideArm) -> &Vec<Stmt>| {
            vec![Stmt::Case {
                kind: CaseKind::Exact,
                selector: Expr::Call {
                    callee: Callee::Object(Box::new(ObjectOp::Load(field))),
                    args: vec![Some(this.clone())],
                    arg_order: Vec::new(),
                },
                items: self
                    .arms
                    .iter()
                    .map(|arm| CaseItemBase {
                        labels: vec![CaseLabel::Value(Expr::Ident(
                            VirtualInterfaceSignal::Instance {
                                definition: DefinitionId::of(&arm.module),
                            }
                            .name(),
                        ))],
                        // The arm is a copy of the override of the module
                        // instance it selects: its sites name that instance.
                        body: {
                            let mut body = part(arm).clone();
                            crate::procedural::set_site_home(
                                &mut body,
                                &crate::procedural::arm_home(),
                            );
                            body
                        },
                    })
                    .collect(),
                default: Some(Vec::new()),
            }]
        };
        subroutine.body = select(&|arm| &arm.body);
        subroutine.sensitivity_body = if self.arms.iter().all(|arm| arm.sensitivity_body.is_empty())
        {
            Vec::new()
        } else {
            select(&|arm| &arm.sensitivity_body)
        };
        subroutine
    }

    /// The definitions of the module whose instances its arms select.
    pub fn modules(&self) -> impl Iterator<Item = &str> {
        self.arms.iter().map(|arm| arm.module.as_str())
    }
}

/// The class a method subroutine `name` (`class::method`) belongs to.
fn owner(name: &str) -> Option<&str> {
    name.rsplit_once("::").map(|(owner, _)| owner)
}

/// `subroutine` with every name it owns that `rename` renames renamed, and
/// every such name added to `seen`. Collecting what a subroutine names and
/// rewriting it are this one walk, built on `SubroutineBase::map`, which
/// rebuilds every part of the subroutine: its body and `@*` sensitivity
/// body, its formals' defaults, its locals' initializers. No part is seen
/// by one and missed by the other.
fn walk(
    subroutine: Subroutine,
    rename: &dyn Fn(&str) -> Option<String>,
    seen: &mut HashSet<String>,
) -> Subroutine {
    fn name(
        name: &mut String,
        rename: &dyn Fn(&str) -> Option<String>,
        seen: &mut HashSet<String>,
    ) {
        seen.insert(name.clone());
        if let Some(renamed) = rename(name) {
            *name = renamed;
        }
    }
    let mut exprs = HashSet::default();
    let mut lvalues = HashSet::default();
    let mut subroutine = subroutine.map(
        &mut |e| walk_expr(e, rename, &mut exprs),
        &mut |mut lvalue: LValue| {
            let (LValue::Ident(ident) | LValue::Select { name: ident, .. }) = &mut lvalue;
            name(ident, rename, &mut lvalues);
            lvalue
        },
        &mut |ty| ty,
    );
    seen.extend(exprs);
    seen.extend(lvalues);
    for param in &mut subroutine.params {
        name(&mut param.name, rename, seen);
        for leaf in &mut param.leaves {
            name(leaf, rename, seen);
        }
    }
    if let Some(result) = &mut subroutine.return_var {
        name(result, rename, seen);
    }
    for local in subroutine
        .automatic_locals
        .iter_mut()
        .chain(&mut subroutine.persistent_locals)
    {
        name(local, rename, seen);
    }
    // The names that statements declare or call.
    for stmt in subroutine
        .body
        .iter_mut()
        .chain(&mut subroutine.sensitivity_body)
    {
        stmt.visit_mut(&mut |_| {}, &mut |_| {}, &mut |declared| {
            name(declared, rename, seen)
        });
    }
    subroutine
}

/// [`walk`] of one expression, for the operands of another.
fn walk_expr(
    expr: Expr,
    rename: &dyn Fn(&str) -> Option<String>,
    seen: &mut HashSet<String>,
) -> Expr {
    let expr = match expr {
        Expr::Ident(ident) => {
            seen.insert(ident.clone());
            return Expr::Ident(rename(&ident).unwrap_or(ident));
        }
        Expr::Call {
            callee: Callee::Subroutine(callee),
            args,
            arg_order,
        } => {
            seen.insert(callee.clone());
            Expr::Call {
                callee: Callee::Subroutine(rename(&callee).unwrap_or(callee)),
                args,
                arg_order,
            }
        }
        expr => expr,
    };
    map_operands(expr, &mut |child| walk_expr(child, rename, seen))
}

/// Every name `subroutine` reads, writes, declares or calls (see [`walk`]).
fn subroutine_names(subroutine: &Subroutine) -> HashSet<String> {
    let mut seen = HashSet::default();
    walk(subroutine.clone(), &|_| None, &mut seen);
    seen
}

fn placeholder(key: &str, member: &str) -> String {
    VirtualInterfaceSignal::Placeholder {
        definition: DefinitionId::of(key),
        member: member.to_owned(),
    }
    .name()
}

/// The call of `target` that a dispatcher with formals `params` and result
/// `result` makes, passing its formals on (as `dispatch_body` of the
/// analysis does).
fn dispatch_call(target: &str, params: &[SubroutineParam], result: Option<&str>) -> Stmt {
    let args = params
        .iter()
        .map(|param| {
            Some(
                if param.direction.is_reference() && !param.leaves.is_empty() {
                    Expr::Concat(param.leaves.iter().cloned().map(Expr::Ident).collect())
                } else {
                    Expr::Ident(param.name.clone())
                },
            )
        })
        .collect::<Vec<_>>();
    match result {
        Some(result) => Stmt::Assign {
            lhs: LValue::Ident(result.to_owned()),
            rhs: Expr::Call {
                callee: Callee::Subroutine(target.to_owned()),
                args,
                arg_order: Vec::new(),
            },
            nonblocking: false,
        },
        None => Stmt::Call {
            name: target.to_owned(),
            args,
            arg_order: Vec::new(),
        },
    }
}

/// The subroutine a case item of a dispatcher calls.
fn item_target(body: &[Stmt]) -> Option<&str> {
    match body {
        [Stmt::Call { name, .. }] => Some(name),
        [
            Stmt::Assign {
                rhs:
                    Expr::Call {
                        callee: Callee::Subroutine(name),
                        ..
                    },
                ..
            },
        ] => Some(name),
        _ => None,
    }
}

/// The class ids a case item of a dispatcher is labelled with.
fn item_ids(labels: &[CaseLabel<Expr>]) -> Vec<u32> {
    labels
        .iter()
        .filter_map(|label| match label {
            CaseLabel::Value(Expr::Literal(text)) => text.strip_prefix("32'd")?.parse().ok(),
            _ => None,
        })
        .collect()
}

impl Module {
    /// Export the overrides that the module's classes `classes` make of
    /// the virtual methods of classes declared outside the module: the
    /// arms that the module's own lowering of those classes' dispatchers
    /// adds for its classes (see `subroutines`'s dispatcher re-lowering).
    pub(crate) fn export_overrides(
        &mut self,
        classes: &ModuleClasses,
    ) -> Result<(), crate::AnalyzerError> {
        let local_classes: HashSet<&str> = classes.classes.iter().map(String::as_str).collect();
        let local = |name: &str| owner(name).is_none_or(|owner| local_classes.contains(owner));
        let by_name: HashMap<&str, &Subroutine> = self
            .subroutines
            .iter()
            .map(|subroutine| (subroutine.name.as_str(), subroutine))
            .collect();
        // The dispatchers of classes outside the module that call an
        // override of a class of the module, by override.
        let mut targets: BTreeMap<&str, Vec<(String, Vec<u32>)>> = BTreeMap::new();
        for dispatcher in &self.subroutines {
            let Some(name) = dispatcher.name.strip_suffix("$virtual") else {
                continue;
            };
            if local(name) {
                continue;
            }
            let Some(Stmt::Case { items, .. }) = dispatcher.body.first() else {
                continue;
            };
            for item in items {
                let Some(target) = item_target(&item.body) else {
                    continue;
                };
                if owner(target).is_some_and(|owner| local_classes.contains(owner)) {
                    targets
                        .entry(target)
                        .or_default()
                        .push((dispatcher.name.clone(), item_ids(&item.labels)));
                }
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        // The module's items an override may read: its signals and ports.
        let mut items: HashMap<&str, (Type, bool)> = HashMap::default();
        for signal in &self.signals {
            if VirtualInterfaceSignal::of(&signal.name).is_none()
                && signal.package_variable.is_none()
                && signal.hierarchical.is_none()
            {
                items.insert(&signal.name, (signal.r#type.clone(), signal.is_net));
            }
        }
        for port in &self.ports {
            items.insert(&port.name, (port.r#type.clone(), port.is_net));
        }
        // The module's constants and those of the packages it imports,
        // which the bounds of its items may name.
        let constants = self
            .parameters
            .iter()
            .chain(&self.imported_parameters)
            .filter_map(|parameter| {
                Some((parameter.name().to_owned(), parameter.resolved_value()?))
            })
            .collect::<crate::ast::ConstEnv>();
        let mut exports = Vec::new();
        let mut handle_members: BTreeMap<String, ()> = BTreeMap::new();
        for (target, dispatchers) in targets {
            let Some(&subroutine) = by_name.get(target) else {
                continue;
            };
            // The subroutines it calls, directly or not.
            let mut callees: Vec<&Subroutine> = Vec::new();
            let mut seen: HashSet<&str> = HashSet::from_iter([target]);
            let mut pending = vec![subroutine];
            while let Some(next) = pending.pop() {
                for name in subroutine_names(next) {
                    if let Some((&known, &callee)) = by_name.get_key_value(name.as_str())
                        && seen.insert(known)
                    {
                        callees.push(callee);
                        pending.push(callee);
                    }
                }
            }
            // A hierarchical name the override or its helpers read is bound
            // where the module's hierarchy is: a scope receiving the export
            // has no binding of it.
            let hierarchical: HashSet<&str> = self
                .signals
                .iter()
                .filter(|signal| signal.hierarchical.is_some())
                .map(|signal| signal.name.as_str())
                .collect();
            for reached in std::iter::once(subroutine).chain(callees.iter().copied()) {
                if let Some(name) = subroutine_names(reached)
                    .into_iter()
                    .find(|name| hierarchical.contains(name.as_str()))
                {
                    return Err(crate::AnalyzerError::Unsupported(format!(
                        "override `{target}` of a class of module `{}` that reads the hierarchical name `{name}`",
                        self.name
                    )));
                }
            }
            for callee in &callees {
                // A static variable of a helper is the module's, which the
                // copies that receiving scopes make of the helper would each
                // hold apart.
                if local(&callee.name) && !callee.persistent_locals.is_empty() {
                    return Err(crate::AnalyzerError::Unsupported(format!(
                        "override `{target}` of a class of module `{}` that calls `{}`, which has static variables",
                        self.name, callee.name
                    )));
                }
                if local(&callee.name)
                    && subroutine_names(callee)
                        .iter()
                        .any(|name| items.contains_key(name.as_str()))
                {
                    return Err(crate::AnalyzerError::Unsupported(format!(
                        "override `{target}` of a class of module `{}` that calls `{}`, which reads the module's items",
                        self.name, callee.name
                    )));
                }
            }
            let names = subroutine_names(subroutine);
            let read: HashSet<&str> = items
                .keys()
                .copied()
                .filter(|item| names.contains(*item))
                .collect();
            let class = owner(target).unwrap_or_default();
            let instance = classes
                .instance_fields
                .iter()
                .find(|(name, _)| name == class)
                .map(|(_, field)| *field);
            // A static variable of the override is the class's, one for every
            // call (IEEE 1800-2023 8.10); the copies that receiving scopes
            // make of the override would each hold their own.
            if !subroutine.persistent_locals.is_empty() {
                return Err(crate::AnalyzerError::Unsupported(format!(
                    "static variables of override `{target}` of a class of module `{}`",
                    self.name
                )));
            }
            if !read.is_empty() {
                if instance.is_none() {
                    return Err(crate::AnalyzerError::Unsupported(format!(
                        "override `{target}` of class `{class}` of module `{}` without its instance",
                        self.name
                    )));
                }
            }
            // The locals of the body belong to this definition: another
            // definition's arm may give a local of the same name another
            // type. The formals and the result are the override's own.
            let shared: HashSet<&str> = subroutine
                .params
                .iter()
                .map(|param| param.name.as_str())
                .chain(
                    subroutine
                        .params
                        .iter()
                        .flat_map(|param| param.leaves.iter().map(String::as_str)),
                )
                .chain(subroutine.return_var.as_deref())
                .collect();
            // So do the locals of its helpers, the module's own subroutines,
            // which another definition may declare alike.
            let mut helper_names: HashSet<String> = HashSet::default();
            for callee in callees.iter().filter(|callee| local(&callee.name)) {
                helper_names.extend(subroutine_names(callee));
            }
            let mut renamed: HashMap<String, String> = self
                .locals
                .iter()
                .filter(|local| {
                    (names.contains(&local.name) && !shared.contains(local.name.as_str()))
                        || helper_names.contains(&local.name)
                })
                .map(|local| {
                    (
                        local.name.clone(),
                        format!("{}${}", local.name, classes.key),
                    )
                })
                .collect();
            // A package variable is named by its package where the export
            // goes, whatever import named it in the module: another scope may
            // declare a variable of the name the import gave it.
            let mut reached = names.clone();
            for callee in &callees {
                reached.extend(subroutine_names(callee));
            }
            let packaged: Vec<(&Signal, String)> = self
                .signals
                .iter()
                .filter(|signal| reached.contains(&signal.name))
                .filter_map(|signal| {
                    let (package, variable) = signal.package_variable.as_ref()?;
                    Some((signal, format!("{package}::{variable}")))
                })
                .collect();
            let mut qualify: HashMap<String, String> = HashMap::default();
            for (signal, qualified) in &packaged {
                if signal.name != *qualified {
                    qualify.insert(signal.name.clone(), qualified.clone());
                }
            }
            renamed.extend(
                qualify
                    .iter()
                    .map(|(name, qualified)| (name.clone(), qualified.clone())),
            );
            // The arm: the override with the module's items as placeholders,
            // its locals renamed for the definition and package variables
            // named by their packages, in every part (see `walk`).
            let rename = |name: &str| {
                if read.contains(name) {
                    Some(placeholder(&classes.key, name))
                } else {
                    renamed.get(name).cloned()
                }
            };
            let rewritten = walk(subroutine.clone(), &rename, &mut HashSet::default());
            let arm = OverrideArm {
                module: classes.key.clone(),
                body: rewritten.body.clone(),
                sensitivity_body: rewritten.sensitivity_body.clone(),
                automatic_locals: rewritten.automatic_locals.clone(),
            };
            let mut used = names.clone();
            for callee in &callees {
                used.extend(subroutine_names(callee));
            }
            let locals = self
                .locals
                .iter()
                .filter(|local| used.contains(&local.name))
                .map(|local| match renamed.get(&local.name) {
                    Some(name) => LocalVariable {
                        name: name.clone(),
                        ..local.clone()
                    },
                    None => local.clone(),
                })
                .collect();
            let mut read: Vec<&str> = read.into_iter().collect();
            read.sort_unstable();
            let members = read
                .iter()
                .map(|&member| {
                    let (r#type, is_net) = &items[member];
                    let r#type = crate::ast::virtual_interfaces::resolved_type(r#type, &constants)
                        .ok_or_else(|| {
                            crate::AnalyzerError::Unsupported(format!(
                                "item `{member}` of module `{}` that override `{target}` reads",
                                self.name
                            ))
                        })?;
                    Ok((
                        classes.key.clone(),
                        crate::ast::virtual_interfaces::VirtualInterfaceMember {
                            name: member.to_owned(),
                            r#type,
                            is_net: *is_net,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, crate::AnalyzerError>>()?;
            let mut signals: Vec<Signal> = read
                .iter()
                .map(|&member| {
                    handle_members.insert(member.to_owned(), ());
                    let (r#type, is_net) = items[member].clone();
                    Signal::new(placeholder(&classes.key, member), r#type, is_net)
                })
                .collect();
            signals.extend(packaged.iter().map(|(signal, qualified)| Signal {
                name: qualified.clone(),
                ..(*signal).clone()
            }));
            // The module's own helpers name their locals for the definition,
            // and package variables by their packages, too. Merging names
            // them apart from every other subroutine (see `merge_overrides`).
            let helpers: Vec<String> = callees
                .iter()
                .filter(|callee| local(&callee.name))
                .map(|callee| callee.name.clone())
                .collect();
            let callees = callees
                .into_iter()
                .map(|callee| {
                    if local(&callee.name) {
                        walk(
                            callee.clone(),
                            &|name| renamed.get(name).cloned(),
                            &mut HashSet::default(),
                        )
                    } else {
                        callee.clone()
                    }
                })
                .collect::<Vec<_>>();
            exports.push(ExportedOverride {
                dispatchers,
                template: rewritten,
                instance,
                arms: vec![arm],
                callees,
                helpers,
                locals,
                signals,
                wrapper: None,
                members,
            });
        }
        // The module numbers its instances whenever it exports an override
        // that a class holding its instance makes, which another
        // definition's export of the override may need to select.
        if exports.iter().any(|export| export.instance.is_some()) {
            self.interface_handles.push(InterfaceHandle {
                handle: MODULE_HANDLE.to_owned(),
                interface: classes.key.clone(),
                port: false,
                members: handle_members
                    .into_keys()
                    .map(|member| (member.clone(), member))
                    .collect(),
                blocks: Vec::new(),
            });
        }
        self.exported_overrides = exports;
        Ok(())
    }

    /// The overrides of the module's classes that every scope's dispatchers
    /// call (see `Module::export_overrides`).
    pub fn exported_overrides(&self) -> &[ExportedOverride] {
        &self.exported_overrides
    }

    /// Add `overrides` to the dispatchers of the module that call them,
    /// with what they need: the subroutines, locals and placeholders the
    /// module lacks.
    pub(crate) fn inject_overrides(&mut self, overrides: &[ExportedOverride]) {
        let mut subroutines: HashSet<String> = self
            .subroutines
            .iter()
            .map(|subroutine| subroutine.name.clone())
            .collect();
        let mut locals: HashSet<String> =
            self.locals.iter().map(|local| local.name.clone()).collect();
        let mut signals: HashSet<String> = self
            .signals
            .iter()
            .map(|signal| signal.name.clone())
            .collect();
        for export in overrides {
            let exported = export.subroutine();
            let mut called = false;
            for (dispatcher, ids) in &export.dispatchers {
                let Some(subroutine) = self
                    .subroutines
                    .iter_mut()
                    .find(|subroutine| subroutine.name == *dispatcher)
                else {
                    continue;
                };
                let call = dispatch_call(
                    &exported.name,
                    &subroutine.params,
                    subroutine.return_var.as_deref(),
                );
                let Some(Stmt::Case { items, .. }) = subroutine.body.first_mut() else {
                    continue;
                };
                // The arm a scope that sees the class lowered calls the
                // override directly; the export serves every instance.
                for item in items.iter_mut() {
                    item.labels.retain(|label| match label {
                        CaseLabel::Value(Expr::Literal(text)) => !ids
                            .iter()
                            .any(|id| text.strip_prefix("32'd") == Some(id.to_string().as_str())),
                        _ => true,
                    });
                }
                items.retain(|item| !item.labels.is_empty());
                items.push(CaseItemBase {
                    labels: ids
                        .iter()
                        .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                        .collect(),
                    body: vec![call],
                });
                called = true;
            }
            if !called {
                continue;
            }
            for subroutine in std::iter::once(&exported).chain(&export.callees) {
                if subroutines.insert(subroutine.name.clone()) {
                    self.subroutines.push(subroutine.clone());
                }
            }
            for local in &export.locals {
                if locals.insert(local.name.clone()) {
                    self.locals.push(local.clone());
                }
            }
            for signal in &export.signals {
                if signals.insert(signal.name.clone()) {
                    self.signals.push(signal.clone());
                }
            }
        }
    }
}

/// `exports` with those of one override merged: a module's
/// specializations, and modules that declare classes of one name, each
/// export the override of a class whose objects share the class id, so one
/// subroutine selects among the instances of all their definitions. A
/// definition that more than one analysis exported counts once.
///
/// `taken` holds the names of the subroutines the design declares, which
/// the subroutine selecting among the arms of an override does not take.
pub fn merge_overrides(
    exports: impl IntoIterator<Item = ExportedOverride>,
    taken: &HashSet<String>,
) -> Result<Vec<ExportedOverride>, crate::AnalyzerError> {
    let unsupported = |name: &str, what: &str| {
        crate::AnalyzerError::Unsupported(format!(
            "override `{name}` of classes of several module definitions {what}"
        ))
    };
    let mut merged: BTreeMap<String, ExportedOverride> = BTreeMap::new();
    // The names given to the subroutines merging makes or renames.
    let mut named: HashSet<String> = HashSet::default();
    let fresh = |base: String, named: &mut HashSet<String>| {
        let name = std::iter::once(base.clone())
            .chain((1..).map(|index| format!("{base}_{index}")))
            .find(|name| !taken.contains(name) && !named.contains(name))
            .expect("an unbounded sequence of names");
        named.insert(name.clone());
        name
    };
    for mut export in exports {
        // The module's helpers of each export are subroutines of their own:
        // another module, or definition, may declare a helper of the name.
        if !export.helpers.is_empty() {
            let helpers: HashMap<String, String> = export
                .helpers
                .iter()
                .map(|helper| {
                    (
                        helper.clone(),
                        fresh(format!("{helper}$helper"), &mut named),
                    )
                })
                .collect();
            let rename = |name: &str| helpers.get(name).cloned();
            export.template = walk(export.template, &rename, &mut HashSet::default());
            for arm in &mut export.arms {
                let rewritten = walk(
                    Subroutine {
                        body: std::mem::take(&mut arm.body),
                        sensitivity_body: std::mem::take(&mut arm.sensitivity_body),
                        ..export.template.clone()
                    },
                    &rename,
                    &mut HashSet::default(),
                );
                arm.body = rewritten.body;
                arm.sensitivity_body = rewritten.sensitivity_body;
            }
            for callee in &mut export.callees {
                let mut rewritten = walk(callee.clone(), &rename, &mut HashSet::default());
                if let Some(name) = helpers.get(&callee.name) {
                    rewritten.name = name.clone();
                }
                *callee = rewritten;
            }
            export.helpers = export
                .helpers
                .iter()
                .map(|helper| helpers[helper].clone())
                .collect();
        }
        let name = export.template.name.clone();
        let Some(known) = merged.get_mut(&name) else {
            merged.insert(name, export);
            continue;
        };
        let modules: HashSet<String> = known.modules().map(str::to_owned).collect();
        let arms: Vec<OverrideArm> = export
            .arms
            .into_iter()
            .filter(|arm| !modules.contains(&arm.module))
            .collect();
        if arms.is_empty() {
            continue;
        }
        let signature = |subroutine: &Subroutine| {
            (
                subroutine
                    .params
                    .iter()
                    .map(|param| (param.name.clone(), param.r#type.clone(), param.direction))
                    .collect::<Vec<_>>(),
                subroutine.return_type.clone(),
                subroutine.return_var.clone(),
            )
        };
        if signature(&known.template) != signature(&export.template) {
            return Err(unsupported(&name, "with formals or results of other types"));
        }
        if known.instance.is_none() || known.instance != export.instance {
            return Err(unsupported(&name, "without one instance property"));
        }
        if !known.template.persistent_locals.is_empty() {
            return Err(unsupported(&name, "with static variables"));
        }
        known.arms.extend(arms);
        for (dispatcher, ids) in export.dispatchers {
            match known
                .dispatchers
                .iter_mut()
                .find(|(known, _)| *known == dispatcher)
            {
                Some((_, known)) => {
                    for id in ids {
                        if !known.contains(&id) {
                            known.push(id);
                        }
                    }
                }
                None => known.dispatchers.push((dispatcher, ids)),
            }
        }
        for callee in export.callees {
            match known.callees.iter().find(|known| known.name == callee.name) {
                Some(other) if *other != callee => {
                    return Err(unsupported(
                        &name,
                        &format!("that call different subroutines `{}`", callee.name),
                    ));
                }
                Some(_) => {}
                None => known.callees.push(callee),
            }
        }
        for local in export.locals {
            match known.locals.iter().find(|known| known.name == local.name) {
                Some(other) if *other != local => {
                    return Err(unsupported(
                        &name,
                        &format!("whose callees declare `{}` differently", local.name),
                    ));
                }
                Some(_) => {}
                None => known.locals.push(local),
            }
        }
        for signal in export.signals {
            if !known.signals.iter().any(|known| known.name == signal.name) {
                known.signals.push(signal);
            }
        }
        known.members.extend(export.members);
    }
    let mut merged: Vec<ExportedOverride> = merged.into_values().collect();
    for export in &mut merged {
        if export.arms.len() == 1 && export.members.is_empty() {
            continue;
        }
        export.wrapper = Some(fresh(
            format!("{}$instances", export.template.name),
            &mut named,
        ));
    }
    Ok(merged)
}
