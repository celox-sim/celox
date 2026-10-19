//! Interface contracts and binding plans collected from authored declarations.
//!
//! Port directions and generic bindings are computed before semantic lowering.
//! `native_interfaces` binds bodies and references in the original syntax tree;
//! these contracts never generate or reparse SystemVerilog source.

use std::path::Path;

use super::instances::normalize_identifier;
use super::*;
use celox_sv_syntax as native;
type BodyId = native::BodyItemId;
type ExprId = native::ExprId;
type ScopeHandle = crate::migration::ScopeHandle;

/// Joins an interface handle (instance or port) and an interface item name.
const SEPARATOR: &str = "$";

/// Interface declarations exported for mixed-language type resolution. The
/// signature uses the original syntax tree, including typedefs and parameter widths.
#[derive(Clone, Debug)]
pub struct InterfacePrototype {
    pub name: String,
    pub module: crate::ir::Module,
    pub time_declarations: String,
    pub modports: std::collections::BTreeMap<String, Vec<(String, String)>>,
    pub parameters: Vec<String>,
    pub driven: Vec<String>,
    pub has_behavior: bool,
    /// A declaration cast whose target depends on an overridable header.
    /// Native IR currently retains only its resolved target width.
    pub header_dependent_cast: bool,
    /// What analyzing the signature warned about.
    pub warnings: Vec<crate::ir::AnalyzerWarning>,
}

pub fn interface_prototypes(
    sources: &[(&str, &Path)],
) -> Result<Vec<InterfacePrototype>, AnalyzerError> {
    interface_prototypes_for(sources, None)
}

