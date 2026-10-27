//! Time scopes and SV 2023 timescale queries (IEEE 1800-2023 3.14, 20.4.1).

use super::*;

/// Celox's implementation-defined defaults, in powers of ten seconds.
const DEFAULT: Scale = Scale {
    unit: -9,
    precision: -12,
};

enum DisplayScope {
    Relative(String),
    Absolute(String),
}

struct ResolvedScope {
    /// The query or `$printtimescale` call that resolved the scope.
    call: system_functions::SystemCall,
    scale: Scale,
    display: DisplayScope,
}

impl ResolvedScope {
    fn value(&self) -> i32 {
        if self.call == system_functions::SystemCall::Function(SystemFunction::Timeunit) {
            self.scale.unit
        } else {
            self.scale.precision
        }
    }

    fn message(&self, interface_prefix: Option<&str>) -> String {
        let literal_name = |name: &str| name.replace('%', "%%");
        let display = match &self.display {
            DisplayScope::Absolute(name) => literal_name(name),
            DisplayScope::Relative(path) => {
                let mut name = "%m".to_string();
                if let Some(prefix) = interface_prefix {
                    name.push('.');
                    name.push_str(prefix);
                }
                if !path.is_empty() {
                    name.push('.');
                    name.push_str(&literal_name(path));
                }
                name
            }
        };
        let time = |exponent: i32| {
            let n = (exponent + 15) as usize;
            format!(
                "{}{}",
                [1, 10, 100][n % 3],
                ["fs", "ps", "ns", "us", "ms", "s"][n / 3]
            )
        };
        // SystemTaskArg::Str stores the source spelling of string contents.
        format!(
            "Time scale of ({display}) is {} / {}",
            time(self.scale.unit),
            time(self.scale.precision)
        )
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
    }
}

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
    parents: Vec<Option<usize>>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct Timescales {
    // Identical expanded tokens have identical native scope positions. Content keys
    // remain valid when ParsedSource values move or their trees are dropped.
    units: HashMap<crate::source_tree::SourceText, Arc<Unit>>,
    definitions: HashMap<String, Scope>,
    packages: HashMap<String, Scope>,
    precision: Option<i32>,
    instantiated: HashSet<String>,
    roots: Option<HashSet<String>>,
    specified: [u8; 2],
    options: crate::AnalysisOptions,
}

/// Time-query state owned by one analysis, shared explicitly by nested scopes.
#[derive(Default)]
pub(super) struct State {
    design: RefCell<QueryContext>,
    interface_instance: RefCell<Option<String>>,
    revision: generate::Revision,
}

pub(super) fn revision(state: &State) -> &generate::Revision {
    &state.revision
}

pub(super) fn analysis_options(state: &State) -> crate::AnalysisOptions {
    state.design.borrow().design.options
}

pub(super) fn with_interface_instance<T>(
    state: &State,
    name: &str,
    analyze: impl FnOnce() -> T,
) -> T {
    struct Restore<'a> {
        state: &'a State,
        previous: Option<String>,
        revision: u64,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            *self.state.interface_instance.borrow_mut() = self.previous.take();
            self.state.revision.restore(self.revision);
        }
    }
    let previous = {
        let mut current = state.interface_instance.borrow_mut();
        let nested = current
            .as_ref()
            .map_or_else(|| name.to_string(), |parent| format!("{parent}.{name}"));
        current.replace(nested)
    };
    let _restore = Restore {
        state,
        previous,
        revision: state.revision.change(),
    };
    analyze()
}

#[cfg(test)]
thread_local! {
    static COLLECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PARENT_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCOPE_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Default)]
struct QueryContext {
    design: Arc<Timescales>,
    prepared: Option<PreparedTree>,
}

struct PreparedTree {
    address: usize,
    unit: Arc<Unit>,
}

pub(super) struct Installed<'a> {
    previous: QueryContext,
    revision: u64,
    state: &'a State,
    // A prepared tree's address is used only while this guard borrows it.
    _tree: Option<&'a SyntaxTree>,
}
impl Drop for Installed<'_> {
    fn drop(&mut self) {
        *self.state.design.borrow_mut() = std::mem::take(&mut self.previous);
        self.state.revision.restore(self.revision);
    }
}
pub(super) fn install_for_tree<'a>(
    state: &'a State,
    design: Arc<Timescales>,
    tree: &'a SyntaxTree,
) -> Converted<Installed<'a>> {
    let source = key(tree);
    // Missing sources and type-specialized trees contribute to the same design.
    // Collect and merge once, before any query or module-body analysis.
    let (design, unit) = if let Some(unit) = design.units.get(&source) {
        let unit = unit.clone();
        (design, unit)
    } else {
        let design = Arc::new(design.including_tree(tree)?);
        let unit = design.units[&source].clone();
        (design, unit)
    };
    let prepared = Some(PreparedTree {
        address: tree as *const SyntaxTree as usize,
        unit,
    });
    Ok(install_context(
        state,
        QueryContext { design, prepared },
        Some(tree),
    ))
}

fn install_context<'a>(
    state: &'a State,
    context: QueryContext,
    tree: Option<&'a SyntaxTree>,
) -> Installed<'a> {
    Installed {
        previous: state.design.replace(context),
        revision: state.revision.change(),
        state,
        _tree: tree,
    }
}

