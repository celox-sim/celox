//! Time scopes and SV 2023 timescale queries (IEEE 1800-2023 3.14, 20.4.1).

use super::*;

/// Celox's implementation-defined defaults, in powers of ten seconds.
const DEFAULT: Scale = Scale {
    unit: -9,
    precision: -12,
};

#[derive(Debug, Clone, Copy)]
struct Scale {
    unit: i32,
    precision: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Definition,
    Package,
}

#[derive(Debug, Clone)]
struct Scope {
    name: String,
    kind: ScopeKind,
    start: usize,
    end: usize,
    scale: Scale,
    time_items_end: usize,
    specified: (bool, bool),
    instances: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct Unit {
    scale: Scale,
    scopes: Vec<Scope>,
}

#[derive(Debug, Default)]
pub(super) struct Timescales {
    // Identical preprocessed text has identical scope offsets. Content keys
    // remain valid when ParsedSource values move or their trees are dropped.
    units: HashMap<String, Arc<Unit>>,
    definitions: HashMap<String, Scope>,
    packages: HashMap<String, Scope>,
    precision: Option<i32>,
    instantiated: HashSet<String>,
}

#[cfg(test)]
thread_local! {
    static COLLECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static DESIGN: RefCell<QueryContext> = RefCell::new(QueryContext::default());
}

#[derive(Default)]
struct QueryContext {
    design: Arc<Timescales>,
    prepared: Option<PreparedTree>,
}

struct PreparedTree {
    address: usize,
    unit: Arc<Unit>,
    local: Option<Timescales>,
}

pub(super) struct Installed<'a> {
    previous: QueryContext,
    // A prepared tree's address is used only while this guard borrows it.
    // The guard must also stay on the thread whose context it installed.
    _tree: Option<&'a SyntaxTree>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for Installed<'_> {
    fn drop(&mut self) {
        DESIGN.with(|current| {
            *current.borrow_mut() = std::mem::take(&mut self.previous);
        });
    }
}
pub(super) fn install(design: Arc<Timescales>) -> Installed<'static> {
    install_context(
        QueryContext {
            design,
            prepared: None,
        },
        None,
    )
}

pub(super) fn install_for_tree(
    design: Arc<Timescales>,
    tree: &SyntaxTree,
) -> Converted<Installed<'_>> {
    let source = key(tree);
    // Type overrides change the preprocessed text and offsets. Collect that
    // tree once per analysis; queries keep the original cross-file design.
    let local = (!design.units.contains_key(source))
        .then(|| Timescales::collect(&[(tree, Path::new(""))]))
        .transpose()?;
    let unit = design
        .units
        .get(source)
        .or_else(|| local.as_ref().and_then(|local| local.units.get(source)))
        .unwrap()
        .clone();
    let prepared = Some(PreparedTree {
        address: tree as *const SyntaxTree as usize,
        unit,
        local,
    });
    Ok(install_context(
        QueryContext { design, prepared },
        Some(tree),
    ))
}

fn install_context(context: QueryContext, tree: Option<&SyntaxTree>) -> Installed<'_> {
    Installed {
        previous: DESIGN.with(|current| current.replace(context)),
        _tree: tree,
        _thread: std::marker::PhantomData,
    }
}

fn key(tree: &SyntaxTree) -> &str {
    tree.into_iter()
        .find_map(|node| match node {
            RefNode::SourceText(source) => tree.get_str(source),
            _ => None,
        })
        .unwrap_or_default()
}

/// A token's offset, excluding leading/trailing whitespace and directives.
fn offset(node: RefNode<'_>) -> Option<usize> {
    let mut whitespace = 0;
    for event in node.into_iter().event() {
        match event {
            sv_parser::NodeEvent::Enter(RefNode::WhiteSpace(_)) => whitespace += 1,
            sv_parser::NodeEvent::Leave(RefNode::WhiteSpace(_)) => whitespace -= 1,
            sv_parser::NodeEvent::Enter(RefNode::Locate(locate)) if whitespace == 0 => {
                return Some(locate.offset);
            }
            _ => {}
        }
    }
    None
}

fn exponent(number: &str, suffix: &str) -> Converted<i32> {
    let number = number.replace('_', "");
    let number = number.trim();
    let number = number
        .split_once('.')
        .map_or(Some(number), |(whole, fractional)| {
            fractional.chars().all(|c| c == '0').then_some(whole)
        });
    let magnitude = match number.and_then(|number| number.parse::<u16>().ok()) {
        Some(1) => 0,
        Some(10) => 1,
        Some(100) => 2,
        _ => {
            return Err(unsupported(
                "time unit or precision magnitude (expected 1, 10, or 100)",
            ));
        }
    };
    let base = match suffix.trim() {
        "s" => 0,
        "ms" => -3,
        "us" => -6,
        "ns" => -9,
        "ps" => -12,
        "fs" => -15,
        _ => return Err(unsupported("time unit or precision suffix")),
    };
    Ok(base + magnitude)
}