pub fn interface_prototypes_for(
    sources: &[(&str, &Path)],
    selected: Option<&HashSet<String>>,
) -> Result<Vec<InterfacePrototype>, AnalyzerError> {
    if selected.is_some_and(HashSet::is_empty) {
        return Ok(Vec::new());
    }
    let files = sources
        .iter()
        .map(|(code, path)| File::new(code, path))
        .collect::<Result<Vec<_>, _>>()?;
    let declaration_warnings: Vec<_> = files
        .iter()
        .map(|file| super::warnings::components(&file.syntax_tree))
        .collect();
    let mut source_warnings = Vec::new();
    for declarations in &declaration_warnings {
        for (name, warnings) in declarations {
            if selected.is_none_or(|names| names.contains(name)) {
                super::warnings::append(&mut source_warnings, warnings.iter().cloned());
            }
        }
    }
    let design = Design::collect_selected(&files, selected)
        .map_err(|error| error.with_warnings(source_warnings.clone()))?;
    let mut declarations = design
        .interfaces
        .values()
        .filter(|interface| selected.is_none_or(|names| names.contains(&interface.name)))
        .collect::<Vec<_>>();
    declarations.sort_by(|left, right| left.name.cmp(&right.name));
    let timescales = if declarations.is_empty() {
        None
    } else {
        Some(super::timescales::Timescales::collect(
            &files
                .iter()
                .zip(sources)
                .map(|(file, (_, path))| (&file.syntax_tree, *path))
                .collect::<Vec<_>>(),
        )?)
    };
    let trees = files
        .iter()
        .zip(sources)
        .map(|(file, (_, path))| (&file.syntax_tree, *path))
        .collect::<Vec<_>>();
    let packages = packages::Packages::analyze(&trees)
        .map_err(|error| error.with_warnings(source_warnings))?;
    let package_warnings = packages.warnings().to_vec();
    let mut used_names = design
        .modules
        .keys()
        .chain(design.interfaces.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let mut found = package_warnings.clone();
    declarations
        .into_iter()
        .map(|interface| {
            let warnings = declaration_warnings[interface.file]
                .get(&interface.name)
                .cloned()
                .unwrap_or_default();
            super::warnings::append(&mut found, warnings.iter().cloned());
            let prototype = (|| -> Result<InterfacePrototype, AnalyzerError> {
                let prefix = format!("__celox_interface_{}", interface.name);
                let mut probe_name = prefix.clone();
                let mut index = 0;
                while !used_names.insert(probe_name.clone()) {
                    probe_name = format!("{prefix}_{index}");
                    index += 1;
                }
                let time_declarations = timescales
                    .as_ref()
                    .unwrap()
                    .declaration_statements(&interface.name)?;
                let behavior_functions = interface
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::Process(span) => Some(*span),
                        Item::Member(index) => interface.members[*index].initializer,
                        _ => None,
                    })
                    .flat_map(|span| interface.referenced(span))
                    .filter(|name| interface.function(name).is_some())
                    .map(str::to_string)
                    .collect::<HashSet<_>>();
                let behavior_captures =
                    interface.captured_members(&interface.function_closure(&behavior_functions));
                Ok(InterfacePrototype {
                    name: interface.name.clone(),
                    warnings: warnings::components(&files[interface.file].syntax_tree)
                        .remove(&interface.name)
                        .unwrap_or_default(),
                    header_dependent_cast: interface
                        .header_dependent_cast(&files[interface.file])?,
                    module: {
                        let file = &files[interface.file];
                        let scoped = file.syntax_tree.scoped(interface.scope)?;
                        interface_ast::signature(
                            &scoped,
                            sources[interface.file].1,
                            &packages,
                            probe_name.clone(),
                            &interface
                                .members
                                .iter()
                                .map(|m| m.name.clone())
                                .collect::<Vec<_>>(),
                            &interface.names,
                        )?
                    },
                    time_declarations,
                    parameters: interface
                        .parameters
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                    driven: interface
                        .members
                        .iter()
                        .filter(|member| {
                            member.initializer.is_some()
                                || behavior_captures.get(&member.name) == Some(&Direction::Output)
                                || interface.references.iter().any(|(span, name)| {
                                    name == &member.name
                                        && interface.lvalues.contains(&span.0)
                                        && !interface.functions.iter().any(|function| {
                                            function.span.0 <= span.0 && span.1 <= function.span.1
                                        })
                                })
                        })
                        .map(|member| member.name.clone())
                        .collect(),
                    has_behavior: interface
                        .items
                        .iter()
                        .any(|item| matches!(item, Item::Process(_)))
                        || interface
                            .members
                            .iter()
                            .any(|member| member.initializer.is_some()),
                    modports: interface
                        .modports
                        .iter()
                        .map(|(name, items)| {
                            (
                                name.clone(),
                                items
                                    .iter()
                                    .map(|item| match item {
                                        ModportItem::Member(direction, name) => {
                                            (name.clone(), direction.keyword().to_string())
                                        }
                                        ModportItem::Import(name) => {
                                            (name.clone(), "import".to_string())
                                        }
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                })
            })();
            prototype
                .map(|mut prototype| {
                    prototype.warnings = package_warnings.clone();
                    for warning in warnings {
                        if !prototype.warnings.contains(&warning) {
                            prototype.warnings.push(warning);
                        }
                    }
                    prototype
                })
                .map_err(|error| error.with_warnings(found.clone()))
        })
        .collect::<Result<Vec<_>, AnalyzerError>>()
}

type Span = (usize, usize);

fn roots(tree: &SyntaxTree) -> Converted<Vec<(ScopeHandle, &native::Unit)>> {
    let declarations = tree.declarations();
    let mut pending = declarations
        .source_items
        .iter()
        .filter_map(|item| match item.kind {
            native::SourceItemKind::Unit(id) => Some(ScopeHandle::SourceUnit(id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    pending.reverse();
    let mut result = Vec::new();
    let mut seen = HashSet::default();
    while let Some(handle) = pending.pop() {
        if !seen.insert(handle) {
            return Err(AnalyzerError::Parse(
                "repeated native interface root".into(),
            ));
        }
        let unit = handle
            .unit(declarations)
            .ok_or_else(|| unsupported("missing native interface root"))?;
        pending.extend(unit.children.iter().rev().map(|id| ScopeHandle::Unit(*id)));
        result.push((handle, unit));
    }
    Ok(result)
}
fn body_items(unit: &native::Unit, declarations: &native::Declarations) -> Converted<Vec<BodyId>> {
    let mut pending = unit.items.iter().rev().copied().collect::<Vec<_>>();
    let mut result = Vec::new();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native interface body item".into(),
            ));
        }
        result.push(id);
        match &declarations.body_item(id).kind {
            native::BodyItemKind::Region(items)
            | native::BodyItemKind::Block { items, .. }
            | native::BodyItemKind::Unconverted { items, .. } => {
                pending.extend(items.iter().rev().copied())
            }
            native::BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.extend(otherwise);
                pending.push(*then_body);
            }
            native::BodyItemKind::Case { arms, .. } => {
                pending.extend(arms.iter().rev().map(|arm| arm.body))
            }
            native::BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(result)
}
fn block_spans(unit: &native::Unit, file: &File<'_>) -> Converted<Vec<Span>> {
    let native = file.syntax_tree.declarations();
    let mut ids = HashSet::default();
    for id in body_items(unit, native)? {
        match &native.body_item(id).kind {
            native::BodyItemKind::Block { .. } => {
                ids.insert(id);
            }
            native::BodyItemKind::Loop { body, .. } => {
                ids.insert(*body);
            }
            native::BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                for id in std::iter::once(then_body).chain(otherwise) {
                    if !matches!(
                        native.body_item(*id).kind,
                        native::BodyItemKind::Conditional { .. }
                    ) {
                        ids.insert(*id);
                    }
                }
            }
            native::BodyItemKind::Case { arms, .. } => {
                for arm in arms {
                    if !matches!(
                        native.body_item(arm.body).kind,
                        native::BodyItemKind::Conditional { .. }
                    ) {
                        ids.insert(arm.body);
                    }
                }
            }
            _ => {}
        }
    }
    ids.into_iter()
        .map(|id| file.span(native.body_item(id).tokens))
        .collect()
}
pub(crate) fn body_component_references(tree: &SyntaxTree) -> HashSet<String> {
    tree.declarations()
        .instantiations
        .iter()
        .map(|instance| normalize_identifier(&instance.name.spelling))
        .collect()
}

fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}

/// Lexical qualified spelling for contract discovery, before package binding.
fn expression_name(id: ExprId, native: &native::Declarations) -> Option<String> {
    match &native
        .expression(native_expressions::ungroup(id, native))
        .kind
    {
        native::ExprKind::Name(name) => Some(native_name(name)),
        native::ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => Some(format!(
            "{}::{}",
            expression_name(*expression, native)?,
            native_name(name)
        )),
        _ => None,
    }
}

fn native_name(name: &native::Name) -> String {
    normalize_identifier(&name.spelling)
}

struct File<'a> {
    code: &'a str,
    syntax_tree: SyntaxTree,
    /// The spans of all tokens other than white space and comments, in order.
    tokens: Vec<Span>,
    /// The start offsets of the identifier tokens.
    identifiers: HashSet<usize>,
    /// The start offsets of the module or interface names of instantiations
    /// and of struct and union member names, which live in other name spaces
    /// than handles.
    type_names: HashSet<usize>,
}

impl<'a> File<'a> {
    fn new(code: &'a str, path: &Path) -> Result<Self, AnalyzerError> {
        let syntax_tree = crate::migration::parse_source(code, path)?;
        let native = syntax_tree.declarations();
        let mut tokens = Vec::new();
        let mut identifiers = HashSet::default();
        let mut type_names = HashSet::default();
        for token in &native.tokens {
            if token.is_trivia() {
                continue;
            }
            let span = native.sources.invocation(token.origin);
            if native.sources.file(span.file).path != path {
                continue;
            }
            tokens.push((span.start, span.end));
        }
        // Value names and authored declarations, rather than keyword-shaped
        // lexer words: an escaped handle can be named `logic` or `input`.
        let mut mark = |name: &native::Name| {
            let span = native.sources.invocation(name.origin);
            if native.sources.file(span.file).path == path {
                identifiers.insert(span.start);
            }
        };
        for expression in &native.expressions {
            match &expression.kind {
                native::ExprKind::Name(name) | native::ExprKind::Member { name, .. } => mark(name),
                _ => {}
            }
        }
        for unit in native.all_units() {
            for port in &unit.ports {
                match &port.kind {
                    native::PortKind::Named(name) | native::PortKind::Explicit { name, .. } => {
                        mark(name)
                    }
                    _ => {}
                }
            }
        }
        for declaration in &native.instantiations {
            for instance in &declaration.instances {
                mark(&instance.name);
            }
        }
        for declaration in &native.data_declarations {
            if let native::DataDeclaration::Variables(declaration)
            | native::DataDeclaration::Constructed(declaration) = declaration
            {
                for variable in &declaration.variables {
                    mark(&variable.name);
                }
            }
        }
        for declaration in &native.net_declarations {
            for variable in &declaration.variables {
                mark(&variable.name);
            }
        }
        for instance in &native.instantiations {
            type_names.insert(native.sources.invocation(instance.name.origin).start);
        }
        for ty in &native.types {
            if let native::DataTypeKind::Struct { fields, .. } = &ty.kind {
                for field in fields {
                    type_names.insert(native.sources.invocation(field.name.origin).start);
                }
            }
        }
        tokens.sort_unstable();
        tokens.dedup();
        Ok(Self {
            code,
            syntax_tree,
            tokens,
            identifiers,
            type_names,
        })
    }
    fn range_span(&self, range: native::TokenRange) -> Option<Span> {
        let native = self.syntax_tree.declarations();
        native
            .tokens
            .get(range.start..range.end)?
            .iter()
            .filter(|token| !token.is_trivia())
            .map(|token| native.sources.invocation(token.origin))
            .fold(None, |range, span| {
                Some(match range {
                    None => (span.start, span.end),
                    Some((start, end)) => (start.min(span.start), end.max(span.end)),
                })
            })
    }
    fn span(&self, range: native::TokenRange) -> Converted<Span> {
        self.range_span(range)
            .ok_or_else(|| unsupported("interface elaboration of an empty node"))
    }
    fn name_span(&self, name: &native::Name) -> Span {
        let span = self
            .syntax_tree
            .declarations()
            .sources
            .invocation(name.origin);
        (span.start, span.end)
    }
    fn expression_span(&self, id: ExprId) -> Converted<Span> {
        self.span(self.syntax_tree.declarations().expression(id).tokens)
    }

    fn token_span(&self, (start, end): Span) -> Span {
        let index = self
            .tokens
            .partition_point(|&(_, token_end)| token_end <= end);
        let end = index
            .checked_sub(1)
            .map(|index| self.tokens[index].1)
            .filter(|&token_end| token_end > start)
            .unwrap_or(start);
        (start, end)
    }

    fn binding_span(&self, binding: &package_owners::Binding) -> Result<Span, AnalyzerError> {
        binding
            .original
            .map(|range| self.token_span(range))
            .ok_or_else(|| unsupported("interface elaboration of an empty import"))
    }

    fn text(&self, (start, end): Span) -> &'a str {
        &self.code[start..end]
    }

    fn previous_token(&self, start: usize) -> Option<&'a str> {
        let index = self.tokens.partition_point(|&(_, end)| end <= start);
        index
            .checked_sub(1)
            .map(|index| self.text(self.tokens[index]))
    }

    /// The references `h<selects>.item` and bare `h` in `span` to a name
    /// `h` that `is_handle` accepts. The references are found in the tokens,
    /// as the parser represents `h.item` either as a hierarchical name or as
    /// a member select of `h`.
    fn handle_references(
        &self,
        span: Span,
        is_handle: impl Fn(&str) -> bool,
    ) -> Vec<TokenReference> {
        let mut references = Vec::new();
        let first = self.tokens.partition_point(|&(start, _)| start < span.0);
        let last = self.tokens.partition_point(|&(start, _)| start < span.1);
        let tokens = &self.tokens[first..last];
        for (index, &token) in tokens.iter().enumerate() {
            if !self.identifiers.contains(&token.0) || self.type_names.contains(&token.0) {
                continue;
            }
            let handle = normalize_identifier(self.text(token));
            if !is_handle(&handle)
                || matches!(self.previous_token(token.0), Some("." | "::"))
                || self.next_token(token.1) == Some("::")
            {
                continue;
            }
            // Element selects, then `.item`.
            let mut next = index + 1;
            while next < tokens.len() && self.text(tokens[next]) == "[" {
                let mut depth = 0usize;
                while next < tokens.len() {
                    match self.text(tokens[next]) {
                        "[" => depth += 1,
                        "]" => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    next += 1;
                }
                next += 1;
            }
            let item = (next + 1 < tokens.len()
                && self.text(tokens[next]) == "."
                && self.identifiers.contains(&tokens[next + 1].0))
            .then(|| {
                (
                    normalize_identifier(self.text(tokens[next + 1])),
                    tokens[next + 1],
                )
            });
            let selects = match &item {
                Some(_) if !self.code[token.1..tokens[next].0].trim().is_empty() => {
                    Some((token.1, tokens[next].0))
                }
                _ => None,
            };
            references.push(TokenReference {
                handle,
                selects,
                item,
                span: token,
            });
        }
        references
    }

    fn next_token(&self, end: usize) -> Option<&'a str> {
        let index = self.tokens.partition_point(|&(start, _)| start < end);
        self.tokens.get(index).map(|&span| self.text(span))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Input,
    Output,
}

impl Direction {
    fn keyword(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// An overridable parameter of an interface.
struct InterfaceParameter {
    name: String,
    /// The data type, unless implicit.
    data_type: Option<Span>,
    default: Option<Span>,
}

/// Imports needed to infer calls made by interface members.
enum HeaderForm {
    Constant,
    Import {
        imports: Vec<(String, Option<String>)>,
    },
}

/// A variable or net declared in the interface scope.
struct Member {
    name: String,
    /// The declared data type, unless implicit.
    data_type: Option<Span>,
    /// The unpacked dimensions of the declarator.
    dimensions: Vec<Span>,
    /// The initializer of a variable.
    initializer: Option<Span>,
}

struct Function {
    name: String,
    span: Span,
}

enum Item {
    /// An overridable parameter, by index into `InterfaceDecl::parameters`.
    Parameter(usize),
    Constant {
        span: Span,
        header: HeaderForm,
    },
    Member(usize),
    Function,
    /// A process, continuous assignment, genvar, or generate construct.
    Process(Span),
}

enum ModportItem {
    Member(Direction, String),
    Import(String),
}

struct InterfaceDecl {
    scope: crate::migration::ScopeHandle,
    name: String,
    file: usize,
    /// The start of the declaration in its file.
    start: usize,
    /// Whether the header has a parameter port list, which makes a body
    /// `parameter` a localparam.
    has_parameter_port_list: bool,
    /// Whether the declaration uses macros or other compiler directives.
    uses_macros: bool,
    /// Whether the declaration refers to compilation-unit items of its file.
    uses_unit_items: bool,
    parameters: Vec<InterfaceParameter>,
    type_parameters: Vec<String>,
    parameter_order: Vec<String>,
    items: Vec<Item>,
    members: Vec<Member>,
    functions: Vec<Function>,
    modports: HashMap<String, Vec<ModportItem>>,
    /// The names declared in the interface scope.
    names: HashSet<String>,
    /// The references to interface-scope names in the interface text.
    references: Vec<(Span, String)>,
    /// The start offsets of the assignment targets in the interface text.
    lvalues: HashSet<usize>,
}

impl InterfaceDecl {
    fn header_dependent_cast(&self, file: &File<'_>) -> Result<bool, AnalyzerError> {
        let mut spans = self.declaration_spans();
        spans.extend(
            self.function_closure(&self.declaration_functions())
                .iter()
                .filter_map(|name| self.function(name).map(|function| function.span)),
        );
        let mut dependent = self
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<HashSet<_>>();
        let native = file.syntax_tree.declarations();
        // Declaration names are not expression references in the native tree.
        // Include them in the dependency closure so a localparam or typedef
        // alias of a header parameter cannot hide a dependent cast target.
        let declarations = native
            .parameter_declarations
            .iter()
            .flat_map(|declaration| {
                declaration
                    .parameters
                    .iter()
                    .map(|parameter| &parameter.name)
            })
            .chain(native.type_aliases.iter().map(|alias| &alias.name))
            .map(|name| (file.name_span(name), native_name(name)))
            .collect::<Vec<_>>();
        loop {
            let before = dependent.len();
            for span in &spans {
                let names = self
                    .referenced(*span)
                    .chain(declarations.iter().filter_map(|(name_span, name)| {
                        (span.0 <= name_span.0 && name_span.1 <= span.1).then_some(name.as_str())
                    }))
                    .collect::<Vec<_>>();
                if names.iter().any(|name| dependent.contains(name)) {
                    dependent.extend(names);
                }
            }
            if dependent.len() == before {
                break;
            }
        }
        for expression in &native.expressions {
            let native::ExprKind::Cast { target, .. } = expression.kind else {
                continue;
            };
            let target = file.expression_span(target)?;
            if spans
                .iter()
                .any(|span| span.0 <= target.0 && target.1 <= span.1)
                && self.referenced(target).any(|name| dependent.contains(name))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name == name)
    }

    fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
    }

    /// The interface-scope names that `span` refers to.
    fn referenced(&self, span: Span) -> impl Iterator<Item = &str> {
        self.references
            .iter()
            .filter(move |((start, end), _)| *start >= span.0 && *end <= span.1)
            .map(|(_, name)| name.as_str())
    }

    /// `functions` and every function they call, in declaration order.
    fn function_closure(&self, functions: &HashSet<String>) -> Vec<String> {
        let mut closure = functions.clone();
        let mut pending: Vec<String> = functions.iter().cloned().collect();
        while let Some(name) = pending.pop() {
            let Some(function) = self.function(&name) else {
                continue;
            };
            for called in self.referenced(function.span) {
                if self.function(called).is_some() && closure.insert(called.to_string()) {
                    pending.push(called.to_string());
                }
            }
        }
        self.functions
            .iter()
            .filter(|function| closure.contains(&function.name))
            .map(|function| function.name.clone())
            .collect()
    }

    /// The spans of parameter, constant and member declarations.
    fn declaration_spans(&self) -> Vec<Span> {
        let mut spans = Vec::new();
        for item in &self.items {
            match item {
                Item::Parameter(index) => {
                    let parameter = &self.parameters[*index];
                    spans.extend(parameter.data_type);
                    spans.extend(parameter.default);
                }
                Item::Constant { span, .. } => spans.push(*span),
                Item::Member(index) => {
                    let member = &self.members[*index];
                    spans.extend(member.data_type);
                    spans.extend(member.dimensions.iter().copied());
                }
                Item::Function | Item::Process(_) => {}
            }
        }
        spans
    }

    /// The functions that parameter, constant and member declarations call,
    /// which every expansion of the interface declares.
    fn declaration_functions(&self) -> HashSet<String> {
        self.declaration_spans()
            .into_iter()
            .flat_map(|span| self.referenced(span))
            .filter(|name| self.function(name).is_some())
            .map(str::to_string)
            .collect()
    }

    /// The members that parameter, constant and member declarations refer
    /// to.
    fn declaration_member_references(&self) -> impl Iterator<Item = &str> {
        self.declaration_spans()
            .into_iter()
            .flat_map(|span| self.referenced(span))
            .filter(|name| self.member(name).is_some())
    }

    /// The members that `functions` access: an output for a member one of
    /// them assigns, an input for one they only read.
    fn captured_members(&self, functions: &[String]) -> HashMap<String, Direction> {
        let mut captured = HashMap::default();
        for function in functions.iter().filter_map(|name| self.function(name)) {
            for ((start, end), name) in &self.references {
                if *start < function.span.0 || *end > function.span.1 || self.member(name).is_none()
                {
                    continue;
                }
                let direction = captured.entry(name.clone()).or_insert(Direction::Input);
                if self.lvalues.contains(start) {
                    *direction = Direction::Output;
                }
            }
        }
        captured
    }
}

/// An interface port of a module.
#[derive(Clone)]
struct InterfacePort {
    /// The interface, or `None` for a generic `interface` port.
    interface: Option<String>,
    modport: Option<String>,
    dimensions: Vec<Span>,
}

struct PortDecl {
    name: String,
    interface: Option<InterfacePort>,
    /// Whether the module may write the actual (`output`, `inout`, `ref`).
    writes: bool,
}

struct ModuleDecl {
    scope: crate::migration::ScopeHandle,
    name: String,
    file: usize,
    span: Span,
    ports: Vec<PortDecl>,
}

impl ModuleDecl {
    fn port(&self, name: &str) -> Option<&PortDecl> {
        self.ports.iter().find(|port| port.name == name)
    }

    fn has_interface_ports(&self) -> bool {
        self.ports.iter().any(|port| port.interface.is_some())
    }
}

/// What a module port of an interface type carries: the members in interface
/// declaration order and the functions it reaches.
#[derive(Clone)]
struct Expansion {
    members: Vec<(String, Direction)>,
    /// The functions to declare: the callable ones and those they and the
    /// declarations call.
    functions: Vec<String>,
    /// The functions the module may call, or `None` for all of them.
    callable: Option<HashSet<String>>,
    /// The members the module may access, or `None` for all of them. A
    /// modport port also carries the members that imported functions and
    /// declarations use.
    accessible: Option<HashSet<String>>,
}

impl Expansion {
    fn direction(&self, member: &str) -> Option<Direction> {
        self.members
            .iter()
            .find(|(name, _)| name == member)
            .map(|(_, direction)| *direction)
    }
}

/// The interface a port connection binds and how it names it.
struct Actual {
    handle: String,
    /// The span of the element selects after the handle name.
    selects: Option<Span>,
    /// A modport named in the connection (`h.mp`).
    modport: Option<String>,
}

/// An interface known inside a module: an instance or a port.
#[derive(Clone)]
struct Handle {
    interface: String,
    /// The expansion of a port; `None` for an instance, which has every member.
    port: Option<Expansion>,
    arrayed: bool,
}

/// The interface handles of a module, by name and declaring scope: the
/// module or a generate block.
#[derive(Default)]
struct Handles {
    by_name: HashMap<String, Vec<(Span, Handle)>>,
}

impl Handles {
    fn insert(&mut self, name: String, scope: Span, handle: Handle) {
        self.by_name.entry(name).or_default().push((scope, handle));
    }

    /// The handle `name` refers to at `span`: the one of the innermost
    /// scope around it.
    fn at(&self, name: &str, span: Span) -> Option<&Handle> {
        self.by_name
            .get(name)?
            .iter()
            .filter(|(scope, _)| scope.0 <= span.0 && span.1 <= scope.1)
            .min_by_key(|(scope, _)| scope.1 - scope.0)
            .map(|(_, handle)| handle)
    }
}

/// A child instantiation found in a module body: its module, the formal
/// interface port and the name of the handle connected to it.
struct ChildBinding {
    module: String,
    formal: String,
    handle: String,
    context: Context,
    /// The interfaces bound to the child's generic ports, sorted.
    bindings: Vec<(String, String)>,
}

/// A module, its port, the interface bound to it and the bindings of the
/// module's other generic ports.
type ExpansionKey = (String, String, String, Vec<(String, String)>);

struct Design<'a> {
    files: &'a [File<'a>],
    interfaces: HashMap<String, InterfaceDecl>,
    modules: HashMap<String, ModuleDecl>,
    /// The ports of every subroutine declared or imported, by name.
    subroutines: HashMap<String, Vec<(Scope, usize, SubroutinePorts)>>,
    expansions: std::cell::RefCell<HashMap<ExpansionKey, Expansion>>,
    /// The expansions being computed, to reject a recursive instantiation.
    expanding: std::cell::RefCell<HashSet<ExpansionKey>>,
}

