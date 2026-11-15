//! Tasks and functions of an interface called through a virtual interface
//! (IEEE 1800-2023 25.9).
//!
//! A call `vif.drive(req)` calls the subroutine `I$vif$drive(vif, req)`,
//! where `I` is the interface of the handle's type
//! ([`virtual_interfaces::subroutine_name`]). This module builds one such
//! subroutine for each task and function of each interface a virtual
//! interface type names: the subroutine of the interface, analyzed in the
//! interface's own scope, with every member of the interface renamed to the
//! placeholder of that member (`$vifany$I$m`), its body in one case item on
//! the handle labelled with the instance placeholder (`$vifinstance$I`).
//! Expanding a module's virtual interface placeholders repeats that item
//! for each instance of the interface, with the members of that instance
//! ([`crate::ir::Module::expand_virtual_interface_instances`]), as it does
//! for any other access through a virtual interface.
//!
//! One subroutine serves every instance, so it runs as an automatic one:
//! its formals and variables belong to each call, where those of a static
//! subroutine of an interface would belong to each instance (13.3.2).

use super::*;
use virtual_interfaces::{VirtualInterface, VirtualInterfaceSignal};

/// The subroutines that call the tasks and functions of interfaces through
/// virtual interfaces, which every scope that may make such a call imports.
#[derive(Debug, Clone, Default)]
pub(crate) struct Subroutines {
    pub(super) subroutines: Vec<Arc<Subroutine>>,
    /// The shapes of the formals of each, the handle's first.
    pub(super) shapes: HashMap<String, Arc<Vec<VariableDimensions>>>,
    /// Their formals and variables, which a scope declares with them.
    pub(super) locals: Vec<Arc<LocalVariable>>,
}

/// The subroutines of the interfaces in `declarations`, built with the
/// analyzed `packages` (see the module documentation).
pub(super) fn build(
    packages: &packages::Packages,
    declarations: &virtual_interfaces::VirtualInterfaces,
) -> Converted<Subroutines> {
    let mut built = Subroutines::default();
    let mut interfaces: Vec<_> = declarations
        .interfaces
        .iter()
        .filter(|(_, declaration)| !declaration.subroutines.is_empty())
        .collect();
    interfaces.sort_by(|left, right| left.0.cmp(right.0));
    for (interface, declaration) in interfaces {
        let Some((tree, path)) = packages.interface_sources.get(interface) else {
            continue;
        };
        for handle in tree.root_scopes()? {
            let scoped = tree.scoped(handle)?;
            if instances::normalize_identifier(&scoped.root_name()?) != *interface {
                continue;
            }
            // An interface whose subroutines cannot be analyzed has none to
            // call through a virtual interface; a design that calls none
            // keeps working, and a call names the missing subroutine.
            let mut found = Subroutines::default();
            if interface_subroutines(packages, &scoped, path, interface, declaration, &mut found)
                .is_ok()
            {
                built.subroutines.extend(found.subroutines);
                built.shapes.extend(found.shapes);
                built.locals.extend(found.locals);
            }
        }
    }
    Ok(built)
}

/// Add the subroutines of interface `interface`, whose declaration `scoped`
/// is, to `built`.
fn interface_subroutines(
    packages: &packages::Packages,
    scoped: &SyntaxTree,
    path: &std::path::Path,
    interface: &str,
    declaration: &virtual_interfaces::VirtualInterfaceDeclaration,
    built: &mut Subroutines,
) -> Converted<()> {
    let overrides = HashMap::default();
    let context = AnalysisContext::unspecialized(scoped);
    let mut design = packages.design();
    let session = &mut Session::with_packages(packages, &mut design);
    let module = interface_ast::analyze(
        &context.child(scoped, &overrides),
        session,
        path,
        packages,
        &overrides,
        &[],
        &ModuleInterfaces::default(),
        false,
    )?;
    let symbols = module
        .symbols
        .as_ref()
        .ok_or_else(|| unsupported("interface without symbols"))?;
    let prefix = format!("{interface}$vif");
    let mut scratch = scope::ScopeSymbols::default();
    let mut names =
        interface_binding::binding_names_with_prefix(&module, symbols, "", &prefix, &mut scratch);
    for member in &declaration.members {
        names.insert(
            member.name.clone(),
            VirtualInterfaceSignal::Placeholder {
                interface: interface.to_string(),
                member: member.name.clone(),
            }
            .name(),
        );
    }
    let bound = symbols.renamed(&names);
    let own: HashSet<String> = declaration
        .subroutines
        .keys()
        .map(|name| virtual_interfaces::subroutine_name(interface, name))
        .collect();
    let ty = virtual_interfaces::value_type(VirtualInterface {
        interface: interface.to_string(),
        modport: None,
    });
    let handle_shape = procedural::dimensions_from_type(&ty);
    let ty = type_nodes::ir_type(session, &context.functions, ty, &ConstEnv::default());
    for subroutine in &bound.subroutines {
        if !own.contains(&subroutine.name) {
            continue;
        }
        let mut shapes = vec![handle_shape.clone()];
        shapes.extend(
            bound
                .subroutine_shapes
                .get(&subroutine.name)
                .map(|shapes| shapes.as_ref().clone())
                .unwrap_or_default(),
        );
        built
            .shapes
            .insert(subroutine.name.clone(), Arc::new(shapes));
        // Each subroutine names its handle apart from the others', which
        // the handle of a call of another one reads.
        let handle = format!("{}$$handle", subroutine.name);
        let dispatching = dispatching(subroutine, interface, &handle, &ty, &own);
        built.subroutines.push(Arc::new(dispatching));
        built.locals.push(Arc::new(LocalVariable {
            name: handle.clone(),
            source_name: handle,
            r#type: ty.clone(),
            owned: true,
        }));
    }
    // The variables of the interface's subroutines, renamed with them.
    for local in &bound.locals {
        if local.name.starts_with(&prefix)
            && !built.locals.iter().any(|known| known.name == local.name)
        {
            built.locals.push(local.clone());
        }
    }
    Ok(())
}

