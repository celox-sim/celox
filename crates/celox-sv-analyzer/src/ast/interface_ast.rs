//! Structural views of interface declarations. Tokens retain their original
//! locations; no generated SystemVerilog is parsed to collect an interface.
use super::*;

pub(super) fn module_view(
    interface: &sv_parser::InterfaceDeclarationAnsi,
) -> Converted<sv_parser::ModuleDeclarationAnsi> {
    use sv_parser::*;
    let header = &interface.nodes.0.nodes;
    let items = interface
        .nodes
        .2
        .iter()
        .filter_map(|item| match item {
            NonPortInterfaceItem::ModportDeclaration(_) => None,
            NonPortInterfaceItem::TimeunitsDeclaration(item) => {
                Some(Ok(NonPortModuleItem::TimeunitsDeclaration(item.clone())))
            }
            NonPortInterfaceItem::GenerateRegion(region) => {
                let mut region = region.clone();
                Some(
                    region
                        .nodes
                        .1
                        .iter_mut()
                        .try_for_each(generate_item)
                        .map(|()| NonPortModuleItem::GenerateRegion(region)),
                )
            }
            NonPortInterfaceItem::InterfaceOrGenerateItem(item) => Some(
                module_item(item)
                    .map(|item| NonPortModuleItem::ModuleOrGenerateItem(Box::new(item))),
            ),
            _ => Some(Err(unsupported("nested declaration in interface"))),
        })
        .collect::<Converted<Vec<_>>>()?;
    Ok(ModuleDeclarationAnsi {
        nodes: (
            ModuleAnsiHeader {
                nodes: (
                    header.0.clone(),
                    ModuleKeyword::Module(Box::new(header.1.clone())),
                    header.2.clone(),
                    ModuleIdentifier {
                        nodes: header.3.nodes.clone(),
                    },
                    header.4.clone(),
                    header.5.clone(),
                    header.6.clone(),
                    header.7.clone(),
                ),
            },
            interface.nodes.1.clone(),
            items,
            interface.nodes.3.clone(),
            interface.nodes.4.as_ref().map(|(colon, name)| {
                (
                    colon.clone(),
                    ModuleIdentifier {
                        nodes: name.nodes.clone(),
                    },
                )
            }),
        ),
    })
}

fn module_item(
    item: &sv_parser::InterfaceOrGenerateItem,
) -> Converted<sv_parser::ModuleOrGenerateItem> {
    let sv_parser::InterfaceOrGenerateItem::Module(item) = item else {
        return Err(unsupported("extern subroutine in interface"));
    };
    let mut common = item.nodes.1.clone();
    common_item(&mut common)?;
    Ok(sv_parser::ModuleOrGenerateItem::ModuleItem(Box::new(
        sv_parser::ModuleOrGenerateItemModuleItem {
            nodes: (item.nodes.0.clone(), common),
        },
    )))
}

fn generate_item(item: &mut sv_parser::GenerateItem) -> Converted<()> {
    match item {
        sv_parser::GenerateItem::InterfaceOrGenerateItem(interface) => {
            *item =
                sv_parser::GenerateItem::ModuleOrGenerateItem(Box::new(module_item(interface)?));
        }
        sv_parser::GenerateItem::ModuleOrGenerateItem(item) => {
            if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item.as_mut() {
                common_item(&mut item.nodes.1)?;
            }
        }
        _ => return Err(unsupported("checker item in interface")),
    }
    Ok(())
}

fn block(block: &mut sv_parser::GenerateBlock) -> Converted<()> {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => generate_item(item),
        sv_parser::GenerateBlock::Multiple(block) => {
            block.nodes.3.iter_mut().try_for_each(generate_item)
        }
    }
}

fn common_item(item: &mut sv_parser::ModuleCommonItem) -> Converted<()> {
    use sv_parser::*;
    match item {
        ModuleCommonItem::LoopGenerateConstruct(item) => block(&mut item.nodes.2)?,
        ModuleCommonItem::ConditionalGenerateConstruct(item) => match item.as_mut() {
            ConditionalGenerateConstruct::If(item) => {
                block(&mut item.nodes.2)?;
                if let Some((_, other)) = &mut item.nodes.3 {
                    block(other)?;
                }
            }
            ConditionalGenerateConstruct::Case(item) => {
                for case in &mut item.nodes.2 {
                    match case {
                        CaseGenerateItem::Nondefault(item) => block(&mut item.nodes.2)?,
                        CaseGenerateItem::Default(item) => block(&mut item.nodes.2)?,
                    }
                }
            }
        },
        _ => {}
    }
    Ok(())
}

/// Analyze the original interface body and retain its symbols for the scope
/// that instantiates it. The structural view borrows every token's original
/// source location, including the lexical time scope of processes/functions.
pub(super) fn analyze(
    declaration: &sv_parser::InterfaceDeclarationAnsi,
    tree: &SyntaxTree,
    path: &Path,
    packages: &packages::Packages,
    overrides: &HashMap<String, ConstExpr>,
    interfaces: &ModuleInterfaces,
    declarations_only: bool,
) -> Converted<Module> {
    let view = module_view(declaration)?;
    let node = RefNode::ModuleDeclarationAnsi(&view);
    let mut imported =
        imports::imported_symbols(node.clone(), tree, packages, packages.unit(path))?;
    imported.declarations_only = declarations_only;
    let name = module_name_from_node(node.clone(), tree)?;
    let _timescales = timescales::install_for_tree(packages.timescales.clone(), tree)?;
    Module::from_scope_node_with_parameter_overrides(
        node,
        tree,
        &name,
        overrides,
        &InterfaceLookup {
            local: interfaces,
            extra: &ModuleInterfaces::default(),
        },
        Arc::new(imported),
        true,
    )
}