impl<'a> Design<'a> {
    fn collect(files: &'a [File<'a>]) -> Result<Self, AnalyzerError> {
        Self::collect_selected(files, None)
    }

    fn collect_selected(
        files: &'a [File<'a>],
        selected: Option<&HashSet<String>>,
    ) -> Converted<Self> {
        let interface_names = files
            .iter()
            .map(|file| roots(&file.syntax_tree))
            .collect::<Converted<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter(|(_, unit)| unit.kind == native::UnitKind::Interface)
            .map(|(_, unit)| native_name(&unit.name))
            .collect::<HashSet<_>>();
        let mut interfaces = HashMap::default();
        for (index, file) in files.iter().enumerate() {
            for (handle, unit) in roots(&file.syntax_tree)? {
                if unit.kind != native::UnitKind::Interface {
                    continue;
                }
                let name = native_name(&unit.name);
                if selected.is_some_and(|selected| !selected.contains(&name)) {
                    continue;
                }
                if unit.port_style != native::PortStyle::Ansi {
                    return Err(unsupported("non-ANSI interface declarations"));
                }
                let mut interface = InterfaceDecl::collect(handle, index, file, &interface_names)?;
                let unit_names = unit_names(file)?;
                let declared = declared_names(unit, file)?
                    .into_iter()
                    .collect::<HashSet<_>>();
                let scoped = file.syntax_tree.scoped(handle)?;
                interface.uses_unit_items = imports::unqualified_names(&scoped)
                    .keys()
                    .any(|name| unit_names.contains(name) && !declared.contains(name));
                if !interface.uses_unit_items {
                    let native = file.syntax_tree.declarations();
                    interface.uses_unit_items=native.expressions.iter().any(|expression|unit.tokens.start<=expression.tokens.start&&expression.tokens.end<=unit.tokens.end&&matches!(&expression.kind,native::ExprKind::Member{expression,separator,name} if separator=="::"&&expression_name(*expression,native).as_deref()==Some("$unit")&&unit_names.contains(&native_name(name))));
                }
                if interfaces.insert(name.clone(), interface).is_some() {
                    return Err(AnalyzerError::DuplicateModule { name });
                }
            }
        }
        let mut modules = HashMap::default();
        let mut subroutines: HashMap<String, Vec<(Scope, usize, SubroutinePorts)>> =
            HashMap::default();
        if !interfaces.is_empty() {
            for (index, file) in files.iter().enumerate() {
                let native = file.syntax_tree.declarations();
                let scopes = Scope::collect(file)?;
                for (handle, unit) in roots(&file.syntax_tree)? {
                    if unit.kind == native::UnitKind::Module
                        && unit.port_style == native::PortStyle::Ansi
                    {
                        let module = ModuleDecl::collect(handle, index, file, &interfaces)?;
                        if interfaces.contains_key(&module.name) {
                            return Err(AnalyzerError::DuplicateModule { name: module.name });
                        }
                        modules.insert(module.name.clone(), module);
                    }
                }
                for subroutine in &native.subroutines {
                    let scope = Scope::of(file.span(subroutine.tokens)?, &scopes);
                    subroutines
                        .entry(native_name(&subroutine.name))
                        .or_default()
                        .push((scope, index, subroutine_ports(subroutine)));
                }
                for declaration in &native.dpi_declarations {
                    if !declaration.import {
                        continue;
                    }
                    let scope = Scope::of(file.span(declaration.tokens)?, &scopes);
                    let mut inherited = false;
                    let ports = declaration
                        .ports
                        .iter()
                        .map(|port| {
                            if let Some(direction) = port.direction {
                                inherited = direction != native::Direction::Input
                                    && !(direction == native::Direction::Ref && port.constant);
                            }
                            (port.name.as_ref().map(native_name), inherited)
                        })
                        .collect();
                    subroutines
                        .entry(native_name(&declaration.name))
                        .or_default()
                        .push((scope, index, ports));
                }
            }
        }
        for (index, file) in files.iter().enumerate() {
            if !interfaces.values().any(|interface| interface.file == index) {
                continue;
            }
            let unit_imports = unit_imports(file)?;
            for interface in interfaces
                .values_mut()
                .filter(|interface| interface.file == index)
            {
                let preceding = unit_imports
                    .iter()
                    .filter(|(span, _)| span.1 <= interface.start)
                    .map(|(span, imports)| Item::Constant {
                        span: *span,
                        header: HeaderForm::Import {
                            imports: imports.clone(),
                        },
                    })
                    .collect::<Vec<_>>();
                interface.items.splice(0..0, preceding);
            }
        }
        for interface in interfaces.values_mut() {
            let file = &files[interface.file];
            let native = file.syntax_tree.declarations();
            let unit = interface
                .scope
                .unit(native)
                .ok_or_else(|| unsupported("missing native interface scope"))?;
            let outer = interface
                .items
                .iter()
                .flat_map(|item| match item {
                    Item::Constant {
                        header: HeaderForm::Import { imports },
                        ..
                    } => imports.clone(),
                    _ => Vec::new(),
                })
                .collect::<Vec<_>>();
            let scoped = scoped_imports(interface.scope, file)?;
            for expression in &native.expressions {
                if unit.tokens.start <= expression.tokens.start
                    && expression.tokens.end <= unit.tokens.end
                    && matches!(expression.kind, native::ExprKind::Call { .. })
                {
                    let span = file.span(expression.tokens)?;
                    written_arguments(
                        expression,
                        &Scope::Interface(interface.name.clone()),
                        &|_| None,
                        file,
                        interface.file,
                        &subroutines,
                        &visible_imports(&outer, &scoped, span),
                        &mut interface.lvalues,
                    )?;
                }
            }
        }
        Ok(Self {
            files,
            interfaces,
            modules,
            subroutines,
            expansions: Default::default(),
            expanding: Default::default(),
        })
    }

    fn interface(&self, name: &str) -> Result<&InterfaceDecl, AnalyzerError> {
        self.interfaces
            .get(name)
            .ok_or_else(|| unsupported(format!("unknown interface `{name}`")))
    }

    fn module_unit(&self, module: &ModuleDecl) -> Converted<&'a native::Unit> {
        module
            .scope
            .unit(self.files[module.file].syntax_tree.declarations())
            .ok_or_else(|| unsupported("missing native module scope"))
    }

    fn expansion(
        &self,
        module: &str,
        port: &str,
        interface: &str,
        bindings: &[(String, String)],
    ) -> Result<Expansion, AnalyzerError> {
        let key = (
            module.to_string(),
            port.to_string(),
            interface.to_string(),
            bindings.to_vec(),
        );
        if let Some(expansion) = self.expansions.borrow().get(&key) {
            return Ok(expansion.clone());
        }
        if !self.expanding.borrow_mut().insert(key.clone()) {
            return Err(unsupported(format!(
                "recursive instantiation through interface port `{port}` of module `{module}`"
            )));
        }
        let expansion = self.compute_expansion(module, port, interface, bindings);
        self.expanding.borrow_mut().remove(&key);
        let expansion = expansion?;
        self.expansions.borrow_mut().insert(key, expansion.clone());
        Ok(expansion)
    }

    fn compute_expansion(
        &self,
        module_name: &str,
        port_name: &str,
        interface_name: &str,
        bindings: &[(String, String)],
    ) -> Result<Expansion, AnalyzerError> {
        let module = &self.modules[module_name];
        let port = module
            .port(port_name)
            .and_then(|port| port.interface.clone())
            .ok_or_else(|| {
                unsupported(format!(
                    "module `{module_name}` has no interface port `{port_name}`"
                ))
            })?;
        let uses = self.module_uses(module_name, (port_name, interface_name), bindings)?;
        let interface = self.interface(interface_name)?;
        // A port declares the interface's parameters, constants and types in
        // the module header, before the member ports they would refer to.
        if let Some(member) = interface.declaration_member_references().next() {
            return Err(unsupported(format!(
                "reference to member `{member}` in a declaration of interface `{interface_name}`, which port `{port_name}` of module `{module_name}` carries"
            )));
        }
        // The directions do not depend on generate conditions, loop bounds
        // or which subroutines are called, which are not evaluated here, so
        // a write that depends on them needs a modport, and an imported
        // function that writes must be called unconditionally.
        let through = match &port.modport {
            Some(modport) => format!(" through its port `{port_name}` of modport `{modport}`"),
            None => format!(", whose port `{port_name}` has no modport"),
        };
        let conditional_write = |member: &str, context: Context| {
            unsupported(format!(
                "write of `{port_name}.{member}` {} of module `{module_name}`{through}",
                context.description()
            ))
        };
        if let Some(modport) = &port.modport {
            let items = interface.modports.get(modport).ok_or_else(|| {
                unsupported(format!(
                    "interface `{interface_name}` has no modport `{modport}`"
                ))
            })?;
            let mut listed: HashMap<String, Direction> = HashMap::default();
            let mut imports = HashSet::default();
            for item in items {
                match item {
                    ModportItem::Member(direction, member) => {
                        listed.insert(member.clone(), *direction);
                    }
                    ModportItem::Import(function) => {
                        imports.insert(function.clone());
                    }
                }
            }
            let accessible: HashSet<String> = listed.keys().cloned().collect();
            // An imported function runs in the interface scope, so the
            // members it accesses pass through the port whatever the modport
            // lists, but only when the module or a child calls it.
            let imported = interface.captured_members(&interface.function_closure(&imports));
            let mut called = interface.declaration_functions();
            for (handle, item, context) in &uses.references {
                if handle != port_name || !imports.contains(item) {
                    continue;
                }
                let closure = interface.function_closure(&HashSet::from_iter([item.clone()]));
                for (member, direction) in interface.captured_members(&closure) {
                    if direction == Direction::Output && *context != Context::Module {
                        return Err(conditional_write(&member, *context));
                    }
                }
                called.insert(item.clone());
            }
            let functions = interface.function_closure(&called);
            for (member, direction) in interface.captured_members(&functions) {
                let listed = listed.entry(member).or_insert(direction);
                if direction == Direction::Output {
                    *listed = Direction::Output;
                }
            }
            for child in uses
                .children
                .iter()
                .filter(|child| child.handle == port_name)
            {
                let child_expansion = self.expansion(
                    &child.module,
                    &child.formal,
                    interface_name,
                    &child.bindings,
                )?;
                for (member, direction) in child_expansion.members {
                    let Some(access) = imported.get(&member) else {
                        continue;
                    };
                    if direction == Direction::Output
                        && *access == Direction::Output
                        && listed.get(&member) != Some(&Direction::Output)
                    {
                        if child.context != Context::Module {
                            return Err(conditional_write(&member, child.context));
                        }
                        listed.insert(member, Direction::Output);
                    } else {
                        listed.entry(member).or_insert(Direction::Input);
                    }
                }
            }
            return Ok(Expansion {
                members: interface
                    .members
                    .iter()
                    .filter_map(|member| {
                        listed
                            .get(&member.name)
                            .map(|direction| (member.name.clone(), *direction))
                    })
                    .collect(),
                functions,
                callable: Some(imports),
                accessible: Some(accessible),
            });
        }
        // Without a modport every member is reachable. A member the module or
        // a child writes is an output.
        let mut written: HashSet<String> = HashSet::default();
        for (handle, member, context) in &uses.writes {
            if handle == port_name && interface.member(member).is_some() {
                if *context != Context::Module {
                    return Err(conditional_write(member, *context));
                }
                written.insert(member.clone());
            }
        }
        let mut called = interface.declaration_functions();
        for (handle, item, context) in &uses.references {
            if handle != port_name || interface.function(item).is_none() {
                continue;
            }
            let closure = interface.function_closure(&HashSet::from_iter([item.clone()]));
            for (member, direction) in interface.captured_members(&closure) {
                if direction == Direction::Output {
                    if *context != Context::Module {
                        return Err(conditional_write(&member, *context));
                    }
                    written.insert(member);
                }
            }
            called.insert(item.clone());
        }
        let functions = interface.function_closure(&called);
        let member_names: Vec<String> = interface
            .members
            .iter()
            .map(|member| member.name.clone())
            .collect();
        for child in uses
            .children
            .iter()
            .filter(|child| child.handle == port_name)
        {
            let child_expansion = self.expansion(
                &child.module,
                &child.formal,
                interface_name,
                &child.bindings,
            )?;
            for (member, direction) in child_expansion.members {
                if direction == Direction::Output {
                    if child.context != Context::Module {
                        return Err(conditional_write(&member, child.context));
                    }
                    written.insert(member);
                }
            }
        }
        Ok(Expansion {
            members: member_names
                .into_iter()
                .map(|member| {
                    let direction = if written.contains(&member) {
                        Direction::Output
                    } else {
                        Direction::Input
                    };
                    (member, direction)
                })
                .collect(),
            functions,
            callable: None,
            accessible: None,
        })
    }

    /// The interface accesses of a module body, by handle name. `bound`
    /// names a port and the interface it is bound to.
    fn module_uses(
        &self,
        module_name: &str,
        bound: (&str, &str),
        bindings: &[(String, String)],
    ) -> Converted<ModuleUses> {
        let module = &self.modules[module_name];
        let file = &self.files[module.file];
        let native = file.syntax_tree.declarations();
        let unit = self.module_unit(module)?;
        let items = body_items(unit, native)?;
        let blocks = block_spans(unit, file)?;
        let mut uses = ModuleUses::default();
        let mut handle_interfaces: HashMap<String, Option<String>> = HashMap::default();
        for port in &module.ports {
            if let Some(interface) = &port.interface {
                let interface = match &interface.interface {
                    None if port.name == bound.0 => Some(bound.1.to_string()),
                    None => bindings
                        .iter()
                        .find(|(name, _)| *name == port.name)
                        .map(|(_, interface)| interface.clone()),
                    interface => interface.clone(),
                };
                handle_interfaces.insert(port.name.clone(), interface);
            }
        }
        let mut instances = Vec::new();
        for id in &items {
            let native::BodyItemKind::Instance(instance) = native.body_item(*id).kind else {
                continue;
            };
            let declaration = native.instantiation(instance);
            let type_name = native_name(&declaration.name);
            if !self.interfaces.contains_key(&type_name) {
                continue;
            }
            let span = file.span(declaration.tokens)?;
            let scope = blocks
                .iter()
                .filter(|block| block.0 <= span.0 && span.1 <= block.1)
                .min_by_key(|block| block.1 - block.0)
                .copied()
                .unwrap_or(module.span);
            for instance in &declaration.instances {
                instances.push((native_name(&instance.name), scope, type_name.clone()));
            }
        }
        let instance_at = |handle: &str, span: Span| {
            instances
                .iter()
                .filter(|(instance, scope, _)| {
                    instance == handle && scope.0 <= span.0 && span.1 <= scope.1
                })
                .min_by_key(|(_, scope, _)| scope.1 - scope.0)
                .map(|(_, _, interface)| interface.clone())
        };
        let interface_at = |handle: &str, span: Span| {
            instance_at(handle, span).or_else(|| handle_interfaces.get(handle).cloned().flatten())
        };
        let is_port = |handle: &str, span: Span| instance_at(handle, span).is_none();
        let outer = unit_imports(file)?
            .into_iter()
            .filter(|(span, _)| span.1 <= module.span.0)
            .flat_map(|(_, imports)| imports)
            .collect::<Vec<_>>();
        let scoped = scoped_imports(module.scope, file)?;
        let mut lvalues = HashSet::default();
        let mut generates = Vec::new();
        let mut subroutines = Vec::new();
        for id in &items {
            let item = native.body_item(*id);
            match item.kind {
                native::BodyItemKind::Subroutine(_) => subroutines.push(file.span(item.tokens)?),
                native::BodyItemKind::Conditional { .. }
                | native::BodyItemKind::Case { .. }
                | native::BodyItemKind::Loop { .. } => generates.push(file.span(item.tokens)?),
                _ => {}
            }
        }
        for expression in &native.expressions {
            if unit.tokens.start <= expression.tokens.start
                && expression.tokens.end <= unit.tokens.end
            {
                if matches!(expression.kind, native::ExprKind::Call { .. }) {
                    let span = file.span(expression.tokens)?;
                    written_arguments(
                        expression,
                        &Scope::Module(module_name.to_string()),
                        &|handle| interface_at(handle, span),
                        file,
                        module.file,
                        &self.subroutines,
                        &visible_imports(&outer, &scoped, span),
                        &mut lvalues,
                    )?;
                }
                if let Some(destination) = readmem_destination(expression, file)? {
                    written_expression(destination, file, &mut lvalues);
                }
                if let native::ExprKind::Unary {
                    operator,
                    expression,
                    ..
                } = &expression.kind
                    && matches!(operator.as_str(), "++" | "--")
                {
                    written_expression(*expression, file, &mut lvalues);
                }
            }
        }
        for statement in &native.statements {
            if unit.tokens.start <= statement.tokens.start
                && statement.tokens.end <= unit.tokens.end
                && let native::StatementKind::Assignment { left, .. } = statement.kind
            {
                written_expression(left, file, &mut lvalues);
            }
        }
        for assign in &native.assign_statements {
            if unit.tokens.start <= assign.tokens.start && assign.tokens.end <= unit.tokens.end {
                for assignment in &assign.assignments {
                    written_expression(assignment.left, file, &mut lvalues);
                }
            }
        }
        let context = |span| Context::of(span, &generates, &subroutines);
        let mut unknown = HashSet::default();
        for id in &items {
            let native::BodyItemKind::Instance(instance) = native.body_item(*id).kind else {
                continue;
            };
            let instantiation = native.instantiation(instance);
            let child_name = native_name(&instantiation.name);
            if self.interfaces.contains_key(&child_name) {
                continue;
            }
            let Some(child) = self.modules.get(&child_name) else {
                for instance in &instantiation.instances {
                    for connection in &instance.connections {
                        let actual = match connection.kind {
                            native::InstanceConnectionKind::Ordered(actual) => actual,
                            native::InstanceConnectionKind::Named {
                                value: native::ConnectionValue::Expression(actual),
                                ..
                            } => Some(actual),
                            _ => None,
                        };
                        if let Some(actual) = actual {
                            let mut pending = vec![actual];
                            let mut seen = HashSet::default();
                            while let Some(id) = pending.pop() {
                                if !seen.insert(id) {
                                    continue;
                                }
                                unknown.extend(
                                    file.range_span(native.expression(id).tokens)
                                        .map(|span| span.0),
                                );
                                pending.extend(parameters::skipped::children(id, native));
                            }
                        }
                    }
                }
                continue;
            };
            let instance_context = context(file.span(instantiation.tokens)?);
            for instance in &instantiation.instances {
                let actuals = connections(instance, child, file, child.has_interface_ports())?;
                let mut bindings = Vec::new();
                for (formal, actual) in &actuals {
                    if let Some(port) = child.port(formal)
                        && port
                            .interface
                            .as_ref()
                            .is_some_and(|port| port.interface.is_none())
                        && let Some((actual, span)) = actual.interface_actual(formal, file)?
                        && let Some(interface) = interface_at(&actual.handle, span)
                    {
                        bindings.push((formal.clone(), interface));
                    }
                }
                bindings.sort();
                for (formal, actual) in actuals {
                    let Some(port) = child.port(&formal) else {
                        continue;
                    };
                    if port.interface.is_none() {
                        if port.writes
                            && let Connection::Explicit(Some(expression)) = actual
                        {
                            written_expression(expression, file, &mut lvalues);
                        }
                    } else if let Some((actual, span)) = actual.interface_actual(&formal, file)?
                        && is_port(&actual.handle, span)
                    {
                        uses.children.push(ChildBinding {
                            module: child_name.clone(),
                            formal,
                            handle: actual.handle,
                            context: instance_context,
                            bindings: bindings.clone(),
                        });
                    }
                }
            }
        }
        let ports = module
            .ports
            .iter()
            .filter(|port| port.interface.is_some())
            .map(|port| port.name.as_str())
            .collect::<HashSet<_>>();
        for reference in file.handle_references(module.span, |name| ports.contains(name)) {
            if !is_port(&reference.handle, reference.span) {
                continue;
            }
            if let Some((item, _)) = reference.item {
                let context = context(reference.span);
                if unknown.contains(&reference.span.0) {
                    uses.writes.push((
                        reference.handle.clone(),
                        item.clone(),
                        Context::UnknownPort,
                    ));
                }
                if lvalues.contains(&reference.span.0) {
                    uses.writes
                        .push((reference.handle.clone(), item.clone(), context));
                }
                uses.references.push((reference.handle, item, context));
            }
        }
        Ok(uses)
    }
}