fn literal(value: &sv_parser::TimeLiteral, tree: &SyntaxTree) -> Converted<i32> {
    match value {
        sv_parser::TimeLiteral::Unsigned(value) => exponent(
            tree.get_str_trim(&value.nodes.0).unwrap_or_default(),
            tree.get_str_trim(&value.nodes.1).unwrap_or_default(),
        ),
        sv_parser::TimeLiteral::FixedPoint(value) => exponent(
            tree.get_str_trim(&value.nodes.0).unwrap_or_default(),
            tree.get_str_trim(&value.nodes.1).unwrap_or_default(),
        ),
    }
}

fn declaration(
    value: &sv_parser::TimeunitsDeclaration,
    tree: &SyntaxTree,
) -> Converted<(Option<i32>, Option<i32>)> {
    use sv_parser::TimeunitsDeclaration::*;
    Ok(match value {
        Timeunit(v) => (
            Some(literal(&v.nodes.1, tree)?),
            v.nodes
                .2
                .as_ref()
                .map(|(_, p)| literal(p, tree))
                .transpose()?,
        ),
        Timeprecision(v) => (None, Some(literal(&v.nodes.1, tree)?)),
        TimeunitTimeprecision(v) => (
            Some(literal(&v.nodes.1, tree)?),
            Some(literal(&v.nodes.4, tree)?),
        ),
        TimeprecisionTimeunit(v) => (
            Some(literal(&v.nodes.4, tree)?),
            Some(literal(&v.nodes.1, tree)?),
        ),
    })
}

fn merge(
    target: &mut (Option<i32>, Option<i32>),
    value: (Option<i32>, Option<i32>),
) -> Converted<()> {
    for (previous, next) in [(&mut target.0, value.0), (&mut target.1, value.1)] {
        if let Some(next) = next {
            if previous.is_some_and(|previous| previous != next) {
                return Err(unsupported(
                    "conflicting timeunit or timeprecision declarations",
                ));
            }
            *previous = Some(next);
        }
    }
    Ok(())
}

/// Only direct time declarations and compiler directives leave the initial
/// time-declaration region open. `sv-parser` stores `timescale` in whitespace,
/// outside these item lists; its top-level `resetall` description is handled
/// explicitly below. Nested declarations never extend the initial region.
fn time_item(node: &RefNode<'_>) -> bool {
    use sv_parser::{
        InterfaceItem, ModuleItem, NonPortInterfaceItem, NonPortModuleItem, NonPortProgramItem,
        PackageItem, ProgramItem,
    };
    match node {
        RefNode::NonPortModuleItem(NonPortModuleItem::TimeunitsDeclaration(_))
        | RefNode::NonPortInterfaceItem(NonPortInterfaceItem::TimeunitsDeclaration(_))
        | RefNode::NonPortProgramItem(NonPortProgramItem::TimeunitsDeclaration(_))
        | RefNode::PackageItem(PackageItem::TimeunitsDeclaration(_))
        | RefNode::Description(sv_parser::Description::ResetallCompilerDirective(_)) => true,
        RefNode::ModuleItem(ModuleItem::NonPortModuleItem(item)) => {
            matches!(&**item, NonPortModuleItem::TimeunitsDeclaration(_))
        }
        RefNode::InterfaceItem(InterfaceItem::NonPortInterfaceItem(item)) => {
            matches!(&**item, NonPortInterfaceItem::TimeunitsDeclaration(_))
        }
        RefNode::ProgramItem(ProgramItem::NonPortProgramItem(item)) => {
            matches!(&**item, NonPortProgramItem::TimeunitsDeclaration(_))
        }
        RefNode::Description(sv_parser::Description::PackageItem(item)) => {
            matches!(item.nodes.1, PackageItem::TimeunitsDeclaration(_))
        }
        _ => false,
    }
}