fn key(tree: &SyntaxTree) -> crate::source_tree::SourceText {
    tree.source_text().clone()
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

fn declaration(value: &celox_sv_syntax::TimeDeclaration) -> Converted<(Option<i32>, Option<i32>)> {
    let convert = |value: &celox_sv_syntax::TimeValue| exponent(&value.magnitude, &value.unit);
    Ok((
        value.unit.as_ref().map(convert).transpose()?,
        value.precision.as_ref().map(convert).transpose()?,
    ))
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

/// Scope positions are native token indices, shared by all native expression
/// and declaration IDs, including macro expansions.
fn scope_node(unit: &celox_sv_syntax::Unit, native: &celox_sv_syntax::Declarations) -> Scope {
    Scope {
        name: unit.name.spelling.clone(),
        kind: if unit.kind == celox_sv_syntax::UnitKind::Package {
            ScopeKind::Package
        } else {
            ScopeKind::Definition
        },
        start: unit.tokens.start,
        end: unit.tokens.end,
        scale: DEFAULT,
        time_items_end: unit
            .items
            .iter()
            .map(|id| native.body_item(*id))
            .find(|item| !matches!(item.kind, celox_sv_syntax::BodyItemKind::Time(_)))
            .map_or(unit.body.end, |item| item.tokens.start),
        specified: (false, false),
        instances: HashMap::default(),
    }
}

/// Syntax scopes are sorted by start and either nest or are disjoint. Each
/// scope is pushed and popped at most once, including in flat generated files.
fn scope_parents(scopes: &[Scope]) -> Vec<Option<usize>> {
    let mut parents = Vec::with_capacity(scopes.len());
    let mut active: Vec<usize> = Vec::new();
    for (i, scope) in scopes.iter().enumerate() {
        while let Some(&parent) = active.last() {
            #[cfg(test)]
            PARENT_CHECKS.with(|count| count.set(count.get() + 1));
            if scopes[parent].start < scope.start && scope.end < scopes[parent].end {
                break;
            }
            active.pop();
        }
        parents.push(active.last().copied());
        active.push(i);
    }
    parents
}

fn scope_at(scopes: &[Scope], parents: &[Option<usize>], at: usize) -> Option<usize> {
    let mut candidate = scopes
        .partition_point(|scope| {
            #[cfg(test)]
            SCOPE_CHECKS.with(|count| count.set(count.get() + 1));
            scope.start <= at
        })
        .checked_sub(1);
    while let Some(i) = candidate {
        #[cfg(test)]
        SCOPE_CHECKS.with(|count| count.set(count.get() + 1));
        if at < scopes[i].end {
            return Some(i);
        }
        candidate = parents[i];
    }
    None
}

fn time_declarations(unit: Option<i32>, precision: Option<i32>) -> String {
    fn literal(exponent: i32) -> String {
        let suffix = match exponent.div_euclid(3) {
            -5 => "fs",
            -4 => "ps",
            -3 => "ns",
            -2 => "us",
            -1 => "ms",
            _ => "s",
        };
        format!("{}{suffix}", 10u32.pow(exponent.rem_euclid(3) as u32))
    }
    let mut declarations = String::new();
    if let Some(unit) = unit {
        declarations.push_str(&format!("timeunit {};\n", literal(unit)));
    }
    if let Some(precision) = precision {
        declarations.push_str(&format!("timeprecision {};\n", literal(precision)));
    }
    declarations
}

pub(crate) fn generated_source_declarations(
    sources: &[(&str, &Path)],
    roots: Option<&[&str]>,
    options: crate::AnalysisOptions,
) -> Converted<String> {
    let trees = sources
        .iter()
        .map(|(code, path)| crate::source_tree::parse_source(code, path).map(|tree| (tree, *path)))
        .collect::<Converted<Vec<_>>>()?;
    let mut scales = Timescales::collect_with_options(
        &trees
            .iter()
            .map(|(tree, path)| (tree, *path))
            .collect::<Vec<_>>(),
        options,
    )?;
    if let Some(roots) = roots {
        scales.select_roots(roots);
    }
    Ok(scales.generated_declarations())
}

/// Validate authored native queries, including operands discarded by folding.
pub(super) fn validate_queries(
    tree: &AnalysisContext<'_>,
    ranges: &[celox_sv_syntax::TokenRange],
    excluded: &crate::source_tree::Ranges,
) -> Converted<()> {
    let native = tree.declarations();
    let facts = validation::Facts::of(tree);
    let (callees, statements) = (&facts.callees, &facts.expression_statements);
    let scope = crate::source_tree::Ranges::new(ranges.iter().copied());
    for id in tree.spans().expressions.within(ranges) {
        let at = native.expression(id).tokens.start;
        if !scope.covers(at) || excluded.covers(at) {
            continue;
        }
        if callees.contains(&id) {
            continue;
        }
        let Ok((name, _)) = call_parts(id, native) else {
            continue;
        };
        if matches!(name, "$timeunit" | "$timeprecision" | "$printtimescale") {
            let site = if statements.contains(&id) {
                system_functions::CallSite::Statement(system_functions::Body::Subroutine)
            } else {
                system_functions::CallSite::Expression
            };
            if name == "$printtimescale" {
                print_message(id, tree, site)?;
            } else {
                query(id, tree)?;
            }
        }
    }
    Ok(())
}

fn call_parts(
    root: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Converted<(&str, Option<&[celox_sv_syntax::Argument]>)> {
    use celox_sv_syntax::ExprKind;
    let (callee, arguments) = match &native.expression(root).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, Some(arguments.as_slice())),
        ExprKind::Name(_) => (root, None),
        _ => return Err(unsupported("timescale query name")),
    };
    let ExprKind::Name(name) = &native.expression(callee).kind else {
        return Err(unsupported("timescale query name"));
    };
    Ok((&name.spelling, arguments))
}

pub(super) fn is_query(
    root: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    call_parts(root, native).is_ok_and(|(name, _)| matches!(name, "$timeunit" | "$timeprecision"))
}

fn design_path(
    mut root: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Converted<(bool, Vec<String>)> {
    use celox_sv_syntax::ExprKind;
    let mut path = Vec::new();
    loop {
        match &native.expression(root).kind {
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." => {
                path.push(name.spelling.clone());
                root = *expression;
            }
            ExprKind::Name(name) => {
                path.push(name.spelling.clone());
                break;
            }
            ExprKind::Select { .. } => {
                return Err(unsupported("selected design element in timescale query"));
            }
            _ => return Err(unsupported("timescale query design-element argument")),
        }
    }
    path.reverse();
    let absolute = path.first().is_some_and(|name| name == "$root");
    if absolute {
        path.remove(0);
    }
    if path.is_empty() || path.iter().any(|name| name.starts_with('$')) {
        return Err(unsupported("timescale query design-element argument"));
    }
    Ok((absolute, path))
}

impl Timescales {
    pub(super) fn has_mixed_timescales(&self) -> bool {
        self.specified.contains(&3)
    }

    pub(super) fn scope_scale(&self, name: &str) -> Option<(i32, i32)> {
        let scope = self
            .definitions
            .get(name)
            .or_else(|| self.packages.get(name))?;
        Some((scope.scale.unit, scope.scale.precision))
    }

    pub(super) fn global_precision(&self) -> i32 {
        self.precision.unwrap_or(DEFAULT.precision)
    }
    fn including_tree(&self, tree: &SyntaxTree) -> Converted<Self> {
        let local = Self::collect_with_options(&[(tree, Path::new(""))], self.options)?;
        let specified = [
            self.specified[0] | local.specified[0],
            self.specified[1] | local.specified[1],
        ];
        if specified.contains(&3) && !self.options.allow_mixed_timescales {
            return Err(unsupported(
                "mixed explicit and default time units or precisions across design elements",
            ));
        }
        let mut design = self.clone();
        design.specified = specified;
        design.precision = self.precision.into_iter().chain(local.precision).min();
        design.units.extend(local.units);
        design.definitions.extend(local.definitions);
        design.packages.extend(local.packages);
        design.instantiated.extend(local.instantiated);
        // Explicit simulator roots remain authoritative; otherwise the merged
        // instantiation set determines inferred roots across all sources.
        Ok(design)
    }

    pub(super) fn select_roots(&mut self, roots: &[&str]) {
        self.roots = Some(roots.iter().map(|name| (*name).to_string()).collect());
    }

    fn is_root(&self, name: &str) -> bool {
        self.roots.as_ref().map_or_else(
            || !self.instantiated.contains(name),
            |roots| roots.contains(name),
        )
    }

    pub(super) fn generated_declarations(&self) -> String {
        let unit = self
            .definitions
            .values()
            .filter(|scope| self.is_root(&scope.name))
            .map(|scope| scope.scale.unit)
            .min()
            .unwrap_or(DEFAULT.unit);
        time_declarations(
            (self.specified[0] == 2).then_some(unit),
            (self.specified[1] == 2).then_some(self.precision.unwrap_or(DEFAULT.precision)),
        )
    }

    pub(super) fn declaration_statements(&self, name: &str) -> Converted<String> {
        let scope = self
            .definitions
            .get(name)
            .ok_or_else(|| unsupported(format!("unknown time scope `{name}`")))?;
        Ok(time_declarations(
            scope.specified.0.then_some(scope.scale.unit),
            scope.specified.1.then_some(scope.scale.precision),
        ))
    }

    #[cfg(test)]
    pub(super) fn collect(trees: &[(&SyntaxTree, &Path)]) -> Converted<Self> {
        Self::collect_with_options(trees, crate::AnalysisOptions::default())
    }

    pub(super) fn collect_with_options(
        trees: &[(&SyntaxTree, &Path)],
        options: crate::AnalysisOptions,
    ) -> Converted<Self> {
        #[cfg(test)]
        COLLECTIONS.with(|count| count.set(count.get() + 1));
        let default = match options.default_timescale {
            Some((unit, precision))
                if (-15..=2).contains(&unit)
                    && (-15..=2).contains(&precision)
                    && precision <= unit =>
            {
                Scale { unit, precision }
            }
            Some(_) => return Err(unsupported("invalid default time unit or precision")),
            None => DEFAULT,
        };
        let mut design = Self {
            precision: options.default_timescale.map(|(_, precision)| precision),
            options,
            ..Self::default()
        };
        // Each bit records whether a design element uses a default (1) or an
        // explicit/inherited setting (2), independently for unit and precision.
        let mut specified = [0u8; 2];
        for (tree, _) in trees {
            let native = tree.declarations();
            let mut scopes: Vec<_> = native
                .all_units()
                .map(|unit| scope_node(unit, native))
                .collect();
            scopes.sort_by_key(|s| s.start);
            let parents = scope_parents(&scopes);
            let mut declarations = vec![(None, None); scopes.len()];
            let mut unit_decl = (None, None);
            let unit_time_items_end = native
                .source_items
                .iter()
                .find(|item| {
                    !matches!(item.kind, celox_sv_syntax::SourceItemKind::Declaration(id)
                    if matches!(native.body_item(id).kind, celox_sv_syntax::BodyItemKind::Time(_)))
                })
                .map_or(usize::MAX, |item| item.tokens.start);
            for value in &native.time_declarations {
                let at = value.tokens.start;
                let value = declaration(value)?;
                if let Some(p) = value.1 {
                    design.precision = Some(design.precision.map_or(p, |old| old.min(p)));
                }
                let target = scope_at(&scopes, &parents, at);
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
            let mut directives = Vec::new();
            for directive in &native.time_directives {
                let scale = match &directive.kind {
                    celox_sv_syntax::TimeDirectiveKind::Scale { unit, precision } => {
                        let scale = Scale {
                            unit: exponent(&unit.magnitude, &unit.unit)?,
                            precision: exponent(&precision.magnitude, &precision.unit)?,
                        };
                        if scale.precision > scale.unit {
                            return Err(unsupported("timeprecision coarser than timeunit"));
                        }
                        design.precision = Some(
                            design
                                .precision
                                .map_or(scale.precision, |old| old.min(scale.precision)),
                        );
                        Some(scale)
                    }
                    celox_sv_syntax::TimeDirectiveKind::Reset => None,
                };
                directives.push((directive.tokens.start, scale));
            }
            // Every authored instance contributes to root inference. Only direct
            // unit instances are visible without elaborated generate paths.
            for instance in &native.instantiations {
                design.instantiated.insert(instance.name.spelling.clone());
            }
            for unit in native.all_units() {
                let Some(i) = scopes
                    .binary_search_by_key(&unit.tokens.start, |scope| scope.start)
                    .ok()
                else {
                    continue;
                };
                let mut items = unit.items.clone();
                while let Some(id) = items.pop() {
                    match &native.body_item(id).kind {
                        celox_sv_syntax::BodyItemKind::Region(children) => items.extend(children),
                        celox_sv_syntax::BodyItemKind::Instance(id) => {
                            let instance = native.instantiation(*id);
                            for declarator in &instance.instances {
                                if declarator.dimensions.is_empty() {
                                    scopes[i].instances.insert(
                                        declarator.name.spelling.clone(),
                                        instance.name.spelling.clone(),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            directives.sort_by_key(|(at, _)| *at);
            let unit = Scale {
                unit: unit_decl.0.unwrap_or(default.unit),
                precision: unit_decl.1.unwrap_or(default.precision),
            };
            if unit.precision > unit.unit {
                return Err(unsupported("timeprecision coarser than timeunit"));
            }
            for i in 0..scopes.len() {
                let enclosing = parents[i].map(|parent| {
                    let scope = &scopes[parent];
                    (scope.scale, scope.specified)
                });
                let inherited = enclosing
                    .or_else(|| {
                        let before = directives.partition_point(|(at, _)| *at < scopes[i].start);
                        before
                            .checked_sub(1)
                            .map(|i| &directives[i])
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
                // Relaxed designs may mix explicit precision with the default.
                // Both contribute to the global simulation precision.
                let precision = scopes[i].scale.precision;
                design.precision =
                    Some(design.precision.map_or(precision, |old| old.min(precision)));
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
                key(tree),
                Arc::new(Unit {
                    scale: unit,
                    scopes,
                    parents,
                }),
            );
        }
        if specified.contains(&3) && !options.allow_mixed_timescales {
            return Err(unsupported(
                "mixed explicit and default time units or precisions across design elements",
            ));
        }
        design.specified = specified;
        Ok(design)
    }

    pub(super) fn query(
        &self,
        call: celox_sv_syntax::ExprId,
        tree: &SyntaxTree,
        interface_types: &HashMap<String, String>,
    ) -> Converted<i32> {
        let source = key(tree);
        let extended = (!self.units.contains_key(&source))
            .then(|| self.including_tree(tree))
            .transpose()?;
        let design = extended.as_ref().unwrap_or(self);
        design.query_in_unit(call, tree, &design.units[&source], interface_types)
    }

    fn query_in_unit(
        &self,
        call: celox_sv_syntax::ExprId,
        tree: &SyntaxTree,
        unit: &Unit,
        interface_types: &HashMap<String, String>,
    ) -> Converted<i32> {
        self.resolve(
            call,
            tree,
            unit,
            system_functions::CallSite::Expression,
            interface_types,
        )
        .map(|scope| scope.value())
    }

    /// The time units of the simulation, which counts the global time
    /// precision (3.14.3), in one time unit of the scope of `node`.
    fn time_divisor_in_unit(&self, at: usize, unit: &Unit) -> u64 {
        self.ticks_in(at, unit, |scale| scale.unit)
    }

    /// The time units of the simulation in one time precision step of the
    /// scope of `node` (3.14.1).
    fn precision_divisor_in_unit(&self, at: usize, unit: &Unit) -> u64 {
        self.ticks_in(at, unit, |scale| scale.precision)
    }

    /// The time units of the simulation in `10^exponent(scale)` seconds,
    /// where `scale` is the time scale of the scope at `at`.
    fn ticks_in(&self, at: usize, unit: &Unit, exponent: impl Fn(&Scale) -> i32) -> u64 {
        let scale =
            scope_at(&unit.scopes, &unit.parents, at).map_or(unit.scale, |i| unit.scopes[i].scale);
        let precision = self.precision.unwrap_or(DEFAULT.precision);
        // A scope's precision is not coarser than its unit, and no unit is
        // longer than 100 s or precision finer than 1 fs.
        10u64.pow((exponent(&scale) - precision.min(scale.precision)) as u32)
    }

    fn resolve(
        &self,
        call: celox_sv_syntax::ExprId,
        tree: &SyntaxTree,
        unit: &Unit,
        site: system_functions::CallSite,
        interface_types: &HashMap<String, String>,
    ) -> Converted<ResolvedScope> {
        let native = tree.declarations();
        let (name, arguments) = call_parts(call, native)?;
        let args = arguments.map(|args| {
            args.iter()
                .map(|arg| {
                    if arg.value.is_some() {
                        system_functions::Arg::Given
                    } else {
                        system_functions::Arg::Omitted
                    }
                })
                .collect::<Vec<_>>()
        });
        let named = arguments.is_some_and(|args| args.iter().any(|arg| arg.name.is_some()));
        let checked = system_functions::check_call(
            name,
            (!named).then_some(args.as_deref().unwrap_or(&[])),
            site,
        )?;
        let at = native.expression(call).tokens.start;
        let current = scope_at(&unit.scopes, &unit.parents, at).map(|i| &unit.scopes[i]);
        let mut display = match current {
            Some(scope) if scope.kind == ScopeKind::Package => {
                DisplayScope::Absolute(scope.name.clone())
            }
            Some(_) => DisplayScope::Relative(String::new()),
            None => DisplayScope::Absolute("$unit".into()),
        };
        let scale = if arguments.is_none_or(|args| args.is_empty()) {
            current.map_or(unit.scale, |s| s.scale)
        } else {
            let argument = arguments
                .and_then(|args| args.first())
                .and_then(|arg| arg.value)
                .ok_or_else(|| unsupported("timescale query design-element argument"))?;
            match &native.expression(argument).kind {
                celox_sv_syntax::ExprKind::Name(name) if name.spelling == "$unit" => {
                    display = DisplayScope::Absolute("$unit".into());
                    unit.scale
                }
                celox_sv_syntax::ExprKind::Name(name) if name.spelling == "$root" => {
                    display = DisplayScope::Absolute("$root".into());
                    let precision = self.precision.unwrap_or(DEFAULT.precision);
                    Scale {
                        unit: precision,
                        precision,
                    }
                }
                _ => {
                    let (absolute, path) = design_path(argument, native)?;
                    let first = &path[0];
                    let (mut target, start) = if !absolute
                        && let Some(interface) = interface_types.get(first)
                        && let Some(target) = self.definitions.get(interface)
                    {
                        display = DisplayScope::Relative(path.join("."));
                        (target, 1)
                    } else if !absolute && current.is_some_and(|s| s.instances.contains_key(first))
                    {
                        display = DisplayScope::Relative(path.join("."));
                        (current.unwrap(), 0)
                    } else if self.is_root(first)
                        && let Some(root) = self.definitions.get(first)
                    {
                        display = DisplayScope::Absolute(path.join("."));
                        // A root instance is part of the hierarchy and takes
                        // precedence over a same-named global package.
                        (root, 1)
                    } else if !absolute
                        && path.len() == 1
                        && let Some(package) = self.packages.get(first)
                    {
                        display = DisplayScope::Absolute(path.join("."));
                        // Package and definition names have separate global
                        // namespaces (3.13). An instantiated module type with
                        // this spelling does not hide the package.
                        (package, 1)
                    } else {
                        if !self.is_root(first)
                            && (absolute || current.is_none_or(|scope| scope.name != *first))
                            && self.definitions.contains_key(first)
                        {
                            return Err(unsupported(format!(
                                "timescale query names module type `{first}`, not an instance"
                            )));
                        }
                        display = if !absolute && current.is_some_and(|scope| scope.name == *first)
                        {
                            DisplayScope::Relative(path[1..].join("."))
                        } else {
                            DisplayScope::Absolute(path.join("."))
                        };
                        (
                            self.definitions.get(first).ok_or_else(|| {
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
                        target = self.definitions.get(module).ok_or_else(|| {
                            unsupported(format!("unknown timescale query module `{module}`"))
                        })?;
                    }
                    target.scale
                }
            }
        };
        Ok(ResolvedScope {
            call: checked,
            scale,
            display,
        })
    }

    pub(super) fn print_message(
        &self,
        call: celox_sv_syntax::ExprId,
        tree: &SyntaxTree,
        site: system_functions::CallSite,
        prefix: Option<&str>,
        interface_types: &HashMap<String, String>,
    ) -> Converted<String> {
        let source = key(tree);
        let extended = (!self.units.contains_key(&source))
            .then(|| self.including_tree(tree))
            .transpose()?;
        let design = extended.as_ref().unwrap_or(self);
        design
            .resolve(call, tree, &design.units[&source], site, interface_types)
            .map(|scope| scope.message(prefix))
    }
}

pub(super) fn print_message(
    call: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    site: system_functions::CallSite,
) -> Converted<String> {
    let prefix = tree.state.timescales.interface_instance.borrow().clone();
    {
        let context = tree.state.timescales.design.borrow();
        if let Some(prepared) = &context.prepared
            && prepared.address == tree.syntax_tree as *const SyntaxTree as usize
        {
            context
                .design
                .resolve(
                    call,
                    tree,
                    &prepared.unit,
                    site,
                    &scope::imported(&tree.state).interface_types,
                )
                .map(|scope| scope.message(prefix.as_deref()))
        } else {
            context.design.print_message(
                call,
                tree,
                site,
                prefix.as_deref(),
                &scope::imported(&tree.state).interface_types,
            )
        }
    }
}

pub(super) fn query(call: celox_sv_syntax::ExprId, tree: &AnalysisContext<'_>) -> Converted<i32> {
    {
        let context = tree.state.timescales.design.borrow();
        if let Some(prepared) = &context.prepared
            && prepared.address == tree.syntax_tree as *const SyntaxTree as usize
        {
            context.design.query_in_unit(
                call,
                tree,
                &prepared.unit,
                &scope::imported(&tree.state).interface_types,
            )
        } else {
            context
                .design
                .query(call, tree, &scope::imported(&tree.state).interface_types)
        }
    }
}

/// The simulation's time units in one time unit of the scope of `node`: the
/// divisor that scales the simulation time to `$time` (20.3.1), and the
/// factor that scales a delay to the simulation time (3.14.1).
pub(super) fn time_divisor(
    node: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
) -> Converted<u64> {
    divisor(node, tree, Timescales::time_divisor_in_unit)
}

/// The simulation's time units in one time precision step of the scope of
/// `node`, at which a real delay is rounded (3.14.1).
pub(super) fn precision_divisor(
    node: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
) -> Converted<u64> {
    divisor(node, tree, Timescales::precision_divisor_in_unit)
}

fn divisor(
    node: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    in_unit: fn(&Timescales, usize, &Unit) -> u64,
) -> Converted<u64> {
    let context = tree.state.timescales.design.borrow();
    let at = tree.declarations().expression(node).tokens.start;
    if let Some(prepared) = &context.prepared
        && prepared.address == tree.syntax_tree as *const SyntaxTree as usize
    {
        return Ok(in_unit(&context.design, at, &prepared.unit));
    }
    let design = &context.design;
    let source = key(tree);
    let extended = (!design.units.contains_key(&source))
        .then(|| design.including_tree(tree))
        .transpose()?;
    let design = extended.as_ref().unwrap_or(design);
    Ok(in_unit(design, at, &design.units[&source]))
}

/// A standalone time literal used as a delay: round at the declaring
/// scope's precision before converting to global scheduler ticks (3.14.1).
pub(super) fn delay_literal(
    node: celox_sv_syntax::ExprId,
    text: &str,
    tree: &AnalysisContext<'_>,
) -> Converted<Option<u64>> {
    let Some((number, exponent)) = [
        ("fs", -15),
        ("ps", -12),
        ("ns", -9),
        ("us", -6),
        ("ms", -3),
        ("s", 0),
    ]
    .into_iter()
    .find_map(|(suffix, exponent)| text.strip_suffix(suffix).map(|n| (n, exponent))) else {
        return Ok(None);
    };
    let context = tree.state.timescales.design.borrow();
    let source = key(tree);
    let extended = (!context.design.units.contains_key(&source))
        .then(|| context.design.including_tree(tree))
        .transpose()?;
    let design = extended.as_ref().unwrap_or(&context.design);
    let unit = &design.units[&source];
    let at = tree.declarations().expression(node).tokens.start;
    let scale =
        scope_at(&unit.scopes, &unit.parents, at).map_or(unit.scale, |i| unit.scopes[i].scale);
    let global_precision = design
        .precision
        .unwrap_or(DEFAULT.precision)
        .min(scale.precision);
    let number = number.replace('_', "");
    let fraction = number
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits = number.replace('.', "");
    let coefficient = digits
        .parse::<num_bigint::BigUint>()
        .map_err(|_| unsupported("time literal delay"))?;
    let power = exponent
        - scale.precision
        - i32::try_from(fraction).map_err(|_| unsupported("time literal delay"))?;
    let precision_units = if power >= 0 {
        coefficient * num_bigint::BigUint::from(10u8).pow(power as u32)
    } else {
        let divisor = num_bigint::BigUint::from(10u8).pow(power.unsigned_abs());
        (coefficient + (&divisor / 2u8)) / divisor
    };
    let ticks = precision_units
        * num_bigint::BigUint::from(10u8).pow((scale.precision - global_precision) as u32);
    Ok(Some(u64::try_from(ticks).map_err(|_| {
        unsupported("time literal delay exceeds scheduler range")
    })?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_interface_instances_keep_their_full_path() {
        let state = State::default();
        with_interface_instance(&state, "outer", || {
            with_interface_instance(&state, "child[2]", || {
                assert_eq!(
                    state.interface_instance.borrow().as_deref(),
                    Some("outer.child[2]")
                );
            });
            assert_eq!(state.interface_instance.borrow().as_deref(), Some("outer"));
        });
        assert!(state.interface_instance.borrow().is_none());
    }

    use super::*;
    fn native_calls(tree: &SyntaxTree) -> Vec<celox_sv_syntax::ExprId> {
        let native = tree.declarations();
        let callees: HashSet<_> = native
            .expression_nodes()
            .filter_map(|(_, node)| match node.kind {
                celox_sv_syntax::ExprKind::Call { expression, .. } => Some(expression),
                _ => None,
            })
            .collect();
        let mut calls: Vec<_> = native
            .expression_nodes()
            .filter_map(|(id, _)| (is_query(id, native) && !callees.contains(&id)).then_some(id))
            .collect();
        calls.sort_by_key(|id| native.expression(*id).tokens.start);
        calls
    }
    #[test]
    fn current_query_scope_lookup_scales_logarithmically() {
        let mut code = String::new();
        for i in 0..1024 {
            let time = if i % 2 == 0 {
                "100ns / 1ns"
            } else {
                "1us / 100ps"
            };
            code.push_str(&format!(
                "module M{i}(); timeunit {time}; localparam Q=$timeunit(); endmodule\n"
            ));
        }
        let tree = crate::source_tree::parse_source(&code, Path::new("many_queries.sv")).unwrap();
        let design =
            Arc::new(Timescales::collect(&[(&tree, Path::new("many_queries.sv"))]).unwrap());
        let tree = AnalysisContext::unspecialized(&tree);
        let _guard = install_for_tree(&tree.state.timescales, design, &tree).unwrap();
        let before = SCOPE_CHECKS.with(|count| count.get());
        let mut calls = 0;
        for call in native_calls(&tree) {
            assert_eq!(
                query(call, &tree).unwrap(),
                if calls % 2 == 0 { -7 } else { -6 }
            );
            calls += 1;
        }
        assert_eq!(calls, 1024);
        let checks = SCOPE_CHECKS.with(|count| count.get()) - before;
        assert!(
            checks < 16 * calls,
            "performed {checks} scope checks for {calls} queries"
        );
    }

    #[test]
    fn queries_after_nested_declarations_keep_the_enclosing_scope() {
        let code = "module Outer(); timeunit 100ns / 10ps;
            module Inner(); timeunit 10ns; localparam I=$timeunit(); endmodule
            localparam O=$timeunit(); endmodule
            module Sibling(); timeunit 1ns / 1ps; localparam S=$timeunit(); endmodule";
        let tree = crate::source_tree::parse_source(code, Path::new("nested_queries.sv")).unwrap();
        let design =
            Arc::new(Timescales::collect(&[(&tree, Path::new("nested_queries.sv"))]).unwrap());
        let tree = AnalysisContext::unspecialized(&tree);
        let _guard = install_for_tree(&tree.state.timescales, design, &tree).unwrap();
        let values: Vec<_> = native_calls(&tree)
            .into_iter()
            .map(|call| query(call, &tree).unwrap())
            .collect();
        assert_eq!(values, [-8, -7, -9]);
    }

    #[test]
    fn separately_supplied_modules_share_the_package_design_precision() {
        let packages = crate::analyze_packages(&[(
            "package P; timeunit 1ns / 1ps; endpackage",
            Path::new("external.sv"),
        )])
        .unwrap();
        // Reusing Packages must not retain an earlier omitted module's precision.
        for (precision, expected) in [("1fs", -15), ("100ps", -12)] {
            let code = format!(
                "module Top(); timeunit 1ns / {precision}; localparam U=$timeunit($root); localparam P=$timeprecision($root); endmodule"
            );
            let ir = crate::analyze_source_module_with_packages(
                &code,
                Path::new("omitted.sv"),
                "Top",
                &HashMap::default(),
                &packages,
            )
            .unwrap();
            let values: Vec<_> = ir.modules()[0]
                .parameters()
                .iter()
                .map(|p| p.resolved_value())
                .collect();
            assert_eq!(values, [Some(expected), Some(expected)]);
        }
    }

    #[test]
    fn separately_supplied_modules_validate_mixed_defaults_without_queries() {
        for (package, module) in [
            (
                "package P; timeunit 1ns / 1ps; endpackage",
                "module Top(output logic y); assign y = 0; endmodule",
            ),
            (
                "package P; endpackage",
                "module Top(output logic y); timeunit 1ns / 1ps; assign y = 0; endmodule",
            ),
        ] {
            let packages = crate::analyze_packages(&[(package, Path::new("external.sv"))]).unwrap();
            let result = crate::analyze_source_module_with_packages(
                module,
                Path::new("omitted.sv"),
                "Top",
                &HashMap::default(),
                &packages,
            );
            assert!(result.is_err(), "accepted mixed defaults for {module}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("mixed explicit and default")
            );
        }
    }

    #[test]
    fn separately_supplied_modules_update_inferred_roots() {
        let packages = crate::analyze_packages(&[(
            "module Child(); timeunit 1ns / 1ps; endmodule",
            Path::new("external.sv"),
        )])
        .unwrap();
        let code = "module Top(); timeunit 1us / 1ns; Child child(); localparam Q=$timeunit($root.Child); endmodule";
        assert!(
            crate::analyze_source_module_with_packages(
                code,
                Path::new("omitted.sv"),
                "Top",
                &HashMap::default(),
                &packages
            )
            .is_err()
        );
    }

    #[test]
    fn separately_supplied_modules_preserve_explicit_roots_and_external_paths() {
        let external = crate::ParsedSource::parse(
            "module Child(); timeunit 1ns / 1ps; endmodule module Other(); timeunit 100ns / 10ps; endmodule",
            Path::new("external.sv"),
        ).unwrap();
        let packages =
            crate::ParsedSource::analyze_packages_with_roots(&[&external], &["Top", "Other"])
                .unwrap();
        let source = crate::ParsedSource::parse(
            "module Top(); timeunit 1us / 1ns; Child child(); localparam U=$timeunit($root.Top.child); localparam P=$timeprecision($root.Other); endmodule",
            Path::new("omitted.sv"),
        ).unwrap();
        let ir = source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let values: Vec<_> = ir.modules()[0]
            .parameters()
            .iter()
            .map(|p| p.resolved_value())
            .collect();
        assert_eq!(values, [Some(-9), Some(-11)]);
    }

    #[test]
    fn sibling_scope_collection_uses_linear_parent_checks() {
        let mut code = "timeunit 1ns / 1ps;".to_string();
        for i in 0..1024 {
            code.push_str(&format!(
                "module M{i}(); timeunit 1us / 1ns; M0 child(); endmodule\n"
            ));
        }
        let tree = crate::source_tree::parse_source(&code, Path::new("many_scopes.sv")).unwrap();
        let before = PARENT_CHECKS.with(|count| count.get());
        let design = Timescales::collect(&[(&tree, Path::new("many_scopes.sv"))]).unwrap();
        let checks = PARENT_CHECKS.with(|count| count.get()) - before;
        assert!(checks < 2 * 1024, "performed {checks} parent checks");
        assert_eq!(design.definitions.len(), 1024);
        for scope in design.definitions.values() {
            assert_eq!((scope.scale.unit, scope.scale.precision), (-6, -9));
            assert_eq!(scope.instances.get("child").map(String::as_str), Some("M0"));
        }
    }

    #[test]
    fn nested_scopes_inherit_from_their_parent_without_leaking_to_siblings() {
        let code = "timeunit 1ns / 1ps;
            `timescale 1ms / 1us
            module Outer(); timeunit 100ns / 10ps;
                module First(); timeunit 10ns;
                    module Deep(); endmodule
                    Deep deep();
                endmodule
                module Second(); endmodule
                First first(); Second second();
                timeprecision 10ps;
            endmodule
            module Sibling(); endmodule
            `resetall
            package After; endpackage";
        let tree = crate::source_tree::parse_source(code, Path::new("nested_scopes.sv")).unwrap();
        let design = Timescales::collect(&[(&tree, Path::new("nested_scopes.sv"))]).unwrap();
        for (name, unit, precision) in [
            ("Outer", -7, -11),
            ("First", -8, -11),
            ("Deep", -8, -11),
            ("Second", -7, -11),
            ("Sibling", -3, -6),
        ] {
            let scope = &design.definitions[name];
            assert_eq!(
                (scope.scale.unit, scope.scale.precision),
                (unit, precision),
                "{name}"
            );
        }
        assert_eq!(design.definitions["Outer"].instances.len(), 2);
        assert_eq!(
            design.definitions["First"]
                .instances
                .get("deep")
                .map(String::as_str),
            Some("Deep")
        );
        assert!(design.definitions["Deep"].instances.is_empty());
        let after = &design.packages["After"];
        assert_eq!((after.scale.unit, after.scale.precision), (-9, -12));
    }

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
        // 3.14.3 minimizes declared/directive precisions, not an unused default.
        for (prefix, unit, precision, root) in [
            ("", -9, -12, -12),
            ("timeunit 1ns / 1ns;", -9, -9, -9),
            ("timeunit 100s / 10s;", 2, 1, 1),
        ] {
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
            let tree = crate::source_tree::parse_source(code, Path::new("mixed_times.sv")).unwrap();
            assert!(
                Timescales::collect(&[(&tree, Path::new("mixed_times.sv"))]).is_err(),
                "accepted {code}"
            );
        }
        let explicit = crate::source_tree::parse_source(
            "module A(); timeunit 1ns / 1ns; endmodule",
            Path::new("a.sv"),
        )
        .unwrap();
        let default =
            crate::source_tree::parse_source("module B(); endmodule", Path::new("b.sv")).unwrap();
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
    fn package_queries_require_an_unqualified_single_component() {
        for function in ["$timeunit", "$timeprecision"] {
            for operand in ["P", "$root.P", "P.child", "$root.P.child"] {
                let code = format!(
                    "package P; timeunit 100ns / 10ps; endpackage module Top(); timeunit 1ns / 1ps; localparam Q={function}({operand}); endmodule"
                );
                let result = crate::analyze_source(&code, Path::new("package_path.sv"));
                let tree =
                    crate::source_tree::parse_source(&code, Path::new("package_path.sv")).unwrap();
                let call = native_calls(&tree)[0];
                // Exercise both collected design metadata and the local-tree fallback.
                let local = Timescales::default().query(call, &tree, &HashMap::default());
                if operand == "P" {
                    let expected: i32 = if function == "$timeunit" { -7 } else { -11 };
                    let ir = result.unwrap();
                    assert_eq!(
                        ir.modules()[0].parameters()[0].resolved_value(),
                        Some(i128::from(expected))
                    );
                    assert_eq!(local.unwrap(), expected);
                } else {
                    assert!(result.is_err(), "accepted {function}({operand})");
                    assert!(
                        local.is_err(),
                        "local fallback accepted {function}({operand})"
                    );
                }
            }
        }
    }

    #[test]
    fn explicitly_selected_roots_keep_multiple_roots_and_exclude_unused_definitions() {
        let code = "module A(); timeunit 1ns / 1ps; Child c(); endmodule module B(); timeunit 1us / 1ns; endmodule module Harness(); timeunit 1ms / 1us; A a(); B b(); endmodule module Child(); timeunit 1ns / 1ps; localparam U=$timeunit($root.A); localparam P=$timeprecision($root.B); endmodule";
        let source = crate::ParsedSource::parse(code, Path::new("roots.sv")).unwrap();
        let packages =
            crate::ParsedSource::analyze_packages_with_roots(&[&source], &["A", "B"]).unwrap();
        let ir = source
            .analyze_module(
                "Child",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let values: Vec<_> = ir.modules()[0]
            .parameters()
            .iter()
            .map(|p| p.resolved_value())
            .collect();
        assert_eq!(values, [Some(-9), Some(-9)]);
        for expression in [
            "$timeunit($root.Harness)",
            "$timeprecision(Harness)",
            "$timeunit($root.Child)",
        ] {
            let code = format!(
                "module Top(); timeunit 1ns / 1ps; Child c(); endmodule module Harness(); timeunit 1ms / 1us; endmodule module Child(); timeunit 1ns / 1ps; localparam U={expression}; endmodule"
            );
            let source = crate::ParsedSource::parse(&code, Path::new("non_roots.sv")).unwrap();
            let packages =
                crate::ParsedSource::analyze_packages_with_roots(&[&source], &["Top"]).unwrap();
            assert!(
                source
                    .analyze_module(
                        "Child",
                        &HashMap::default(),
                        &[],
                        &[],
                        &ModuleInterfaces::default(),
                        &packages
                    )
                    .is_err(),
                "accepted {expression}"
            );
        }
    }

    #[test]
    fn interface_handle_queries_validate_operands_without_editing_macros() {
        for (instance, expression) in [
            ("I i();", "$timeunit(i, 1)"),
            ("I i();", "$timeprecision(i + 1)"),
            ("I i[2]();", "$timeunit(i)"),
            ("I i[2]();", "$timeprecision(i[0])"),
            ("if (1) begin : g I i(); end", "$timeunit(i)"),
        ] {
            let code = format!(
                "interface I; timeunit 1ns / 1ps; endinterface module Top(output int y); timeunit 1ns / 1ps; {instance} assign y = {expression}; endmodule"
            );
            assert!(
                crate::analyze_source(&code, Path::new("invalid_handle_query.sv")).is_err(),
                "accepted {expression} with {instance}"
            );
        }
        for (definition, call) in [
            ("`define QUERY(h) $timeunit(h)", "`QUERY(i)"),
            ("`define QUERY $timeunit(i)", "`QUERY"),
            ("`define QUERY \\\n$timeunit(i)", "`QUERY"),
        ] {
            let code = format!(
                "{definition}\ninterface I; timeunit 1ns / 1ps; endinterface module Top(output int y); timeunit 1ns / 1ps; I i(); assign y = {call}; endmodule"
            );
            let result = crate::analyze_source(&code, Path::new("macro_handle_query.sv"));
            assert!(result.is_ok(), "{result:?}");
        }
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
        let ir = crate::analyze_source(&code, Path::new("unused_macro_interface.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(-3));
        let code = format!(
            "{prefix} module Top(output int y); timeunit 1ms / 1us; localparam U=`SCALE; I i(); assign y=i.u(); endmodule"
        );
        let ir = crate::analyze_source(&code, Path::new("used_macro_interface.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(-3));
        assert!(
            ir.modules()[0]
                .subroutines()
                .iter()
                .any(|function| function.name == "i$u")
        );
    }

    #[test]
    fn prepared_tree_context_is_restored_after_nested_analysis_and_errors() {
        let a = crate::source_tree::parse_source(
            "module A(); timeunit 1us / 1ns; localparam U=$timeunit(); endmodule",
            Path::new("a.sv"),
        )
        .unwrap();
        let b = crate::source_tree::parse_source(
            "module B(); timeunit 1ns / 1ps; localparam U=$timeunit(); endmodule",
            Path::new("b.sv"),
        )
        .unwrap();
        let invalid = crate::source_tree::parse_source(
            "module Invalid(); timeunit 2ns; endmodule",
            Path::new("invalid.sv"),
        )
        .unwrap();
        let design = Arc::new(
            Timescales::collect(&[(&a, Path::new("a.sv")), (&b, Path::new("b.sv"))]).unwrap(),
        );
        fn call(tree: &SyntaxTree) -> celox_sv_syntax::ExprId {
            native_calls(tree)[0]
        }
        let a = AnalysisContext::unspecialized(&a);
        let overrides = HashMap::default();
        let b = a.child(&b, &overrides);
        let _outer = install_for_tree(&a.state.timescales, design.clone(), &a).unwrap();
        assert_eq!(query(call(&a), &a).unwrap(), -6);
        {
            let _inner = install_for_tree(&b.state.timescales, design.clone(), &b).unwrap();
            assert_eq!(query(call(&b), &b).unwrap(), -9);
            assert!(install_for_tree(&b.state.timescales, design, &invalid).is_err());
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
        for width in [8, 4, 8] {
            let mut ty = Type::new(TypeKind::Logic);
            ty.packed_ranges = vec![PackedRange::new(
                ConstExpr::Literal((width - 1).to_string()),
                ConstExpr::Literal("0".into()),
            )];
            let before = COLLECTIONS.with(std::cell::Cell::get);
            let result = source
                .analyze_module(
                    "Child",
                    &HashMap::default(),
                    &[("T".into(), ty)],
                    &[],
                    &ModuleInterfaces::default(),
                    &packages,
                )
                .unwrap();
            let after = COLLECTIONS.with(std::cell::Cell::get);
            assert_eq!(
                after - before,
                0,
                "recollected cached time scopes for width {width}"
            );
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
    fn interface_bindings_preserve_macros_in_module_scopes() {
        let code = "`define U $timeunit()\ninterface I; timeunit 1ns / 1ps; endinterface module Top(); timeunit 1ms / 1us; localparam U=`U; endmodule module Child(); timeunit 1us / 1ns; localparam U=`U; endmodule";
        let result = crate::analyze_source(code, Path::new("macro_times.sv")).unwrap();
        for (name, value) in [("Top", -3), ("Child", -6)] {
            let module = result.modules().iter().find(|m| m.name() == name).unwrap();
            assert_eq!(module.parameters()[0].resolved_value(), Some(value));
        }
    }

    #[test]
    fn interface_queries_validate_discarded_and_hidden_operands() {
        for expression in [
            "$timeunit(missing);",
            "void'($timeprecision(1));",
            "int x = $bits($timeunit(1));",
        ] {
            let code = format!(
                "interface I; timeunit 1ns / 1ps; function automatic int f(); {expression} return 0; endfunction endinterface module Top(); timeunit 1ns / 1ps; I i(); endmodule"
            );
            assert!(
                crate::analyze_source(&code, Path::new("invalid_interface_query.sv")).is_err(),
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
                crate::analyze_source(&code, Path::new("invalid_interface_scale.sv")).is_err(),
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