fn declared_names(unit: &native::Unit, file: &File<'_>) -> Converted<Vec<String>> {
    let native = file.syntax_tree.declarations();
    let mut names = Vec::new();
    let mut statements = Vec::new();
    names.extend(
        unit.parameters
            .iter()
            .flatten()
            .map(|parameter| native_name(&parameter.name)),
    );
    for id in body_items(unit, native)? {
        let item = native.body_item(id);
        names.extend(imports::item_names(id, &file.syntax_tree));
        match &item.kind {
            native::BodyItemKind::Subroutine(id) => {
                let sub = native.subroutine(*id);
                names.extend(
                    sub.ports
                        .iter()
                        .flatten()
                        .chain(sub.port_declarations.iter().flat_map(|group| &group.ports))
                        .filter_map(|port| port.name.as_ref().map(native_name)),
                );
                statements.extend(&sub.authored_statements);
            }
            native::BodyItemKind::Procedure(id) => {
                statements.extend(&native.procedure(*id).authored_statements)
            }
            native::BodyItemKind::Loop { variable, .. } => names.push(native_name(variable)),
            native::BodyItemKind::Unconverted { category, .. } if category == "genvar" => names
                .extend(
                    native.tokens[item.tokens.start..item.tokens.end]
                        .iter()
                        .filter(|token| token.text != "genvar")
                        .filter_map(|token| token.identifier().map(normalize_identifier)),
                ),
            _ => {}
        }
    }
    let mut seen = HashSet::default();
    while let Some(id) = statements.pop() {
        if !seen.insert(id) {
            continue;
        }
        let statement = native.statement(id);
        match &statement.kind {
            native::StatementKind::Declaration(declaration) => match declaration {
                native::LocalDeclaration::Variables(decl)
                | native::LocalDeclaration::Constructed(decl) => names.extend(
                    decl.variables
                        .iter()
                        .map(|variable| native_name(&variable.name)),
                ),
                native::LocalDeclaration::Parameters(id) => names.extend(
                    native
                        .parameter_declaration(*id)
                        .parameters
                        .iter()
                        .map(|parameter| native_name(&parameter.name)),
                ),
                native::LocalDeclaration::TypeAlias(id) => {
                    names.push(native_name(&native.type_alias(*id).name))
                }
                _ => {}
            },
            native::StatementKind::Loop {
                kind: native::LoopSyntax::For(header),
                ..
            } => {
                if let Some(native::ForInitialization::Declarations(decls)) = &header.initialization
                {
                    for decl in decls {
                        names.extend(
                            decl.variables
                                .iter()
                                .map(|variable| native_name(&variable.name)),
                        );
                    }
                }
            }
            _ => {}
        }
        statements.extend(statement.kind.children());
    }
    Ok(names)
}