pub(super) fn signature(
    declaration: &sv_parser::InterfaceDeclarationAnsi,
    tree: &SyntaxTree,
    path: &Path,
    packages: &packages::Packages,
    name: String,
    members: &[String],
    names: &HashSet<String>,
) -> Converted<crate::ir::Module> {
    let mut view = module_view(declaration)?;
    // Publishing a type contract must not require lowering the interface's
    // executable processes. Keep declarations and their original locations.
    view.nodes.2.retain(|item| match item {
        sv_parser::NonPortModuleItem::TimeunitsDeclaration(_) => true,
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => matches!(
            item.as_ref(),
            sv_parser::ModuleOrGenerateItem::ModuleItem(item)
                if matches!(&item.nodes.1, sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(_))
        ),
        _ => false,
    });
    let node = RefNode::ModuleDeclarationAnsi(&view);
    let imported = imports::imported_symbols(node.clone(), tree, packages, packages.unit(path))?;
    let _timescales = timescales::install_for_tree(packages.timescales.clone(), tree)?;
    let empty = ModuleInterfaces::default();
    let interfaces = InterfaceLookup {
        local: &empty,
        extra: &empty,
    };
    let mut module = Module::from_module_node_with_parameter_overrides(
        node,
        tree,
        "",
        &HashMap::default(),
        &interfaces,
        Arc::new(imported.clone()),
    )?;
    // Internal signal lowering freezes localparam-dependent dimensions for
    // execution. A public declaration must retain those dependencies so its
    // consumers can specialize the interface with different header values.
    let _scope = scope::install(Arc::new(imported.clone()));
    let _functions = const_functions::install(module.constant_functions.clone());
    let mut env = imported.const_env.clone();
    extend_const_env_with_parameters(&mut env, &module.parameters);
    let aliases =
        type_aliases_from_module_node_with_env(RefNode::ModuleDeclarationAnsi(&view), tree, &env)?;
    let declarations = declarations::signals_from_module_node_with_declaration_ranges(
        RefNode::ModuleDeclarationAnsi(&view),
        tree,
        &env,
        &aliases,
        true,
    )?;
    let names = names
        .iter()
        .map(|name| (name.clone(), format!("h${name}")))
        .collect::<HashMap<_, _>>();
    let rename = scope::Renamer { names: &names };
    module.name = name;
    module.ports = members
        .iter()
        .map(|name| {
            let signal = declarations
                .iter()
                .find(|signal| signal.name() == name)
                .ok_or_else(|| unsupported(format!("interface member `{name}`")))?;
            Ok(Port::new(
                rename.name(name),
                PortDirection::Input,
                rename.r#type(signal.r#type.clone()),
                signal.is_net,
            ))
        })
        .collect::<Converted<_>>()?;
    module
        .signals
        .retain(|signal| !members.contains(&signal.name));
    for signal in &mut module.signals {
        signal.name = rename.name(&signal.name);
        signal.r#type = rename.r#type(signal.r#type.clone());
    }
    module.parameters = module
        .parameters
        .into_iter()
        .map(|p| rename.parameter(p))
        .collect();
    module.constant_functions = module
        .constant_functions
        .renamed(&mut |name| rename.name(name), &mut |stmt| rename.stmt(stmt));
    module.symbolic_functions = module
        .symbolic_functions
        .into_iter()
        .map(|(name, function)| (rename.name(&name), rename.function(function)))
        .collect();
    module.declaration_subroutines = module
        .declaration_subroutines
        .into_iter()
        .map(|subroutine| {
            let subroutine = subroutine.map(
                &mut Into::into,
                &mut |value| match value {
                    crate::ir::LValue::Ident(name) => LValue::Ident(name),
                    crate::ir::LValue::Select {
                        name,
                        msb,
                        lsb,
                        array_slice_width,
                        array_slice_reversed,
                    } => LValue::Select {
                        name,
                        msb: msb.into(),
                        lsb: lsb.into(),
                        array_slice_width: array_slice_width.map(Into::into),
                        array_slice_reversed,
                        signed: false,
                        is_2state: false,
                    },
                },
                &mut |ty| ty.map_range_expressions(|expr| rename.const_expr(expr.into()).into()),
            );
            rename
                .subroutine(subroutine)
                .map(&mut Into::into, &mut Into::into, &mut |ty| ty)
        })
        .collect();
    module.assignments.clear();
    module.comb_processes.clear();
    module.ff_processes.clear();
    module.initial_processes.clear();
    module.subroutines.clear();
    module.locals.clear();
    module.instances.clear();
    let ir = crate::analyze::analyze_source(Source {
        modules: vec![module],
    })?;
    ir.modules()
        .first()
        .cloned()
        .ok_or_else(|| unsupported("interface signature"))
}