/// Subroutine `subroutine` of interface `interface`, with members renamed
/// to their placeholders, taking the handle as formal `handle` and running
/// its body for the instance the handle denotes.
fn dispatching(
    subroutine: &Subroutine,
    interface: &str,
    handle: &str,
    ty: &crate::ir::Type,
    own: &HashSet<String>,
) -> Subroutine {
    let mut subroutine = subroutine.clone();
    let value = Expr::Ident(handle.to_string());
    // A call of another subroutine of the interface runs it for the same
    // instance.
    for stmt in &mut subroutine.body {
        pass_handle(stmt, &value, own);
    }
    let mut automatic_locals = vec![handle.to_string()];
    automatic_locals.extend(subroutine.params.iter().map(|param| param.name.clone()));
    automatic_locals.extend(subroutine.return_var.clone());
    for name in std::mem::take(&mut subroutine.automatic_locals)
        .into_iter()
        .chain(subroutine.body.iter().flat_map(declared_locals))
    {
        if !automatic_locals.contains(&name) {
            automatic_locals.push(name);
        }
    }
    subroutine.automatic = true;
    subroutine.automatic_locals = automatic_locals;
    subroutine.params.insert(
        0,
        SubroutineParam {
            name: handle.to_string(),
            source_name: handle.to_string(),
            direction: crate::procedural::ParamDirection::Input,
            r#type: ty.clone(),
            default: None,
            leaves: Vec::new(),
        },
    );
    let instance = VirtualInterfaceSignal::Instance {
        interface: interface.to_string(),
    }
    .name();
    subroutine.body = vec![Stmt::Case {
        kind: crate::procedural::CaseKind::Exact,
        selector: value.clone(),
        items: vec![crate::procedural::CaseItemBase {
            labels: vec![crate::procedural::CaseLabel::Value(Expr::Ident(instance))],
            body: std::mem::take(&mut subroutine.body),
        }],
        default: Some(vec![Stmt::Eval(virtual_interfaces::null_access(
            &value, 1, false,
        ))]),
    }];
    subroutine
}

/// The names of the variables `stmt` declares.
fn declared_locals(stmt: &Stmt) -> Vec<String> {
    let mut names = Vec::new();
    stmt.walk(&mut |stmt| {
        if let Stmt::Local { name, .. } = stmt {
            names.push(name.clone());
        }
    });
    names
}

/// `stmt` with `handle` passed first to each call of a subroutine in `own`.
fn pass_handle(stmt: &mut Stmt, handle: &Expr, own: &HashSet<String>) {
    stmt.walk_mut(&mut |stmt| {
        if let Stmt::Call { name, args, .. } = stmt
            && own.contains(name)
        {
            args.insert(0, Some(handle.clone()));
        }
    });
    stmt.visit_mut(
        &mut |expr| pass_handle_expr(expr, handle, own),
        &mut |_| {},
        &mut |_| {},
    );
}

fn pass_handle_expr(expr: &mut Expr, handle: &Expr, own: &HashSet<String>) {
    let taken = std::mem::replace(expr, Expr::Literal(String::new()));
    *expr = map_call_args(taken, handle, own);
}

fn map_call_args(expr: Expr, handle: &Expr, own: &HashSet<String>) -> Expr {
    let expr =
        virtual_interfaces::map_children(expr, &mut |child| map_call_args(child, handle, own));
    match expr {
        Expr::Call {
            callee: Callee::Subroutine(name),
            mut args,
            arg_order,
        } if own.contains(&name) => {
            args.insert(0, Some(handle.clone()));
            Expr::Call {
                callee: Callee::Subroutine(name),
                args,
                arg_order,
            }
        }
        expr => expr,
    }
}