fn clone_module_name(module: &str, bindings: &[(String, String)]) -> String {
    let mut name = module.to_string();
    for (_, interface) in bindings {
        name.push_str(SEPARATOR);
        name.push_str(interface);
    }
    name
}

/// Add to `writes` the start offsets of the actual arguments of `call`
/// that the called subroutine may write. A subroutine is found by name;
/// when its declarations disagree about an argument, the call is
/// rejected.
fn written_arguments(
    call: &native::Expr,
    local: &Scope,
    handle_interface: &dyn Fn(&str) -> Option<String>,
    file: &File<'_>,
    file_index: usize,
    subroutines: &HashMap<String, Vec<(Scope, usize, SubroutinePorts)>>,
    imports: &[(String, Option<String>)],
    writes: &mut HashSet<usize>,
) -> Result<(), AnalyzerError> {
    let native = file.syntax_tree.declarations();
    let native::ExprKind::Call {
        expression,
        arguments,
    } = &call.kind
    else {
        return Ok(());
    };
    let (callee, package, handle) = match &native.expression(*expression).kind {
        native::ExprKind::Name(name) => (native_name(name), None, None),
        native::ExprKind::Member {
            expression,
            separator,
            name,
        } => {
            if separator == "::" {
                (
                    native_name(name),
                    expression_name(*expression, native),
                    None,
                )
            } else if separator == "." {
                let mut base = *expression;
                while let native::ExprKind::Select { expression, .. } = native.expression(base).kind
                {
                    base = expression;
                }
                (native_name(name), None, expression_name(base, native))
            } else {
                return Ok(());
            }
        }
        _ => return Ok(()),
    };
    let Some(declared) = subroutines.get(&callee) else {
        return Ok(());
    };
    // The declarations the call can refer to: of the named package, of an
    // interface for a call through a handle, or of the calling module or
    // interface itself, and otherwise of the compilation unit or a package
    // that `imports` import it from.
    // A compilation-unit declaration is visible only in its own file.
    let in_scope = |filter: &dyn Fn(&Scope) -> bool| -> Vec<&SubroutinePorts> {
        declared
            .iter()
            .filter(|(scope, index, _)| {
                (*scope != Scope::Unit || *index == file_index) && filter(scope)
            })
            .map(|(_, _, ports)| ports)
            .collect()
    };
    let declarations = if let Some(package) = package {
        in_scope(&|scope| match scope {
            Scope::Package(name) => *name == package,
            Scope::Unit => package == "$unit",
            _ => false,
        })
    } else if let Some(handle) = handle {
        match handle_interface(&handle) {
            Some(interface) => in_scope(&|scope| *scope == Scope::Interface(interface.clone())),
            None => in_scope(&|scope| matches!(scope, Scope::Interface(_))),
        }
    } else {
        let own = in_scope(&|scope| scope == local);
        if own.is_empty() {
            in_scope(&|scope| match scope {
                Scope::Package(package) => imports.iter().any(|(imported, item)| {
                    imported == package && item.as_ref().is_none_or(|item| *item == callee)
                }),
                Scope::Unit => true,
                _ => false,
            })
        } else {
            own
        }
    };
    // Positional actuals by index, named ones by formal name.
    let actuals = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            argument.value.map(|actual| {
                (
                    argument
                        .name
                        .as_ref()
                        .map_or(Ok(index), |name| Err(native_name(name))),
                    actual,
                )
            })
        });
    for (formal, actual) in actuals {
        let mut directions = declarations.iter().filter_map(|ports| match &formal {
            Ok(index) => ports.get(*index).map(|(_, writes)| *writes),
            Err(formal) => ports
                .iter()
                .find(|(name, _)| name.as_deref() == Some(formal.as_str()))
                .map(|(_, writes)| *writes),
        });
        let Some(first) = directions.next() else {
            continue;
        };
        if directions.any(|writes| writes != first) {
            return Err(unsupported(format!(
                "call of `{callee}`, whose declarations disagree about the direction of an argument, in a module with interface ports"
            )));
        }
        if first {
            written_expression(actual, file, writes);
        }
    }
    Ok(())
}

/// The package imports under `root`, each with the scope it is visible in:
/// the innermost subroutine or block around it, or `root`.
fn scoped_imports(
    handle: ScopeHandle,
    file: &File<'_>,
) -> Result<Vec<(Span, Vec<(String, Option<String>)>)>, AnalyzerError> {
    let root_span = file.span(
        handle
            .unit(file.syntax_tree.declarations())
            .ok_or_else(|| unsupported("missing native import scope"))?
            .tokens,
    )?;
    let mut scopes = Vec::new();
    let mut imports = Vec::new();
    let scoped = file.syntax_tree.scoped(handle)?;
    for view in package_owners::scope(&scoped)? {
        if file
            .syntax_tree
            .declarations()
            .package_binding(view.id)
            .kind
            == celox_sv_syntax::PackageBindingKind::Import
        {
            imports.push((
                file.binding_span(&view)?,
                imported_items(view.id, &file.syntax_tree)?,
            ));
        }
    }
    let native = file.syntax_tree.declarations();
    let unit = scoped.root_unit()?;
    for id in body_items(unit, native)? {
        if matches!(
            native.body_item(id).kind,
            native::BodyItemKind::Block { .. }
                | native::BodyItemKind::Loop { .. }
                | native::BodyItemKind::Subroutine(_)
        ) {
            scopes.extend(file.range_span(native.body_item(id).tokens));
        }
    }
    for statement in &native.statements {
        if unit.tokens.start <= statement.tokens.start
            && statement.tokens.end <= unit.tokens.end
            && matches!(
                statement.kind,
                native::StatementKind::Block { .. } | native::StatementKind::Parallel { .. }
            )
        {
            scopes.extend(file.range_span(statement.tokens));
        }
    }
    Ok(imports
        .into_iter()
        .map(|(span, imported)| {
            let scope = scopes
                .iter()
                .filter(|scope| scope.0 <= span.0 && span.1 <= scope.1)
                .min_by_key(|scope| scope.1 - scope.0)
                .copied()
                .unwrap_or(root_span);
            (scope, imported)
        })
        .collect())
}

/// The imports visible at `span`: `outer` ones and the `scoped` ones whose
/// scope contains it.
fn visible_imports(
    outer: &[(String, Option<String>)],
    scoped: &[(Span, Vec<(String, Option<String>)>)],
    span: Span,
) -> Vec<(String, Option<String>)> {
    let mut imports = outer.to_vec();
    for (scope, imported) in scoped {
        if scope.0 <= span.0 && span.1 <= scope.1 {
            imports.extend(imported.iter().cloned());
        }
    }
    imports
}

/// Record the start of `expression`, and of each name in it outside a
/// select, as an assignment target, for a concatenation of members.
fn written_expression(expression: ExprId, file: &File<'_>, writes: &mut HashSet<usize>) {
    let native = file.syntax_tree.declarations();
    let mut pending = vec![expression];
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let expression = native.expression(id);
        writes.extend(file.range_span(expression.tokens).map(|span| span.0));
        match &expression.kind {
            native::ExprKind::Select { expression, .. }
            | native::ExprKind::Member { expression, .. }
            | native::ExprKind::Group(expression) => pending.push(*expression),
            native::ExprKind::Concatenation(items) => pending.extend(items),
            _ => {}
        }
    }
}
fn readmem_destination(call: &native::Expr, file: &File<'_>) -> Converted<Option<ExprId>> {
    let native::ExprKind::Call {
        expression,
        arguments,
    } = &call.kind
    else {
        return Ok(None);
    };
    let Some(name) = expression_name(*expression, file.syntax_tree.declarations()) else {
        return Ok(None);
    };
    if !matches!(SystemTask::from_name(&name), Some(SystemTask::Readmem(_))) {
        return Ok(None);
    }
    Ok(arguments.get(1).and_then(|argument| argument.value))
}

/// Whether `code` has a backtick outside comments and string literals, for
/// a macro or compiler directive.
fn has_directive(code: &str) -> bool {
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => return true,
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            _ => {}
        }
    }
    false
}

/// The compilation-unit package imports of `file` and what they import.
fn unit_imports(
    file: &File<'_>,
) -> Result<Vec<(Span, Vec<(String, Option<String>)>)>, AnalyzerError> {
    let mut imports = Vec::new();
    for view in package_owners::compilation(&file.syntax_tree)? {
        if file
            .syntax_tree
            .declarations()
            .package_binding(view.id)
            .kind
            == celox_sv_syntax::PackageBindingKind::Import
        {
            imports.push((
                file.binding_span(&view)?,
                imported_items(view.id, &file.syntax_tree)?,
            ));
        }
    }
    Ok(imports)
}

/// What the package imports under `root` import: `(package, item)`, with
/// no item for a wildcard import.
fn imported_items(
    id: celox_sv_syntax::PackageBindingId,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<(String, Option<String>)>, AnalyzerError> {
    use celox_sv_syntax::{PackageBindingItemKind, PackageBindingKind};
    let binding = syntax_tree.declarations().package_binding(id);
    if binding.kind != PackageBindingKind::Import {
        return Err(AnalyzerError::Parse(
            "native dependency import has a different declaration kind".into(),
        ));
    }
    Ok(binding
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            PackageBindingItemKind::Named { package, name } => Some((
                normalize_identifier(&package.spelling),
                Some(normalize_identifier(&name.spelling)),
            )),
            PackageBindingItemKind::Wildcard(package) => {
                Some((normalize_identifier(&package.spelling), None))
            }
            PackageBindingItemKind::All => None,
        })
        .collect())
}

fn unit_names(file: &File<'_>) -> Converted<HashSet<String>> {
    imports::scope_names(&file.syntax_tree.compilation_scope())
}

/// The scope that declares a subroutine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    Module(String),
    Interface(String),
    Package(String),
    /// A class or program, whose subroutines a module cannot call by a
    /// plain name.
    Other,
    /// The compilation unit.
    Unit,
}

impl Scope {
    /// The design elements and classes of `file` with their spans.
    fn collect(file: &File<'_>) -> Converted<Vec<(Span, Self)>> {
        let mut scopes = Vec::new();
        let native = file.syntax_tree.declarations();
        for (_, unit) in roots(&file.syntax_tree)? {
            let name = native_name(&unit.name);
            let scope = match unit.kind {
                native::UnitKind::Module => Self::Module(name),
                native::UnitKind::Interface => Self::Interface(name),
                native::UnitKind::Package => Self::Package(name),
                _ => Self::Other,
            };
            scopes.push((file.span(unit.tokens)?, scope));
        }
        for item in &native.body_items {
            if let native::BodyItemKind::Unconverted { category, .. } = &item.kind
                && category == "class"
            {
                scopes.push((file.span(item.tokens)?, Self::Other));
            }
        }
        Ok(scopes)
    }

    /// The innermost of `scopes` containing `span`.
    fn of(span: Span, scopes: &[(Span, Self)]) -> Self {
        scopes
            .iter()
            .filter(|(outer, _)| outer.0 <= span.0 && span.1 <= outer.1)
            .min_by_key(|(outer, _)| outer.1 - outer.0)
            .map_or(Self::Unit, |(_, scope)| scope.clone())
    }
}