fn scope_node(node: RefNode<'_>, tree: &SyntaxTree) -> Option<Scope> {
    let kind = if matches!(node, RefNode::PackageDeclaration(_)) {
        ScopeKind::Package
    } else {
        ScopeKind::Definition
    };
    let (name, end, items) = match &node {
        RefNode::ModuleDeclarationAnsi(v) => (
            identifier_text(RefNode::ModuleIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::NonPortModuleItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::ModuleDeclarationNonansi(v) => (
            identifier_text(RefNode::ModuleIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::ModuleItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::InterfaceDeclarationAnsi(v) => (
            identifier_text(RefNode::InterfaceIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::NonPortInterfaceItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::InterfaceDeclarationNonansi(v) => (
            identifier_text(RefNode::InterfaceIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::InterfaceItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::ProgramDeclarationAnsi(v) => (
            identifier_text(RefNode::ProgramIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::NonPortProgramItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::ProgramDeclarationNonansi(v) => (
            identifier_text(RefNode::ProgramIdentifier(&v.nodes.0.nodes.3), tree)?,
            v.nodes.3.nodes.0.offset,
            v.nodes
                .2
                .iter()
                .map(RefNode::ProgramItem)
                .collect::<Vec<_>>(),
        ),
        RefNode::PackageDeclaration(v) => (
            identifier_text(RefNode::PackageIdentifier(&v.nodes.3), tree)?,
            v.nodes.7.nodes.0.offset,
            v.nodes
                .6
                .iter()
                .map(|(_, item)| RefNode::PackageItem(item))
                .collect::<Vec<_>>(),
        ),
        _ => return None,
    };
    Some(Scope {
        name,
        kind,
        start: offset(node)?,
        end,
        scale: DEFAULT,
        time_items_end: items
            .into_iter()
            .find(|item| !time_item(item))
            .and_then(offset)
            .unwrap_or(end),
        specified: (false, false),
        instances: HashMap::default(),
    })
}

impl Timescales {
    pub(super) fn collect(trees: &[(&SyntaxTree, &Path)]) -> Converted<Self> {
        #[cfg(test)]
        COLLECTIONS.with(|count| count.set(count.get() + 1));
        let mut design = Self::default();
        // Each bit records whether a design element uses a default (1) or an
        // explicit/inherited setting (2), independently for unit and precision.
        let mut specified = [0u8; 2];
        for (tree, _) in trees {
            let mut scopes: Vec<_> = tree
                .into_iter()
                .filter_map(|node| scope_node(node, tree))
                .collect();
            scopes.sort_by_key(|s| s.start);
            let mut declarations = vec![(None, None); scopes.len()];
            let mut unit_decl = (None, None);
            let unit_time_items_end = tree
                .into_iter()
                .find_map(|node| match node {
                    RefNode::SourceText(source) => source
                        .nodes
                        .2
                        .iter()
                        .map(RefNode::Description)
                        .find(|item| !time_item(item))
                        .and_then(offset),
                    _ => None,
                })
                .unwrap_or(usize::MAX);
            let mut directives = Vec::new();
            let mut generate_depth = 0;
            for event in tree.into_iter().event() {
                let node = match event {
                    sv_parser::NodeEvent::Enter(RefNode::GenerateBlock(_)) => {
                        generate_depth += 1;
                        continue;
                    }
                    sv_parser::NodeEvent::Leave(RefNode::GenerateBlock(_)) => {
                        generate_depth -= 1;
                        continue;
                    }
                    sv_parser::NodeEvent::Enter(node) => node,
                    _ => continue,
                };
                match node {
                    RefNode::TimeunitsDeclaration(v) => {
                        let value = declaration(v, tree)?;
                        if let Some(p) = value.1 {
                            design.precision = Some(design.precision.map_or(p, |old| old.min(p)));
                        }
                        let at = offset(RefNode::TimeunitsDeclaration(v)).unwrap_or_default();
                        let target = scopes
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| s.start <= at && at < s.end)
                            .min_by_key(|(_, s)| s.end - s.start)
                            .map(|(i, _)| i);
                        let time_items_end =
                            target.map_or(unit_time_items_end, |i| scopes[i].time_items_end);
                        let previous = target.map_or(&mut unit_decl, |i| &mut declarations[i]);
                        if (previous.0.is_none() && value.0.is_some()
                            || previous.1.is_none() && value.1.is_some())
                            && at >= time_items_end
                        {
                            return Err(unsupported(
                                "timeunit or timeprecision first declared after other scope items",
                            ));
                        }
                        merge(previous, value)?;
                    }
                    RefNode::TimescaleCompilerDirective(v) => {
                        let scale = Scale {
                            unit: exponent(
                                tree.get_str_trim(&v.nodes.2).unwrap_or_default(),
                                tree.get_str_trim(&v.nodes.3).unwrap_or_default(),
                            )?,
                            precision: exponent(
                                tree.get_str_trim(&v.nodes.5).unwrap_or_default(),
                                tree.get_str_trim(&v.nodes.6).unwrap_or_default(),
                            )?,
                        };
                        if scale.precision > scale.unit {
                            return Err(unsupported("timeprecision coarser than timeunit"));
                        }
                        design.precision = Some(
                            design
                                .precision
                                .map_or(scale.precision, |old| old.min(scale.precision)),
                        );
                        directives.push((v.nodes.1.nodes.0.offset, Some(scale)));
                    }
                    RefNode::ResetallCompilerDirective(v) => {
                        directives.push((v.nodes.1.nodes.0.offset, None))
                    }
                    RefNode::ModuleInstantiation(v) => {
                        let module = identifier_text(RefNode::ModuleIdentifier(&v.nodes.0), tree)
                            .unwrap_or_default();
                        design.instantiated.insert(module.clone());
                        // Keep generated instances out of the parent namespace.
                        // Resolving selected generate scopes requires elaborated paths.
                        if generate_depth > 0 {
                            continue;
                        }
                        let at = offset(RefNode::ModuleInstantiation(v)).unwrap_or_default();
                        if let Some(scope) = scopes
                            .iter_mut()
                            .filter(|s| s.start <= at && at < s.end)
                            .min_by_key(|s| s.end - s.start)
                        {
                            let module =
                                identifier_text(RefNode::ModuleIdentifier(&v.nodes.0), tree)
                                    .unwrap_or_default();
                            for instance in v.nodes.2.contents() {
                                // Array and generate selections are resolved by elaboration, not by
                                // interpreting an arbitrary expression as a design-element name.
                                if !instance.nodes.0.nodes.1.is_empty() {
                                    continue;
                                }
                                if let Some(name) = identifier_text(
                                    RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
                                    tree,
                                ) {
                                    scope.instances.insert(name, module.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            directives.sort_by_key(|(at, _)| *at);
            let unit = Scale {
                unit: unit_decl.0.unwrap_or(DEFAULT.unit),
                precision: unit_decl.1.unwrap_or(DEFAULT.precision),
            };
            if unit.precision > unit.unit {
                return Err(unsupported("timeprecision coarser than timeunit"));
            }
            for i in 0..scopes.len() {
                let enclosing = scopes[..i]
                    .iter()
                    .filter(|s| s.start < scopes[i].start && scopes[i].end < s.end)
                    .min_by_key(|s| s.end - s.start)
                    .map(|s| (s.scale, s.specified));
                let inherited = enclosing
                    .or_else(|| {
                        directives
                            .iter()
                            .rev()
                            .find(|(at, _)| *at < scopes[i].start)
                            .and_then(|(_, scale)| scale.map(|scale| (scale, (true, true))))
                    })
                    .unwrap_or((unit, (unit_decl.0.is_some(), unit_decl.1.is_some())));
                let (inherited, inherited_specified) = inherited;
                scopes[i].specified = (
                    declarations[i].0.is_some() || inherited_specified.0,
                    declarations[i].1.is_some() || inherited_specified.1,
                );
                specified[0] |= if scopes[i].specified.0 { 2 } else { 1 };
                specified[1] |= if scopes[i].specified.1 { 2 } else { 1 };
                scopes[i].scale = Scale {
                    unit: declarations[i].0.unwrap_or(inherited.unit),
                    precision: declarations[i].1.unwrap_or(inherited.precision),
                };
                if scopes[i].scale.precision > scopes[i].scale.unit {
                    return Err(unsupported("timeprecision coarser than timeunit"));
                }
                match scopes[i].kind {
                    ScopeKind::Definition => &mut design.definitions,
                    ScopeKind::Package => &mut design.packages,
                }
                .insert(scopes[i].name.clone(), scopes[i].clone());
            }
            design.units.insert(
                key(tree).to_string(),
                Arc::new(Unit {
                    scale: unit,
                    scopes,
                }),
            );
        }
        if specified.contains(&3) {
            return Err(unsupported(
                "mixed explicit and default time units or precisions across design elements",
            ));
        }
        Ok(design)
    }

    pub(super) fn query(
        &self,
        call: &sv_parser::SystemTfCall,
        tree: &SyntaxTree,
    ) -> Converted<i32> {
        let source = key(tree);
        let local = (!self.units.contains_key(source))
            .then(|| Self::collect(&[(tree, Path::new(""))]))
            .transpose()?;
        let unit = self
            .units
            .get(source)
            .or_else(|| local.as_ref().and_then(|local| local.units.get(source)))
            .unwrap();
        self.query_in_unit(call, tree, unit, local.as_ref())
    }

    fn query_in_unit(
        &self,
        call: &sv_parser::SystemTfCall,
        tree: &SyntaxTree,
        unit: &Unit,
        local: Option<&Self>,
    ) -> Converted<i32> {
        let (name, args) =
            system_tf_call_parts(call, tree).ok_or_else(|| unsupported("timescale query name"))?;
        system_functions::check_call(
            name,
            args.as_deref(),
            system_functions::CallSite::Expression,
        )?;
        let at = offset(RefNode::SystemTfCall(call)).unwrap_or_default();
        let current = unit
            .scopes
            .iter()
            .filter(|s| s.start <= at && at < s.end)
            .min_by_key(|s| s.end - s.start);
        let scale = if args.as_ref().is_some_and(|args| args.is_empty()) {
            current.map_or(unit.scale, |s| s.scale)
        } else {
            let argument = match call {
                sv_parser::SystemTfCall::ArgExpression(call) => {
                    call.nodes.1.nodes.1.0.contents()[0].as_ref()
                }
                sv_parser::SystemTfCall::ArgOptionl(call) => {
                    call.nodes
                        .1
                        .as_ref()
                        .and_then(|paren| match &paren.nodes.1 {
                            sv_parser::ListOfArguments::Ordered(args) => {
                                args.nodes.0.contents()[0].as_ref()
                            }
                            _ => None,
                        })
                }
                _ => None,
            }
            .ok_or_else(|| unsupported("timescale query design-element argument"))?;
            match tree.get_str_trim(argument).unwrap_or_default().trim() {
                "$unit" => unit.scale,
                "$root" => {
                    let precision = self
                        .precision
                        .or_else(|| local.and_then(|local| local.precision))
                        .unwrap_or(DEFAULT.precision);
                    Scale {
                        unit: precision,
                        precision,
                    }
                }
                _ => {
                    let sv_parser::Expression::Primary(primary) = argument else {
                        return Err(unsupported("timescale query design-element argument"));
                    };
                    let sv_parser::Primary::Hierarchical(primary) = &**primary else {
                        return Err(unsupported("timescale query design-element argument"));
                    };
                    if primary.nodes.0.as_ref().is_some_and(|qualifier| !matches!(qualifier, sv_parser::ClassQualifierOrPackageScope::ClassQualifier(c) if c.nodes.0.is_none() && c.nodes.1.is_none())) || !primary.nodes.2.nodes.1.nodes.0.is_empty() || primary.nodes.2.nodes.2.is_some() { return Err(unsupported("timescale query design-element argument")); }
                    let hierarchy = &primary.nodes.1;
                    let mut path = Vec::new();
                    for (identifier, select, _) in &hierarchy.nodes.1 {
                        if !select.nodes.0.is_empty() {
                            return Err(unsupported("selected design element in timescale query"));
                        }
                        path.push(
                            identifier_text(RefNode::Identifier(identifier), tree)
                                .ok_or_else(|| unsupported("timescale query identifier"))?,
                        );
                    }
                    path.push(
                        identifier_text(RefNode::Identifier(&hierarchy.nodes.2), tree)
                            .ok_or_else(|| unsupported("timescale query identifier"))?,
                    );
                    let first = &path[0];
                    let (mut target, start) = if hierarchy.nodes.0.is_none()
                        && current.is_some_and(|s| s.instances.contains_key(first))
                    {
                        (current.unwrap(), 0)
                    } else if !self.instantiated.contains(first)
                        && let Some(root) = self.definitions.get(first).or_else(|| {
                            unit.scopes
                                .iter()
                                .find(|s| s.kind == ScopeKind::Definition && s.name == *first)
                        })
                    {
                        // A root instance is part of the hierarchy and takes
                        // precedence over a same-named global package.
                        (root, 1)
                    } else if hierarchy.nodes.0.is_none()
                        && path.len() == 1
                        && let Some(package) = self.packages.get(first).or_else(|| {
                            unit.scopes
                                .iter()
                                .find(|s| s.kind == ScopeKind::Package && s.name == *first)
                        })
                    {
                        // Package and definition names have separate global
                        // namespaces (3.13). An instantiated module type with
                        // this spelling does not hide the package.
                        (package, 1)
                    } else {
                        if self.instantiated.contains(first)
                            && current.is_none_or(|scope| scope.name != *first)
                        {
                            return Err(unsupported(format!(
                                "timescale query names module type `{first}`, not an instance"
                            )));
                        }
                        (
                            self.definitions
                                .get(first)
                                .or_else(|| {
                                    unit.scopes.iter().find(|s| {
                                        s.kind == ScopeKind::Definition && s.name == *first
                                    })
                                })
                                .or_else(|| self.packages.get(first))
                                .or_else(|| {
                                    unit.scopes
                                        .iter()
                                        .find(|s| s.kind == ScopeKind::Package && s.name == *first)
                                })
                                .ok_or_else(|| {
                                    unsupported(format!(
                                        "unknown timescale query design element `{first}`"
                                    ))
                                })?,
                            1,
                        )
                    };
                    for part in &path[start..] {
                        let module = target.instances.get(part).ok_or_else(|| {
                            unsupported(format!("unknown timescale query design element `{part}`"))
                        })?;
                        target = self
                            .definitions
                            .get(module)
                            .or_else(|| {
                                unit.scopes
                                    .iter()
                                    .find(|s| s.kind == ScopeKind::Definition && s.name == *module)
                            })
                            .ok_or_else(|| {
                                unsupported(format!("unknown timescale query module `{module}`"))
                            })?;
                    }
                    target.scale
                }
            }
        };
        Ok(if name == "$timeunit" {
            scale.unit
        } else {
            scale.precision
        })
    }
}

pub(super) fn query(call: &sv_parser::SystemTfCall, tree: &SyntaxTree) -> Converted<i32> {
    DESIGN.with(|context| {
        let context = context.borrow();
        if let Some(prepared) = &context.prepared
            && prepared.address == tree as *const SyntaxTree as usize
        {
            context
                .design
                .query_in_unit(call, tree, &prepared.unit, prepared.local.as_ref())
        } else {
            context.design.query(call, tree)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_and_hidden_query_operands() {
        for expression in [
            "$timeunit(x)",
            "$timeprecision(1)",
            "$timeunit(x + 1)",
            "$timeunit(missing)",
            "$timeunit(1, 2)",
            "$timeprecision(,)",
            "$timeunit(.scope(x))",
            "$bits($timeunit(x))",
        ] {
            let code = format!(
                "module Top(input int x, output int y); timeunit 1ns / 1ps; localparam unused = {expression}; assign y = 0; endmodule"
            );
            assert!(
                crate::analyze_source(&code, Path::new("invalid_query.sv")).is_err(),
                "accepted {expression}"
            );
        }
    }

    #[test]
    fn rejects_invalid_time_declarations() {
        for items in [
            "timeunit 2ns;",
            "timeunit 1ps / 1ns;",
            "timeunit 1ns / 1ps; timeunit 10ns;",
            "logic x; timeunit 1ns;",
            "timeunit 1ns; logic x; timeprecision 1ps;",
        ] {
            let code = format!("module Top(); {items} endmodule");
            assert!(
                crate::analyze_source(&code, Path::new("invalid_scale.sv")).is_err(),
                "accepted {items}"
            );
        }
    }

    #[test]
    fn default_and_positive_exponents_keep_their_declared_values() {
        for (prefix, unit, precision, root) in
            [("", -9, -12, -12), ("timeunit 100s / 10s;", 2, 1, 1)]
        {
            let code = format!(
                "module Top(); {prefix} localparam U=$timeunit(); localparam P=$timeprecision(); localparam R=$timeprecision($root); endmodule"
            );
            let ir = crate::analyze_source(&code, Path::new("values.sv")).unwrap();
            let values: Vec<_> = ir.modules()[0]
                .parameters()
                .iter()
                .map(|p| p.resolved_value())
                .collect();
            assert_eq!(values, [Some(unit), Some(precision), Some(root)]);
        }
    }

    #[test]
    fn generated_instances_are_not_visible_in_the_parent_scope() {
        let source = "module Child(); timeunit 1ns / 1ps; endmodule module Top(output int y); timeunit 1ns / 1ps; if (1) begin : g Child child(); end assign y = $timeunit(child); endmodule";
        assert!(crate::analyze_source(source, Path::new("generate_scope.sv")).is_err());
    }

    #[test]
    fn allows_separate_time_declarations_before_other_items() {
        for code in [
            "module Top(); timeunit 1ns; timeunit 1ns; timeprecision 1ps; localparam U=$timeunit(); localparam P=$timeprecision(); endmodule",
            "timeunit 1ns; timeunit 1ns; timeprecision 1ps; module Top(); localparam U=$timeunit($unit); localparam P=$timeprecision($unit); endmodule",
            "package P; timeprecision 1ps; timeprecision 1ps; timeunit 1ns; function automatic int u(); return $timeunit(); endfunction endpackage module Top(); timeunit 1ns / 1ps; localparam U=P::u(); endmodule",
        ] {
            let result = crate::analyze_source(code, Path::new("separate_times.sv"));
            assert!(result.is_ok(), "{result:?}");
        }
    }

    #[test]
    fn rejects_mixed_explicit_and_default_design_elements() {
        for code in [
            "module A(); timeunit 1ns / 1ns; endmodule module B(); endmodule",
            "module B(); endmodule module A(); timeunit 1ns / 1ns; endmodule",
            "module A(); timeunit 1ns / 1ns; endmodule package P; endpackage",
        ] {
            let tree = crate::syntax::parse_source(code, Path::new("mixed_times.sv")).unwrap();
            assert!(
                Timescales::collect(&[(&tree, Path::new("mixed_times.sv"))]).is_err(),
                "accepted {code}"
            );
        }
        let explicit = crate::syntax::parse_source(
            "module A(); timeunit 1ns / 1ns; endmodule",
            Path::new("a.sv"),
        )
        .unwrap();
        let default =
            crate::syntax::parse_source("module B(); endmodule", Path::new("b.sv")).unwrap();
        assert!(
            Timescales::collect(&[
                (&explicit, Path::new("a.sv")),
                (&default, Path::new("b.sv"))
            ])
            .is_err()
        );
    }

    #[test]
    fn keeps_package_and_definition_time_scopes_independent() {
        let code = "module P(); timeunit 1us / 1ns; endmodule package P; timeunit 100ns / 10ps; endpackage module Top(); timeunit 1ns / 1ps; P p_inst(); localparam U=$timeunit(p_inst); localparam T=$timeprecision(p_inst); endmodule";
        let result = crate::analyze_source(code, Path::new("namespaces.sv")).unwrap();
        let top = result.modules().iter().find(|m| m.name() == "Top").unwrap();
        let values: Vec<_> = top
            .parameters()
            .iter()
            .map(|p| p.resolved_value())
            .collect();
        assert_eq!(values, [Some(-6), Some(-9)]);
    }

    #[test]
    fn root_instances_take_precedence_over_namesake_packages() {
        let code = "package P; timeunit 100ns / 10ps; endpackage module P(); timeunit 1us / 1ns; localparam U=$timeunit(P); localparam T=$timeprecision(P); endmodule module Top(); timeunit 1ms / 1us; localparam U=$timeunit(P); localparam T=$timeprecision(P); endmodule";
        let result = crate::analyze_source(code, Path::new("root_package_query.sv")).unwrap();
        for module in result.modules() {
            let values: Vec<_> = module
                .parameters()
                .iter()
                .map(|p| p.resolved_value())
                .collect();
            assert_eq!(values, [Some(-6), Some(-9)], "{}", module.name());
        }
    }

    #[test]
    fn interface_macro_queries_are_not_rewritten_into_shared_definitions() {
        let prefix = "`define SCALE $timeunit()\ninterface I; timeunit 100ps / 10fs; function automatic int u(); return `SCALE; endfunction endinterface";
        let code =
            format!("{prefix} module Top(); timeunit 1ms / 1us; localparam U=`SCALE; endmodule");
        let rewritten =
            crate::elaborate_interfaces(&[(&code, Path::new("unused_macro_interface.sv"))])
                .unwrap()
                .unwrap();
        let ir =
            crate::analyze_source(&rewritten[0], Path::new("unused_macro_interface.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(-3));
        let code = format!("{prefix} module Top(); timeunit 1ms / 1us; I i(); endmodule");
        let error = crate::elaborate_interfaces(&[(&code, Path::new("used_macro_interface.sv"))])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("macro or compiler directive in interface `I`"),
            "{error}"
        );
    }

    #[test]
    fn prepared_tree_context_is_restored_after_nested_analysis_and_errors() {
        let a = crate::syntax::parse_source(
            "module A(); timeunit 1us / 1ns; localparam U=$timeunit(); endmodule",
            Path::new("a.sv"),
        )
        .unwrap();
        let b = crate::syntax::parse_source(
            "module B(); timeunit 1ns / 1ps; localparam U=$timeunit(); endmodule",
            Path::new("b.sv"),
        )
        .unwrap();
        let invalid = crate::syntax::parse_source(
            "module Invalid(); timeunit 2ns; endmodule",
            Path::new("invalid.sv"),
        )
        .unwrap();
        let design = Arc::new(
            Timescales::collect(&[(&a, Path::new("a.sv")), (&b, Path::new("b.sv"))]).unwrap(),
        );
        fn call(tree: &SyntaxTree) -> &sv_parser::SystemTfCall {
            tree.into_iter()
                .find_map(|node| match node {
                    RefNode::SystemTfCall(call) => Some(call),
                    _ => None,
                })
                .unwrap()
        }
        let _outer = install_for_tree(design.clone(), &a).unwrap();
        assert_eq!(query(call(&a), &a).unwrap(), -6);
        {
            let _inner = install_for_tree(design.clone(), &b).unwrap();
            assert_eq!(query(call(&b), &b).unwrap(), -9);
            assert!(install_for_tree(design, &invalid).is_err());
            assert_eq!(query(call(&b), &b).unwrap(), -9);
        }
        assert_eq!(query(call(&a), &a).unwrap(), -6);
    }

    #[test]
    fn typed_specializations_collect_time_scopes_once() {
        let mut code = "timeunit 10ns / 1ps; module Child #(parameter type T = logic)(output T y); timeunit 1us / 1ns;".to_string();
        for i in 0..16 {
            code.push_str(&format!("localparam U{i}=$timeunit(); localparam P{i}=$timeprecision(); localparam R{i}=$timeprecision($root);"));
        }
        code.push_str("assign y = 1; endmodule");
        let source = crate::ParsedSource::parse(&code, Path::new("typed_times.sv")).unwrap();
        let unused = crate::ParsedSource::parse(
            "module Unused(); timeunit 1ns / 1fs; endmodule",
            Path::new("unused_times.sv"),
        )
        .unwrap();
        let packages = crate::ParsedSource::analyze_packages(&[&source, &unused]).unwrap();
        for ty in ["logic [7:0]", "logic [3:0]", "logic [7:0]"] {
            let before = COLLECTIONS.with(std::cell::Cell::get);
            let result = source
                .analyze_module(
                    "Child",
                    &HashMap::default(),
                    &[("T".into(), ty.into())],
                    &ModuleInterfaces::default(),
                    &packages,
                )
                .unwrap();
            let after = COLLECTIONS.with(std::cell::Cell::get);
            assert_eq!(after - before, 1, "recollected time scopes for {ty}");
            let values: Vec<_> = result.modules()[0]
                .parameters()
                .iter()
                .filter_map(|p| p.resolved_value())
                .collect();
            assert_eq!(values, [-6, -9, -15].repeat(16));
        }
    }

    #[test]
    fn resolves_packages_before_rejecting_instantiated_definition_names() {
        let code = "module P(); timeunit 1us / 1ns; endmodule package P; timeunit 100ns / 10ps; endpackage package Q; timeunit 10us / 100ps; endpackage module Q(); timeunit 10ns / 1ps; endmodule module Top(); timeunit 1ns / 1ps; P p_inst(); Q q_inst(); localparam U=$timeunit(P); localparam T=$timeprecision(P); localparam V=$timeunit(Q); localparam S=$timeprecision(Q); endmodule";
        let result = crate::analyze_source(code, Path::new("package_queries.sv")).unwrap();
        let top = result.modules().iter().find(|m| m.name() == "Top").unwrap();
        let values: Vec<_> = top
            .parameters()
            .iter()
            .map(|p| p.resolved_value())
            .collect();
        assert_eq!(values, [Some(-7), Some(-11), Some(-5), Some(-10)]);
        let code = "module P(); timeunit 1us / 1ns; endmodule module Top(); timeunit 1ns / 1ps; P p_inst(); localparam U=$timeunit(P); endmodule";
        assert!(crate::analyze_source(code, Path::new("module_type_query.sv")).is_err());
    }

    #[test]
    fn interface_query_rewriting_preserves_macros_in_module_scopes() {
        let code = "`define U $timeunit()\ninterface I; timeunit 1ns / 1ps; endinterface module Top(); timeunit 1ms / 1us; localparam U=`U; endmodule module Child(); timeunit 1us / 1ns; localparam U=`U; endmodule";
        let elaborated = crate::elaborate_interfaces(&[(code, Path::new("macro_times.sv"))])
            .unwrap()
            .unwrap();
        let result = crate::analyze_source(&elaborated[0], Path::new("macro_times.sv")).unwrap();
        for (name, value) in [("Top", -3), ("Child", -6)] {
            let module = result.modules().iter().find(|m| m.name() == name).unwrap();
            assert_eq!(module.parameters()[0].resolved_value(), Some(value));
        }
    }

    #[test]
    fn interface_query_rewriting_validates_discarded_and_hidden_operands() {
        for expression in [
            "$timeunit(missing);",
            "void'($timeprecision(1));",
            "int x = $bits($timeunit(1));",
        ] {
            let code = format!(
                "interface I; timeunit 1ns / 1ps; function automatic int f(); {expression} return 0; endfunction endinterface module Top(); timeunit 1ns / 1ps; I i(); endmodule"
            );
            assert!(
                crate::elaborate_interfaces(&[(&code, Path::new("invalid_interface_query.sv"))])
                    .is_err(),
                "accepted {expression}"
            );
        }
        for items in [
            "timeunit 1ns / 1ps; timeunit 10ns;",
            "logic x; timeunit 1ns;",
            "timeunit 1ns / 1ps;",
        ] {
            let code = format!("interface I; {items} endinterface module Top(); endmodule");
            assert!(
                crate::elaborate_interfaces(&[(&code, Path::new("invalid_interface_scale.sv"))])
                    .is_err(),
                "accepted {items}"
            );
        }
    }

    #[test]
    fn timescale_directives_leave_unit_time_declarations_open() {
        for declarations in [
            "`timescale 10ns / 1ns\ntimeunit 1ns; timeprecision 1ps;",
            "`timescale 10ns / 1ns\ntimeprecision 1ps; timeunit 1ns;",
            "`timescale 10ns / 1ns\ntimeunit 1ns;\n`timescale 10ns / 1ns\ntimeprecision 1ps;",
        ] {
            let code = format!(
                "{declarations} module Top(); localparam U=$timeunit($unit); localparam P=$timeprecision($unit); localparam M=$timeunit(); localparam T=$timeprecision(); endmodule"
            );
            let ir = crate::analyze_source(&code, Path::new("directive_before_unit.sv")).unwrap();
            let values: Vec<_> = ir.modules()[0]
                .parameters()
                .iter()
                .map(|p| p.resolved_value())
                .collect();
            assert_eq!(
                values,
                [Some(-9), Some(-12), Some(-8), Some(-9)],
                "{declarations}"
            );
        }
    }

    #[test]
    fn queries_declared_and_directive_time_scopes() {
        let source = r#"
            timeunit 100ps / 10fs;
            `timescale 10ns / 100ps
            module Child(output int y); assign y = $timeunit; endmodule
            module Top(output int y);
                timeunit 1ms / 1us;
                Child c();
                assign y = ($timeunit == -3) && ($timeprecision() == -6)
                    && ($timeunit(c) == -8) && ($timeprecision(c) == -10)
                    && ($timeunit($unit) == -10) && ($timeprecision($unit) == -14)
                    && ($timeunit($root) == -14) && ($timeprecision($root) == -14);
            endmodule
        "#;
        let result = crate::analyze_source(source, Path::new("timescales.sv"));
        assert!(result.is_ok(), "{result:?}");
    }
}