/// Where an access in a module body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Module,
    /// Inside a generate construct, whose conditions and loop bounds are
    /// not evaluated here.
    Generate,
    /// Inside a function or task, which may never be called.
    Subroutine,
    /// Connected to a port of a module whose port directions are not known
    /// here.
    UnknownPort,
}

impl Context {
    fn of(span: Span, generates: &[Span], subroutines: &[Span]) -> Self {
        let inside = |spans: &[Span]| {
            spans
                .iter()
                .any(|outer| outer.0 <= span.0 && span.1 <= outer.1)
        };
        if inside(subroutines) {
            Self::Subroutine
        } else if inside(generates) {
            Self::Generate
        } else {
            Self::Module
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Module => "in the body",
            Self::Generate => "inside a generate construct",
            Self::Subroutine => "inside a function or task",
            Self::UnknownPort => "through a port of a module that is not analyzed here",
        }
    }
}

/// The ports of a subroutine in order: the name, unless omitted, and whether
/// the subroutine may write the actual argument (`output`, `inout`, `ref`).
type SubroutinePorts = Vec<(Option<String>, bool)>;

fn subroutine_ports(subroutine: &native::Subroutine) -> SubroutinePorts {
    let mut inherited = false;
    subroutine
        .ports
        .iter()
        .flatten()
        .chain(
            subroutine
                .port_declarations
                .iter()
                .flat_map(|group| &group.ports),
        )
        .map(|port| {
            if let Some(direction) = port.direction {
                inherited = direction != native::Direction::Input
                    && !(direction == native::Direction::Ref && port.constant);
            }
            (port.name.as_ref().map(native_name), inherited)
        })
        .collect()
}

#[derive(Default)]
struct ModuleUses {
    /// `(handle, member, context)` assignment targets, including actual
    /// arguments of subroutine ports that write them.
    writes: Vec<(String, String, Context)>,
    /// `(handle, item, context)` references, including function calls.
    references: Vec<(String, String, Context)>,
    children: Vec<ChildBinding>,
}

/// A reference to an interface handle found in the tokens.
struct TokenReference {
    handle: String,
    /// The span of the element selects between the handle and the item.
    selects: Option<Span>,
    /// The item after the handle and its selects, with its span.
    item: Option<(String, Span)>,
    /// The span of the handle name.
    span: Span,
}

/// Whether a primary has a class qualifier or package scope. The parser
/// gives a plain name an empty class qualifier.
/// The interface handle an actual names, optionally selected and qualified by a modport.
fn actual_handle(expression: ExprId, file: &File<'_>) -> Converted<Option<Actual>> {
    let native = file.syntax_tree.declarations();
    let mut base = expression;
    let mut modport = None;
    if let native::ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(base).kind
    {
        if separator != "." {
            return Ok(None);
        }
        modport = Some(native_name(name));
        base = *expression;
    }
    let end = native.expression(base).tokens.end;
    let mut selected = false;
    while let native::ExprKind::Select { expression, .. } = &native.expression(base).kind {
        selected = true;
        base = *expression;
    }
    let native::ExprKind::Name(name) = &native.expression(base).kind else {
        return Ok(None);
    };
    let start = native.expression(base).tokens.end;
    let selects = if selected {
        file.range_span(native::TokenRange { start, end })
    } else {
        None
    };
    Ok(Some(Actual {
        handle: native_name(name),
        selects,
        modport,
    }))
}
#[derive(Clone, Copy)]
enum Connection {
    Explicit(Option<ExprId>),
    Implicit(Span),
}
impl Connection {
    /// Resolve `.p` and `.p(p)` through the same lexical handle lookup.
    fn interface_actual(self, formal: &str, file: &File<'_>) -> Converted<Option<(Actual, Span)>> {
        match self {
            Self::Implicit(span) => Ok(Some((
                Actual {
                    handle: formal.to_string(),
                    selects: None,
                    modport: None,
                },
                span,
            ))),
            Self::Explicit(Some(expression)) => actual_handle(expression, file)?
                .map(|actual| file.expression_span(expression).map(|span| (actual, span)))
                .transpose(),
            Self::Explicit(None) => Ok(None),
        }
    }
}
fn connections(
    instance: &native::InstanceDeclarator,
    module: &ModuleDecl,
    file: &File<'_>,
    strict: bool,
) -> Converted<Vec<(String, Connection)>> {
    use native::{ConnectionValue, InstanceConnectionKind};
    if instance.connections.is_empty() {
        return Ok(Vec::new());
    }
    if instance
        .connections
        .iter()
        .all(|connection| matches!(connection.kind, InstanceConnectionKind::Ordered(_)))
    {
        if instance.connections.len() == 1
            && matches!(
                instance.connections[0].kind,
                InstanceConnectionKind::Ordered(None)
            )
            && module.ports.is_empty()
        {
            return Ok(Vec::new());
        }
        if instance.connections.len() > module.ports.len() {
            return Err(unsupported(format!(
                "more port connections than ports of `{}`",
                module.name
            )));
        }
        return Ok(module
            .ports
            .iter()
            .enumerate()
            .map(|(index, port)| {
                let actual =
                    instance
                        .connections
                        .get(index)
                        .and_then(|connection| match connection.kind {
                            InstanceConnectionKind::Ordered(value) => value,
                            _ => None,
                        });
                (port.name.clone(), Connection::Explicit(actual))
            })
            .collect());
    }
    let mut connections = Vec::new();
    for connection in &instance.connections {
        match &connection.kind {
            InstanceConnectionKind::Named { name, value } => {
                let formal = native_name(name);
                let actual = match value {
                    ConnectionValue::Expression(id) => Connection::Explicit(Some(*id)),
                    ConnectionValue::Empty => Connection::Explicit(None),
                    ConnectionValue::Implicit if !strict => continue,
                    ConnectionValue::Implicit => Connection::Implicit(file.name_span(name)),
                };
                connections.push((formal, actual));
            }
            InstanceConnectionKind::Wildcard if !strict => {}
            InstanceConnectionKind::Wildcard => {
                return Err(unsupported("wildcard port connection"));
            }
            _ => return Err(unsupported("mixed named and positional port connections")),
        }
    }
    if strict {
        for port in &module.ports {
            if port.interface.is_some()
                && !connections.iter().any(|(formal, _)| formal == &port.name)
            {
                connections.push((port.name.clone(), Connection::Explicit(None)));
            }
        }
    }
    Ok(connections)
}
fn parameter_overrides(
    arguments: Option<&[native::Argument]>,
    parameters: &[String],
    file: &File<'_>,
) -> Converted<Vec<(String, Span)>> {
    let Some(arguments) = arguments else {
        return Ok(Vec::new());
    };
    if arguments.iter().all(|argument| argument.name.is_none())
        && arguments.len() > parameters.len()
    {
        return Err(unsupported("more parameter values than parameters"));
    }
    arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            let expression = argument.value?;
            let parameter = argument
                .name
                .as_ref()
                .map(native_name)
                .or_else(|| parameters.get(index).cloned())?;
            Some(
                file.expression_span(expression)
                    .map(|span| (parameter, span)),
            )
        })
        .collect()
}
fn check_no_connections(instance: &native::InstanceDeclarator, interface: &str) -> Converted<()> {
    if instance.connections.is_empty()
        || matches!(
            instance.connections.as_slice(),
            [native::InstanceConnection {
                kind: native::InstanceConnectionKind::Ordered(None),
                ..
            }]
        )
    {
        Ok(())
    } else {
        Err(unsupported(format!("ports of interface `{interface}`")))
    }
}

impl InterfaceDecl {
    fn collect(
        handle: ScopeHandle,
        file_index: usize,
        file: &File<'_>,
        interfaces: &HashSet<String>,
    ) -> Converted<Self> {
        let tree = file.syntax_tree.scoped(handle)?;
        let unit = tree.root_unit()?;
        let native = tree.declarations();
        let interface_name = native_name(&unit.name);
        if !unit.ports.is_empty() {
            return Err(unsupported(format!(
                "ports of interface `{interface_name}`"
            )));
        }
        if native.types.iter().any(|ty| {
            unit.tokens.start <= ty.tokens.start
                && ty.tokens.end <= unit.tokens.end
                && matches!(ty.kind, native::DataTypeKind::Enum { .. })
        }) {
            return Err(unsupported(format!(
                "enum type in interface `{interface_name}`"
            )));
        }
        let span = file.span(unit.tokens)?;
        let mut interface = Self {
            scope: handle,
            name: interface_name.clone(),
            file: file_index,
            start: span.0,
            has_parameter_port_list: unit.parameters.is_some(),
            uses_unit_items: false,
            uses_macros: has_directive(file.text(span)),
            parameters: Vec::new(),
            type_parameters: Vec::new(),
            parameter_order: Vec::new(),
            items: Vec::new(),
            members: Vec::new(),
            functions: Vec::new(),
            modports: HashMap::default(),
            names: HashSet::default(),
            references: Vec::new(),
            lvalues: HashSet::default(),
        };
        for id in &unit.package_bindings {
            let binding = native.package_binding(*id);
            if unit.header.start <= binding.tokens.start && binding.tokens.end <= unit.header.end {
                interface.items.push(Item::Constant {
                    span: file.span(binding.tokens)?,
                    header: HeaderForm::Import {
                        imports: imported_items(*id, &tree)?,
                    },
                });
            }
        }
        for parameter in unit.parameters.iter().flatten() {
            interface.collect_parameter(parameter, true, file)?;
        }
        for id in &unit.items {
            interface.collect_item(*id, file, interfaces)?;
        }
        for (modport, items) in &interface.modports {
            for item in items {
                let (name, expected) = match item {
                    ModportItem::Member(_, member) if interface.member(member).is_none() => {
                        (member, "a member")
                    }
                    ModportItem::Import(function) if interface.function(function).is_none() => {
                        (function, "a function")
                    }
                    _ => continue,
                };
                return Err(AnalyzerError::UnknownModportItem {
                    interface: interface_name,
                    modport: modport.clone(),
                    name: name.clone(),
                    expected,
                });
            }
        }
        let mut declared: HashMap<String, usize> = HashMap::default();
        for name in declared_names(unit, file)? {
            *declared.entry(name).or_default() += 1;
        }
        for (name, count) in &declared {
            if *count > 1 && interface.names.contains(name) {
                return Err(unsupported(format!(
                    "declaration of `{name}` in a nested scope of interface `{interface_name}`, which shadows an interface item"
                )));
            }
        }
        for id in body_items(unit, native)? {
            if let native::BodyItemKind::Block {
                leading_label,
                label,
                ..
            } = &native.body_item(id).kind
            {
                if let Some(name) = label.as_ref().or(leading_label.as_ref()) {
                    interface.names.insert(native_name(name));
                }
            }
        }
        let qualified: HashSet<_> = native
            .expressions
            .iter()
            .filter_map(|expression| match &expression.kind {
                native::ExprKind::Member {
                    expression,
                    separator,
                    ..
                } if separator == "::" => Some(native.expression(*expression).tokens.start),
                _ => None,
            })
            .collect();
        for expression in &native.expressions {
            if expression.tokens.start < unit.tokens.start
                || expression.tokens.end > unit.tokens.end
            {
                continue;
            }
            if let native::ExprKind::Name(name) = &expression.kind {
                let name_text = native_name(name);
                if name_text.strip_prefix("genblk").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                }) && !declared.contains_key(&name_text)
                {
                    return Err(unsupported(format!(
                        "reference to the implicit generate block name `{name_text}` in interface `{interface_name}`"
                    )));
                }
                if interface.names.contains(&name_text)
                    && !qualified.contains(&expression.tokens.start)
                {
                    interface.references.push((file.name_span(name), name_text));
                }
            }
            if let native::ExprKind::Unary {
                operator,
                expression,
                ..
            } = &expression.kind
                && matches!(operator.as_str(), "++" | "--")
            {
                written_expression(*expression, file, &mut interface.lvalues);
            }
            if let Some(destination) = readmem_destination(expression, file)? {
                written_expression(destination, file, &mut interface.lvalues);
            }
        }
        for ty in &native.types {
            if unit.tokens.start <= ty.tokens.start
                && ty.tokens.end <= unit.tokens.end
                && let native::DataTypeKind::Named(path) = &ty.kind
                && let [segment] = path.as_slice()
            {
                let name = native_name(&segment.name);
                if interface.names.contains(&name) {
                    interface
                        .references
                        .push((file.name_span(&segment.name), name));
                }
            }
        }
        for statement in &native.statements {
            if unit.tokens.start <= statement.tokens.start
                && statement.tokens.end <= unit.tokens.end
                && let native::StatementKind::Assignment { left, .. } = statement.kind
            {
                written_expression(left, file, &mut interface.lvalues);
            }
        }
        for assignment in &native.assign_statements {
            if unit.tokens.start <= assignment.tokens.start
                && assignment.tokens.end <= unit.tokens.end
            {
                for assignment in &assignment.assignments {
                    written_expression(assignment.left, file, &mut interface.lvalues);
                    let mut base = assignment.left;
                    while let native::ExprKind::Select { expression, .. } =
                        native.expression(base).kind
                    {
                        base = expression;
                    }
                    if !interface.uses_macros
                        && let native::ExprKind::Name(name) = &native.expression(base).kind
                    {
                        let name = native_name(name);
                        if !interface.names.contains(&name) && !declared.contains_key(&name) {
                            return Err(unsupported(format!(
                                "implicit net `{name}` in interface `{interface_name}`"
                            )));
                        }
                    }
                }
            }
        }
        Ok(interface)
    }
    fn collect_parameter(
        &mut self,
        parameter: &native::Parameter,
        header: bool,
        file: &File<'_>,
    ) -> Converted<()> {
        let local = parameter.local || (!header && self.has_parameter_port_list);
        let native::ParameterKind::Value {
            data_type,
            dimensions,
            default,
        } = &parameter.kind
        else {
            let name = native_name(&parameter.name);
            self.declare(name.clone())?;
            if !local {
                self.parameter_order.push(name.clone());
                self.type_parameters.push(name);
                return Ok(());
            }
            self.items.push(Item::Constant {
                span: file.span(parameter.tokens)?,
                header: HeaderForm::Constant,
            });
            return Ok(());
        };
        if local {
            self.declare(native_name(&parameter.name))?;
            self.items.push(Item::Constant {
                span: file.span(parameter.tokens)?,
                header: HeaderForm::Constant,
            });
            return Ok(());
        }
        if !dimensions.is_empty() {
            return Err(unsupported(format!(
                "unpacked array parameter of interface `{}`",
                self.name
            )));
        }
        let name = native_name(&parameter.name);
        self.declare(name.clone())?;
        self.parameter_order.push(name.clone());
        self.items.push(Item::Parameter(self.parameters.len()));
        self.parameters.push(InterfaceParameter {
            name,
            data_type: file
                .range_span(file.syntax_tree.declarations().data_type(*data_type).tokens),
            default: default.map(|id| file.expression_span(id)).transpose()?,
        });
        Ok(())
    }
    fn collect_item(
        &mut self,
        id: BodyId,
        file: &File<'_>,
        interfaces: &HashSet<String>,
    ) -> Converted<()> {
        let native = file.syntax_tree.declarations();
        let item = native.body_item(id);
        let span = file.span(item.tokens)?;
        match &item.kind {
            native::BodyItemKind::Modport(id) => {
                self.collect_modports(native.modport_declaration(*id))?
            }
            native::BodyItemKind::Time(_) => {}
            native::BodyItemKind::Parameter(id) => {
                for parameter in &native.parameter_declaration(*id).parameters {
                    self.collect_parameter(parameter, false, file)?;
                }
            }
            native::BodyItemKind::TypeAlias(id) => {
                let alias = native.type_alias(*id);
                if !matches!(alias.target, native::TypeAliasTarget::DataType(_)) {
                    return Err(unsupported(format!(
                        "forward type declaration in interface `{}`",
                        self.name
                    )));
                }
                if !alias.dimensions.is_empty() {
                    return Err(unsupported(format!(
                        "unpacked typedef in interface `{}`",
                        self.name
                    )));
                }
                self.declare(native_name(&alias.name))?;
                self.items.push(Item::Constant {
                    span,
                    header: HeaderForm::Constant,
                });
            }
            native::BodyItemKind::PackageBinding(id) => {
                self.items.push(Item::Constant {
                    span,
                    header: HeaderForm::Import {
                        imports: imported_items(*id, &file.syntax_tree)?,
                    },
                });
            }
            native::BodyItemKind::Subroutine(id) => {
                let subroutine = native.subroutine(*id);
                if subroutine.prototype {
                    return Err(unsupported(format!(
                        "extern subroutine in interface `{}`",
                        self.name
                    )));
                }
                if subroutine.kind != native::SubroutineKind::Function {
                    return Err(unsupported(format!(
                        "task, class, or verification declaration in interface `{}`",
                        self.name
                    )));
                }
                let name = native_name(&subroutine.name);
                self.declare(name.clone())?;
                self.functions.push(Function { name, span });
                self.items.push(Item::Function);
            }
            native::BodyItemKind::Data(id) => {
                let native::DataDeclaration::Variables(declaration) = native.data_declaration(*id)
                else {
                    return Err(unsupported(format!(
                        "dynamic array or class variable in interface `{}`",
                        self.name
                    )));
                };
                if declaration.constant {
                    return Err(unsupported(format!(
                        "const variable in interface `{}`",
                        self.name
                    )));
                }
                let data_type = file.range_span(native.data_type(declaration.data_type).tokens);
                for variable in &declaration.variables {
                    self.add_member(Member {
                        name: native_name(&variable.name),
                        data_type,
                        dimensions: variable
                            .dimensions
                            .iter()
                            .map(|dimension| file.span(dimension.tokens))
                            .collect::<Converted<_>>()?,
                        initializer: variable
                            .value
                            .map(|id| file.expression_span(id))
                            .transpose()?,
                    })?;
                }
            }
            native::BodyItemKind::Net(id) => {
                let declaration = native.net_declaration(*id);
                if declaration.net_type == "interconnect" {
                    return Err(unsupported(format!(
                        "interconnect in interface `{}`",
                        self.name
                    )));
                }
                if declaration.strength.is_some()
                    || declaration.vector.is_some()
                    || declaration.delay.is_some()
                {
                    return Err(unsupported(format!(
                        "net strength, vectored, or delay in interface `{}`",
                        self.name
                    )));
                }
                let data_type = file.range_span(native.data_type(declaration.data_type).tokens);
                for variable in &declaration.variables {
                    if variable.value.is_some() {
                        return Err(unsupported(format!(
                            "net declaration assignment in interface `{}`",
                            self.name
                        )));
                    }
                    self.add_member(Member {
                        name: native_name(&variable.name),
                        data_type,
                        dimensions: variable
                            .dimensions
                            .iter()
                            .map(|dimension| file.span(dimension.tokens))
                            .collect::<Converted<_>>()?,
                        initializer: None,
                    })?;
                }
            }
            native::BodyItemKind::Instance(id) => {
                let declaration = native.instantiation(*id);
                if interfaces.contains(&native_name(&declaration.name)) {
                    for instance in &declaration.instances {
                        self.declare(native_name(&instance.name))?;
                    }
                    self.items.push(Item::Process(span));
                    return Ok(());
                }
                if declaration.parameters.is_some()
                    || declaration
                        .instances
                        .iter()
                        .any(|instance| !instance.connections.is_empty())
                {
                    return Err(unsupported(format!(
                        "instantiation, assertion, or alias in interface `{}`",
                        self.name
                    )));
                }
                let data_type = Some(file.name_span(&declaration.name));
                for variable in &declaration.instances {
                    self.add_member(Member {
                        name: native_name(&variable.name),
                        data_type,
                        dimensions: variable
                            .dimensions
                            .iter()
                            .map(|dimension| file.span(dimension.tokens))
                            .collect::<Converted<_>>()?,
                        initializer: None,
                    })?;
                }
            }
            native::BodyItemKind::Assign(_)
            | native::BodyItemKind::Procedure(_)
            | native::BodyItemKind::Region(_)
            | native::BodyItemKind::Block { .. }
            | native::BodyItemKind::Conditional { .. }
            | native::BodyItemKind::Case { .. }
            | native::BodyItemKind::Loop { .. } => self.items.push(Item::Process(span)),
            native::BodyItemKind::Unconverted { category, .. } => match category.as_str() {
                ";" => {}
                "genvar" => {
                    for token in &native.tokens[item.tokens.start..item.tokens.end] {
                        if token.text != "genvar"
                            && let Some(name) = token.identifier()
                        {
                            self.names.insert(normalize_identifier(name));
                        }
                    }
                    self.items.push(Item::Process(span));
                }
                "$fatal" | "$error" | "$warning" | "$info" => self.items.push(Item::Process(span)),
                "interconnect" => {
                    return Err(unsupported(format!(
                        "interconnect in interface `{}`",
                        self.name
                    )));
                }
                "nettype" => {
                    return Err(unsupported(format!(
                        "nettype declaration in interface `{}`",
                        self.name
                    )));
                }
                "clocking" | "default" => {
                    return Err(unsupported(format!(
                        "clocking or default disable declaration in interface `{}`",
                        self.name
                    )));
                }
                "bind" | "alias" | "assert" | "assume" | "cover" | "restrict" => {
                    return Err(unsupported(format!(
                        "instantiation, assertion, or alias in interface `{}`",
                        self.name
                    )));
                }
                _ => {
                    return Err(unsupported(format!(
                        "task, class, or verification declaration in interface `{}`",
                        self.name
                    )));
                }
            },
            _ => {
                return Err(unsupported(format!(
                    "nested declaration in interface `{}`",
                    self.name
                )));
            }
        }
        Ok(())
    }

    fn add_member(&mut self, member: Member) -> Result<(), AnalyzerError> {
        self.declare(member.name.clone())?;
        self.members.push(member);
        self.items.push(Item::Member(self.members.len() - 1));
        Ok(())
    }

    /// Declare `name` in the interface scope.
    fn declare(&mut self, name: String) -> Result<(), AnalyzerError> {
        if !self.names.insert(name.clone()) {
            return Err(AnalyzerError::DuplicateInterfaceItem {
                interface: self.name.clone(),
                name,
            });
        }
        Ok(())
    }

    fn collect_modports(&mut self, declaration: &native::ModportDeclaration) -> Converted<()> {
        for modport in &declaration.modports {
            let modport_name = native_name(&modport.name);
            let mut items = Vec::new();
            for group in &modport.ports {
                match group {
                    native::ModportPorts::Members { direction, ports } => {
                        let direction = match direction {
                            native::Direction::Input => Direction::Input,
                            native::Direction::Output => Direction::Output,
                            _ => {
                                return Err(unsupported(format!(
                                    "inout or ref modport port in interface `{}`",
                                    self.name
                                )));
                            }
                        };
                        for port in ports {
                            let native::ModportMember::Named(name) = port else {
                                return Err(unsupported(format!(
                                    "modport expression in interface `{}`",
                                    self.name
                                )));
                            };
                            items.push(ModportItem::Member(direction, native_name(name)));
                        }
                    }
                    native::ModportPorts::Subroutines { export, ports } => {
                        if *export {
                            return Err(unsupported(format!(
                                "modport export in interface `{}`",
                                self.name
                            )));
                        }
                        for port in ports {
                            let native::ModportSubroutine::Named(name) = port else {
                                return Err(unsupported(format!(
                                    "modport import with a prototype in interface `{}`",
                                    self.name
                                )));
                            };
                            items.push(ModportItem::Import(native_name(name)));
                        }
                    }
                    native::ModportPorts::Clocking(_) => {
                        return Err(unsupported(format!(
                            "modport clocking in interface `{}`",
                            self.name
                        )));
                    }
                }
            }
            let mut listed = HashSet::default();
            for item in &items {
                let (ModportItem::Member(_, name) | ModportItem::Import(name)) = item;
                if !listed.insert(name) {
                    return Err(AnalyzerError::DuplicateModportItem {
                        interface: self.name.clone(),
                        modport: modport_name,
                        name: name.clone(),
                    });
                }
            }
            if self.modports.insert(modport_name.clone(), items).is_some() {
                return Err(AnalyzerError::DuplicateModport {
                    interface: self.name.clone(),
                    name: modport_name,
                });
            }
        }
        Ok(())
    }
}

impl ModuleDecl {
    fn collect(
        handle: ScopeHandle,
        file_index: usize,
        file: &File<'_>,
        interfaces: &HashMap<String, InterfaceDecl>,
    ) -> Converted<Self> {
        let tree = file.syntax_tree.scoped(handle)?;
        let unit = tree.root_unit()?;
        let native = tree.declarations();
        let mut ports = Vec::new();
        let mut writes = true;
        for port in &unit.ports {
            if let Some(direction) = port.direction {
                writes = direction != native::Direction::Input;
            }
            let name = match &port.kind {
                native::PortKind::Named(name) | native::PortKind::Explicit { name, .. } => {
                    native_name(name)
                }
                _ => return Err(unsupported("non-ANSI module port")),
            };
            let interface = if matches!(port.kind, native::PortKind::Explicit { .. })
                || port.direction.is_some()
                || port.net_type.is_some()
                || port.variable
            {
                None
            } else {
                port.data_type
                    .and_then(|id| match &native.data_type(id).kind {
                        native::DataTypeKind::Interface { name, modport } => Some((
                            name.as_ref().map(native_name),
                            modport.as_ref().map(native_name),
                        )),
                        native::DataTypeKind::Named(path)
                            if path.len() == 1
                                && interfaces.contains_key(&native_name(&path[0].name)) =>
                        {
                            Some((Some(native_name(&path[0].name)), None))
                        }
                        _ => None,
                    })
            };
            let interface = interface
                .map(|(interface, modport)| -> Converted<InterfacePort> {
                    if port.default.is_some() {
                        return Err(unsupported("default value of an interface port"));
                    }
                    Ok(InterfacePort {
                        interface,
                        modport,
                        dimensions: port
                            .dimensions
                            .iter()
                            .map(|dimension| file.span(dimension.tokens))
                            .collect::<Converted<_>>()?,
                    })
                })
                .transpose()?;
            ports.push(PortDecl {
                name,
                interface,
                writes,
            });
        }
        Ok(Self {
            scope: handle,
            name: native_name(&unit.name),
            file: file_index,
            span: file.span(unit.tokens)?,
            ports,
        })
    }
}

/// Declaration bindings and port directions, independent of rewritten source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeInterfacePlan {
    pub root_names: HashMap<String, String>,
    pub modules: HashMap<String, NativeModulePlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeModulePlan {
    pub original: String,
    pub ports: Vec<NativePortPlan>,
    /// Child instance identifier offsets and their bound module names.
    pub children: HashMap<(BodyId, usize), String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePortPlan {
    pub array_rank: usize,
    pub functions: Vec<String>,
    pub all_members: Vec<String>,
    pub name: String,
    pub interface: String,
    pub members: Vec<(String, PortDirection)>,
    pub parameters: Vec<String>,
    pub type_parameters: Vec<String>,
}

pub fn plan_interfaces(
    sources: &[(&str, &Path)],
    bindings: &[(String, String, Vec<(String, String)>)],
) -> Converted<NativeInterfacePlan> {
    let files = sources
        .iter()
        .map(|(code, path)| File::new(code, path))
        .collect::<Converted<Vec<_>>>()?;
    let design = Design::collect(&files)?;
    let mut result = NativeInterfacePlan::default();
    if design.interfaces.is_empty() {
        return Ok(result);
    }
    let mut queue = design
        .modules
        .iter()
        .filter(|(_, module)| {
            !module.ports.iter().any(|port| {
                port.interface
                    .as_ref()
                    .is_some_and(|interface| interface.interface.is_none())
            })
        })
        .map(|(name, _)| (name.clone(), Vec::<(String, String)>::new()))
        .collect::<Vec<_>>();
    for (alias, name, supplied) in bindings {
        let Some(module) = design.modules.get(name) else {
            continue;
        };
        let mut generic = Vec::new();
        for (port, interface) in supplied {
            let declared = module
                .port(port)
                .and_then(|port| port.interface.as_ref())
                .ok_or_else(|| {
                    unsupported(format!("`{port}` of `{name}` is not an interface port"))
                })?;
            if let Some(expected) = &declared.interface {
                if expected != interface {
                    return Err(unsupported(format!(
                        "interface `{interface}` connected to `{name}.{port}` of type `{expected}`"
                    )));
                }
            } else {
                generic.push((port.clone(), interface.clone()));
            }
        }
        generic.sort();
        let bound = if generic.is_empty() {
            name.clone()
        } else {
            clone_module_name(name, &generic)
        };
        result.root_names.insert(alias.clone(), bound);
        queue.push((name.clone(), generic));
    }
    while let Some((name, bindings)) = queue.pop() {
        let bound_name = if bindings.is_empty() {
            name.clone()
        } else {
            clone_module_name(&name, &bindings)
        };
        if result.modules.contains_key(&bound_name) {
            continue;
        }
        let module = &design.modules[&name];
        let file = &files[module.file];
        let declaration = design.module_unit(module)?;
        let native = file.syntax_tree.declarations();
        let items = body_items(declaration, native)?;
        let mut handles = Handles::default();
        let mut ports = Vec::new();
        for port in &module.ports {
            let Some(declared) = &port.interface else {
                continue;
            };
            let interface = declared
                .interface
                .as_ref()
                .or_else(|| {
                    bindings
                        .iter()
                        .find(|(name, _)| name == &port.name)
                        .map(|(_, interface)| interface)
                })
                .ok_or_else(|| {
                    unsupported(format!(
                        "generic interface port `{}` of module `{name}` without a binding",
                        port.name
                    ))
                })?;
            let expansion = design.expansion(&name, &port.name, interface, &bindings)?;
            let uses = design.module_uses(&name, (&port.name, interface), &bindings)?;
            let declaration = &design.interfaces[interface];
            for (handle, member, _) in &uses.references {
                if handle != &port.name {
                    continue;
                }
                if declaration.member(member).is_some()
                    && expansion
                        .accessible
                        .as_ref()
                        .is_some_and(|members| !members.contains(member))
                {
                    return Err(unsupported(format!(
                        "access of `{handle}.{member}`, which the modport of port `{handle}` does not list"
                    )));
                }
                if declaration.function(member).is_some()
                    && expansion
                        .callable
                        .as_ref()
                        .is_some_and(|functions| !functions.contains(member))
                {
                    return Err(unsupported(format!(
                        "call of `{handle}.{member}`, which the modport of port `{handle}` does not import"
                    )));
                }
            }
            handles.insert(
                port.name.clone(),
                module.span,
                Handle {
                    interface: interface.clone(),
                    port: Some(expansion.clone()),
                    arrayed: !declared.dimensions.is_empty(),
                },
            );
            ports.push(NativePortPlan {
                array_rank: declared.dimensions.len(),
                functions: expansion.functions.clone(),
                all_members: design.interfaces[interface]
                    .members
                    .iter()
                    .map(|member| member.name.clone())
                    .collect(),
                name: port.name.clone(),
                interface: interface.clone(),
                members: expansion
                    .members
                    .iter()
                    .map(|(name, direction)| {
                        (
                            name.clone(),
                            match direction {
                                Direction::Input => PortDirection::Input,
                                Direction::Output => PortDirection::Output,
                            },
                        )
                    })
                    .collect(),
                type_parameters: design.interfaces[interface].type_parameters.clone(),
                parameters: design.interfaces[interface]
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect(),
            });
        }
        let blocks = block_spans(declaration, file)?;
        for id in &items {
            let native::BodyItemKind::Instance(instance) = native.body_item(*id).kind else {
                continue;
            };
            let instantiation = native.instantiation(instance);
            let interface = native_name(&instantiation.name);
            if !design.interfaces.contains_key(&interface) {
                continue;
            }
            let span = file.span(instantiation.tokens)?;
            let parameter_names = &design.interfaces[&interface].parameter_order;
            let mut supplied = HashSet::default();
            for (name, _) in
                parameter_overrides(instantiation.parameters.as_deref(), parameter_names, file)?
            {
                if !parameter_names.contains(&name) {
                    return Err(AnalyzerError::UnknownInterfaceParameter {
                        interface: interface.clone(),
                        name,
                    });
                }
                if !supplied.insert(name.clone()) {
                    return Err(AnalyzerError::DuplicateInterfaceParameterOverride {
                        interface: interface.clone(),
                        name,
                    });
                }
            }

            let scope = blocks
                .iter()
                .filter(|block| block.0 <= span.0 && span.1 <= block.1)
                .min_by_key(|block| block.1 - block.0)
                .copied()
                .unwrap_or(module.span);
            for instance in &instantiation.instances {
                check_no_connections(instance, &interface)?;
                let handle = native_name(&instance.name);
                handles.insert(
                    handle,
                    scope,
                    Handle {
                        interface: interface.clone(),
                        port: None,
                        arrayed: !instance.dimensions.is_empty(),
                    },
                );
            }
        }
        for reference in
            file.handle_references(module.span, |name| handles.by_name.contains_key(name))
        {
            let handle = handles
                .at(&reference.handle, reference.span)
                .ok_or_else(|| {
                    unsupported(format!(
                        "reference to interface `{}` outside the generate block that declares it",
                        reference.handle
                    ))
                })?;
            if let Some((member, _)) = reference.item
                && design.interfaces[&handle.interface]
                    .function(&member)
                    .is_some()
                && handle.arrayed
                && reference.selects.is_none()
            {
                return Err(unsupported(format!(
                    "call of a function of the unselected interface array `{}`",
                    reference.handle
                )));
            }
        }
        let mut children = HashMap::default();
        for id in &items {
            let native::BodyItemKind::Instance(instance) = native.body_item(*id).kind else {
                continue;
            };
            let instantiation = native.instantiation(instance);
            let child_name = native_name(&instantiation.name);
            let Some(child) = design.modules.get(&child_name) else {
                continue;
            };
            if !child.has_interface_ports() {
                continue;
            }
            for (index, instance) in instantiation.instances.iter().enumerate() {
                let mut generic = Vec::new();
                for (formal, actual) in connections(instance, child, file, true)? {
                    let Some(port) = child.port(&formal).and_then(|port| port.interface.as_ref())
                    else {
                        continue;
                    };
                    let (actual, span) =
                        actual.interface_actual(&formal, file)?.ok_or_else(|| {
                            unsupported(format!(
                                "missing interface actual of `{child_name}.{formal}`"
                            ))
                        })?;
                    let handle = handles.at(&actual.handle, span).ok_or_else(|| {
                        unsupported(format!("unknown interface handle `{}`", actual.handle))
                    })?;
                    if handle.arrayed && actual.selects.is_none() && port.dimensions.is_empty() {
                        return Err(unsupported(format!(
                            "whole interface array `{}` connected to scalar port `{formal}`",
                            actual.handle
                        )));
                    }
                    if let Some(expected) = &port.interface {
                        if expected != &handle.interface {
                            return Err(unsupported(format!(
                                "interface type mismatch at `{child_name}.{formal}`"
                            )));
                        }
                    } else {
                        generic.push((formal.clone(), handle.interface.clone()));
                    }
                    let child_bindings = child
                        .ports
                        .iter()
                        .filter_map(|formal| {
                            let declared = formal.interface.as_ref()?;
                            if declared.interface.is_some() {
                                return None;
                            }
                            let actual = connections(instance, child, file, true)
                                .ok()?
                                .into_iter()
                                .find(|(name, _)| name == &formal.name)?
                                .1;
                            let (actual, span) =
                                actual.interface_actual(&formal.name, file).ok()??;
                            let handle = handles.at(&actual.handle, span)?;
                            Some((formal.name.clone(), handle.interface.clone()))
                        })
                        .collect::<Vec<_>>();
                    let child_expansion = design.expansion(
                        &child_name,
                        &formal,
                        &handle.interface,
                        &child_bindings,
                    )?;
                    if let Some(parent) = &handle.port {
                        for (member, direction) in &child_expansion.members {
                            match parent.direction(member) {
                                None => {
                                    return Err(unsupported(format!(
                                        "port `{formal}` of `{child_name}` needs member `{member}`, which port `{}` of `{name}` does not expose",
                                        actual.handle
                                    )));
                                }
                                Some(Direction::Input) if *direction == Direction::Output => {
                                    return Err(unsupported(format!(
                                        "port `{formal}` of `{child_name}` drives member `{member}`, an input of port `{}` of `{name}`",
                                        actual.handle
                                    )));
                                }
                                _ => {}
                            }
                        }
                    }
                    if actual.modport.is_some() && actual.modport != port.modport {
                        return Err(unsupported(format!(
                            "modport mismatch at `{child_name}.{formal}`"
                        )));
                    }
                }
                generic.sort();
                let child_alias = if generic.is_empty() {
                    child_name.clone()
                } else {
                    clone_module_name(&child_name, &generic)
                };
                children.insert((*id, index), child_alias);
                queue.push((child_name.clone(), generic));
            }
        }
        result.modules.insert(
            bound_name,
            NativeModulePlan {
                original: name,
                ports,
                children,
            },
        );
    }
    Ok(result)
}
