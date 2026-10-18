//! Interface contracts and binding plans collected from authored declarations.
//!
//! Port directions and generic bindings are computed before semantic lowering.
//! `native_interfaces` binds bodies and references in the original syntax tree;
//! these contracts never generate or reparse SystemVerilog source.

use std::path::Path;

use super::instances::normalize_identifier;
use super::*;

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
    let design = Design::collect_selected(&files, selected)?;
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
    let packages = packages::Packages::analyze(&trees)?;
    let mut used_names = design
        .modules
        .keys()
        .chain(design.interfaces.keys())
        .cloned()
        .collect::<HashSet<_>>();
    declarations
        .into_iter()
        .map(|interface| {
            // Analyzing the signature evaluates constant functions, which can
            // warn.
            let (prototype, warnings) =
                super::warnings::analyzing(|| -> Result<InterfacePrototype, AnalyzerError> {
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
                    let behavior_captures = interface
                        .captured_members(&interface.function_closure(&behavior_functions));
                    Ok(InterfacePrototype {
                        name: interface.name.clone(),
                        warnings: Vec::new(),
                        header_dependent_cast: interface
                            .header_dependent_cast(&files[interface.file])?,
                        module: {
                            let file = &files[interface.file];
                            let declaration = file
                                .syntax_tree
                                .into_iter()
                                .find_map(|node| {
                                    if let RefNode::InterfaceDeclarationAnsi(declaration) = node
                                        && name(
                                            RefNode::InterfaceIdentifier(
                                                &declaration.nodes.0.nodes.3,
                                            ),
                                            &file.syntax_tree,
                                        )
                                        .ok()
                                        .as_ref()
                                            == Some(&interface.name)
                                    {
                                        Some(declaration)
                                    } else {
                                        None
                                    }
                                })
                                .ok_or_else(|| unsupported("non-ANSI interface signature"))?;
                            interface_ast::signature(
                                declaration,
                                &file.syntax_tree,
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
                                    || behavior_captures.get(&member.name)
                                        == Some(&Direction::Output)
                                    || interface.references.iter().any(|(span, name)| {
                                        name == &member.name
                                            && interface.lvalues.contains(&span.0)
                                            && !interface.functions.iter().any(|function| {
                                                function.span.0 <= span.0
                                                    && span.1 <= function.span.1
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
                });
            prototype.map(|prototype| InterfacePrototype {
                warnings,
                ..prototype
            })
        })
        .collect::<Result<Vec<_>, AnalyzerError>>()
}

type Span = (usize, usize);

pub(crate) fn component_references(tree: &SyntaxTree) -> HashSet<String> {
    tree.into_iter()
        .filter_map(|node| match node {
            RefNode::ModuleInstantiation(instance) => {
                identifier_text(RefNode::ModuleIdentifier(&instance.nodes.0), tree)
            }
            RefNode::InterfaceIdentifier(identifier) => {
                identifier_text(RefNode::InterfaceIdentifier(identifier), tree)
            }
            _ => None,
        })
        .collect()
}

fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}

fn name(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Result<String, AnalyzerError> {
    identifier_text(node, syntax_tree)
        .ok_or_else(|| unsupported("interface elaboration of an unnamed node"))
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
        let syntax_tree = crate::syntax::parse_source(code, path)?;
        let mut blanks = Vec::new();
        let mut locates = Vec::new();
        let mut identifiers = HashSet::default();
        let mut type_names = HashSet::default();
        for node in &syntax_tree {
            match node {
                RefNode::ModuleInstantiation(instantiation) => {
                    let root = RefNode::ModuleIdentifier(&instantiation.nodes.0);
                    let start = match unwrap_node!(root, SimpleIdentifier, EscapedIdentifier) {
                        Some(RefNode::SimpleIdentifier(identifier)) => {
                            origin_span(&syntax_tree, &identifier.nodes.0)
                        }
                        Some(RefNode::EscapedIdentifier(identifier)) => {
                            origin_span(&syntax_tree, &identifier.nodes.0)
                        }
                        _ => None,
                    };
                    type_names.extend(start.map(|(start, _)| start));
                }
                RefNode::StructUnionMember(member) => {
                    for node in RefNode::StructUnionMember(member) {
                        let RefNode::VariableIdentifier(identifier) = node else {
                            continue;
                        };
                        let start = match &identifier.nodes.0 {
                            sv_parser::Identifier::SimpleIdentifier(identifier) => {
                                origin_span(&syntax_tree, &identifier.nodes.0)
                            }
                            sv_parser::Identifier::EscapedIdentifier(identifier) => {
                                origin_span(&syntax_tree, &identifier.nodes.0)
                            }
                        };
                        type_names.extend(start.map(|(start, _)| start));
                    }
                }
                RefNode::SimpleIdentifier(identifier) => {
                    if let Some((start, _)) = origin_span(&syntax_tree, &identifier.nodes.0) {
                        identifiers.insert(start);
                    }
                }
                RefNode::EscapedIdentifier(identifier) => {
                    if let Some((start, _)) = origin_span(&syntax_tree, &identifier.nodes.0) {
                        identifiers.insert(start);
                    }
                }
                RefNode::WhiteSpace(white_space) => {
                    if let Some(span) = super::type_parameters::node_span(
                        RefNode::WhiteSpace(white_space),
                        &syntax_tree,
                    ) {
                        blanks.push(span);
                    }
                }
                RefNode::Locate(locate) => locates.extend(origin_span(&syntax_tree, locate)),
                _ => {}
            }
        }
        blanks.sort_unstable();
        let mut tokens: Vec<Span> = locates
            .into_iter()
            .filter(|&(start, _)| {
                let index = blanks.partition_point(|&(blank_start, _)| blank_start <= start);
                index == 0 || blanks[index - 1].1 <= start
            })
            .collect();
        tokens.sort_unstable();
        tokens.dedup();
        let file = Self {
            code,
            syntax_tree,
            tokens,
            identifiers,
            type_names,
        };
        Ok(file)
    }

    /// The span of the tokens of `node`, without the white space and comments
    /// that the parser attaches to its last token.
    fn node_span(&self, node: RefNode<'_>) -> Option<Span> {
        let (start, end) = super::type_parameters::node_span(node, &self.syntax_tree)?;
        let index = self
            .tokens
            .partition_point(|&(_, token_end)| token_end <= end);
        let end = index
            .checked_sub(1)
            .map(|index| self.tokens[index].1)
            .filter(|&token_end| token_end > start)
            .unwrap_or(start);
        Some((start, end))
    }

    fn span(&self, node: RefNode<'_>) -> Result<Span, AnalyzerError> {
        self.node_span(node)
            .ok_or_else(|| unsupported("interface elaboration of an empty node"))
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

    /// Whether the identifier at `start` is qualified by `$unit::`.
    fn unit_qualified(&self, start: usize) -> bool {
        let index = self.tokens.partition_point(|&(_, end)| end <= start);
        index >= 2
            && self.text(self.tokens[index - 1]) == "::"
            && self.text(self.tokens[index - 2]) == "$unit"
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

/// The span of a token in the original source text, or `None` for text the
/// preprocessor inserted. Token offsets refer to the preprocessed text.
fn origin_span(syntax_tree: &SyntaxTree, locate: &Locate) -> Option<Span> {
    if locate.len == 0 {
        return None;
    }
    let origin = |offset| {
        syntax_tree
            .get_origin(&Locate {
                offset,
                line: 0,
                len: 1,
            })
            .map(|(_, offset)| offset)
    };
    Some((
        origin(locate.offset)?,
        origin(locate.offset + locate.len - 1)? + 1,
    ))
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
        loop {
            let before = dependent.len();
            for span in &spans {
                let names = self.referenced(*span).collect::<Vec<_>>();
                if names.iter().any(|name| dependent.contains(name)) {
                    dependent.extend(names);
                }
            }
            if dependent.len() == before {
                break;
            }
        }
        for node in &file.syntax_tree {
            let RefNode::CastingType(_) = node else {
                continue;
            };
            let target = file.span(node)?;
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
    ) -> Result<Self, AnalyzerError> {
        let mut interfaces = HashMap::default();
        for (index, file) in files.iter().enumerate() {
            for node in &file.syntax_tree {
                if matches!(
                    node,
                    RefNode::InterfaceDeclarationAnsi(_)
                        | RefNode::InterfaceDeclarationNonansi(_)
                        | RefNode::InterfaceDeclarationWildcard(_)
                ) && let Some(selected) = selected
                    && let Some(identifier) = unwrap_node!(node.clone(), InterfaceIdentifier)
                    && !selected.contains(&name(identifier, &file.syntax_tree)?)
                {
                    continue;
                }
                match node {
                    RefNode::InterfaceDeclarationAnsi(declaration) => {
                        let mut interface = InterfaceDecl::collect(declaration, index, file)?;
                        let unit = unit_names(file)?;
                        let declared: HashSet<String> = declared_names(
                            RefNode::InterfaceDeclarationAnsi(declaration),
                            &file.syntax_tree,
                        )?
                        .into_iter()
                        .collect();
                        for node in RefNode::InterfaceDeclarationAnsi(declaration) {
                            if let RefNode::Identifier(identifier) = node {
                                let span = file.span(RefNode::Identifier(identifier))?;
                                // A qualified name does not resolve to the
                                // compilation unit, unless `$unit::` names it.
                                if !file.unit_qualified(span.0)
                                    && (matches!(file.previous_token(span.0), Some("." | "::"))
                                        || file.next_token(span.1) == Some("::"))
                                {
                                    continue;
                                }
                                let identifier =
                                    name(RefNode::Identifier(identifier), &file.syntax_tree)?;
                                if unit.contains(&identifier) && !declared.contains(&identifier) {
                                    interface.uses_unit_items = true;
                                    break;
                                }
                            }
                        }
                        if interfaces.contains_key(&interface.name) {
                            return Err(AnalyzerError::DuplicateModule {
                                name: interface.name,
                            });
                        }
                        interfaces.insert(interface.name.clone(), interface);
                    }
                    RefNode::InterfaceDeclarationNonansi(_)
                    | RefNode::InterfaceDeclarationWildcard(_) => {
                        return Err(unsupported("non-ANSI interface declarations"));
                    }
                    _ => {}
                }
            }
        }
        let mut modules = HashMap::default();
        let mut subroutines: HashMap<String, Vec<(Scope, usize, SubroutinePorts)>> =
            HashMap::default();
        let mut packages: HashMap<String, HashSet<String>> = HashMap::default();
        if !interfaces.is_empty() {
            for (index, file) in files.iter().enumerate() {
                let syntax_tree = &file.syntax_tree;
                let scopes = Scope::collect(file)?;
                for node in syntax_tree {
                    let subroutine = match node {
                        RefNode::ModuleDeclarationAnsi(declaration) => {
                            let module =
                                ModuleDecl::collect(declaration, index, file, &interfaces)?;
                            // Modules and interfaces share one name space; the
                            // module analysis reports duplicate modules.
                            if interfaces.contains_key(&module.name) {
                                return Err(AnalyzerError::DuplicateModule { name: module.name });
                            }
                            modules.insert(module.name.clone(), module);
                            continue;
                        }
                        RefNode::PackageDeclaration(declaration) => {
                            packages.insert(
                                name(
                                    RefNode::PackageIdentifier(&declaration.nodes.3),
                                    syntax_tree,
                                )?,
                                package_items(declaration, syntax_tree)?,
                            );
                            None
                        }
                        RefNode::FunctionDeclaration(declaration) => unwrap_node!(
                            RefNode::FunctionDeclaration(declaration),
                            FunctionIdentifier
                        )
                        .map(|identifier| (identifier, node.clone())),
                        RefNode::TaskDeclaration(declaration) => {
                            unwrap_node!(RefNode::TaskDeclaration(declaration), TaskIdentifier)
                                .map(|identifier| (identifier, node.clone()))
                        }
                        RefNode::FunctionPrototype(prototype) => Some((
                            RefNode::FunctionIdentifier(&prototype.nodes.2),
                            node.clone(),
                        )),
                        RefNode::TaskPrototype(prototype) => {
                            Some((RefNode::TaskIdentifier(&prototype.nodes.1), node.clone()))
                        }
                        _ => None,
                    };
                    if let Some((identifier, root)) = subroutine {
                        let scope = file
                            .node_span(root.clone())
                            .map_or(Scope::Unit, |span| Scope::of(span, &scopes));
                        subroutines
                            .entry(name(identifier, syntax_tree)?)
                            .or_default()
                            .push((scope, index, subroutine_ports(root, syntax_tree)?));
                    }
                }
            }
        }
        // Compilation-unit imports before an interface are visible in it.
        for (index, file) in files.iter().enumerate() {
            if !interfaces.values().any(|interface| interface.file == index) {
                continue;
            }
            let unit_imports = unit_imports(file)?;
            for interface in interfaces
                .values_mut()
                .filter(|interface| interface.file == index)
            {
                let preceding: Vec<Item> = unit_imports
                    .iter()
                    .filter(|(span, _)| span.1 <= interface.start)
                    .map(|(span, imports)| Item::Constant {
                        span: *span,
                        header: HeaderForm::Import {
                            imports: imports.clone(),
                        },
                    })
                    .collect();
                interface.items.splice(0..0, preceding);
            }
        }
        // A member an interface passes to a written subroutine argument is
        // assigned as well.
        for interface in interfaces.values_mut() {
            let file = &files[interface.file];
            let unit: Vec<(String, Option<String>)> = interface
                .items
                .iter()
                .flat_map(|item| match item {
                    Item::Constant {
                        header: HeaderForm::Import { imports, .. },
                        ..
                    } => imports.clone(),
                    _ => Vec::new(),
                })
                .collect();
            for node in &file.syntax_tree {
                let RefNode::InterfaceDeclarationAnsi(declaration) = node else {
                    continue;
                };
                if name(
                    RefNode::InterfaceIdentifier(&declaration.nodes.0.nodes.3),
                    &file.syntax_tree,
                )? != interface.name
                {
                    continue;
                }
                let scoped = scoped_imports(RefNode::InterfaceDeclarationAnsi(declaration), file)?;
                for node in RefNode::InterfaceDeclarationAnsi(declaration) {
                    if let RefNode::TfCall(call) = node {
                        written_arguments(
                            call,
                            &Scope::Interface(interface.name.clone()),
                            &|_| None,
                            file,
                            interface.file,
                            &subroutines,
                            &visible_imports(&unit, &scoped, file.span(RefNode::TfCall(call))?),
                            &mut interface.lvalues,
                        )?;
                    }
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

    fn module_node(
        &self,
        module: &ModuleDecl,
    ) -> Result<&'a sv_parser::ModuleDeclarationAnsi, AnalyzerError> {
        let file = &self.files[module.file];
        for node in &file.syntax_tree {
            if let RefNode::ModuleDeclarationAnsi(declaration) = node
                && file.node_span(RefNode::ModuleDeclarationAnsi(declaration)) == Some(module.span)
            {
                return Ok(declaration);
            }
        }
        Err(unsupported(format!("module `{}`", module.name)))
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
    ) -> Result<ModuleUses, AnalyzerError> {
        let module = &self.modules[module_name];
        let file = &self.files[module.file];
        let declaration = self.module_node(module)?;
        let mut uses = ModuleUses::default();
        // The interface of each handle, unless generic or ambiguous.
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
        let mut blocks = Vec::new();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            if let RefNode::GenerateBlock(block) = node {
                blocks.extend(file.node_span(RefNode::GenerateBlock(block)));
            }
        }
        // The interface instances with their scopes, which hide a port of
        // their name.
        let mut instances: Vec<(String, Span, String)> = Vec::new();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            let RefNode::ModuleInstantiation(instantiation) = node else {
                continue;
            };
            let type_name = name(
                RefNode::ModuleIdentifier(&instantiation.nodes.0),
                &file.syntax_tree,
            )?;
            if !self.interfaces.contains_key(&type_name) {
                continue;
            }
            let statement = file.span(RefNode::ModuleInstantiation(instantiation))?;
            let scope = blocks
                .iter()
                .filter(|block| block.0 <= statement.0 && statement.1 <= block.1)
                .min_by_key(|block| block.1 - block.0)
                .copied()
                .unwrap_or(module.span);
            for instance in instantiation.nodes.2.contents() {
                let instance_name = name(
                    RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
                    &file.syntax_tree,
                )?;
                instances.push((instance_name, scope, type_name.clone()));
            }
        }
        // The instance `handle` names at `span`, from the innermost scope.
        let instance_at = |handle: &str, span: Span| {
            instances
                .iter()
                .filter(|(instance, scope, _)| {
                    instance == handle && scope.0 <= span.0 && span.1 <= scope.1
                })
                .min_by_key(|(_, scope, _)| scope.1 - scope.0)
                .map(|(_, _, interface)| interface.clone())
        };
        // The interface of `handle` at `span`, unless generic.
        let interface_at = |handle: &str, span: Span| match instance_at(handle, span) {
            Some(interface) => Some(interface),
            None => handle_interfaces.get(handle).cloned().flatten(),
        };
        // Whether `handle` at `span` names the port rather than an instance.
        let is_port = |handle: &str, span: Span| instance_at(handle, span).is_none();
        // The compilation-unit imports before the module, and the module's
        // own by the scope they are visible in.
        let unit: Vec<(String, Option<String>)> = unit_imports(file)?
            .into_iter()
            .filter(|(span, _)| span.1 <= module.span.0)
            .flat_map(|(_, imports)| imports)
            .collect();
        let scoped = scoped_imports(RefNode::ModuleDeclarationAnsi(declaration), file)?;
        // An assignment target starts where its lvalue starts.
        let mut lvalues = HashSet::default();
        let mut generates = Vec::new();
        let mut subroutines = Vec::new();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            match node {
                RefNode::FunctionDeclaration(function) => {
                    subroutines.extend(file.node_span(RefNode::FunctionDeclaration(function)));
                }
                RefNode::TaskDeclaration(task) => {
                    subroutines.extend(file.node_span(RefNode::TaskDeclaration(task)));
                }
                RefNode::TfCall(call) => {
                    let span = file.span(RefNode::TfCall(call))?;
                    written_arguments(
                        call,
                        &Scope::Module(module_name.to_string()),
                        &|handle| interface_at(handle, span),
                        file,
                        module.file,
                        &self.subroutines,
                        &visible_imports(&unit, &scoped, span),
                        &mut lvalues,
                    )?
                }
                RefNode::SystemTfCall(call) => {
                    if let Some(destination) = readmem_destination(call, file)? {
                        written_expression(destination, file, &mut lvalues);
                    }
                }
                RefNode::VariableLvalue(lvalue) => {
                    lvalues.extend(
                        file.node_span(RefNode::VariableLvalue(lvalue))
                            .map(|span| span.0),
                    );
                }
                RefNode::NetLvalue(lvalue) => {
                    lvalues.extend(
                        file.node_span(RefNode::NetLvalue(lvalue))
                            .map(|span| span.0),
                    );
                }
                RefNode::ConditionalGenerateConstruct(construct) => {
                    generates
                        .extend(file.node_span(RefNode::ConditionalGenerateConstruct(construct)));
                }
                RefNode::LoopGenerateConstruct(construct) => {
                    generates.extend(file.node_span(RefNode::LoopGenerateConstruct(construct)));
                }
                _ => {}
            }
        }
        let context = |span: Span| Context::of(span, &generates, &subroutines);
        // Actuals of child ports: written ones, and ones of children whose
        // port directions are not known here.
        let mut unknown = HashSet::default();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            let RefNode::ModuleInstantiation(instantiation) = node else {
                continue;
            };
            let child_name = name(
                RefNode::ModuleIdentifier(&instantiation.nodes.0),
                &file.syntax_tree,
            )?;
            if self.interfaces.contains_key(&child_name) {
                continue;
            }
            let Some(child) = self.modules.get(&child_name) else {
                for node in RefNode::ModuleInstantiation(instantiation) {
                    if let RefNode::Expression(expression) = node {
                        unknown.extend(
                            file.node_span(RefNode::Expression(expression))
                                .map(|span| span.0),
                        );
                    }
                }
                continue;
            };
            let instance_context = context(file.span(RefNode::ModuleInstantiation(instantiation))?);
            for instance in instantiation.nodes.2.contents() {
                let actuals = connections(instance, child, file, child.has_interface_ports())?;
                let mut bindings = Vec::new();
                for (formal, actual) in &actuals {
                    if let Some(port) = child.port(formal)
                        && port
                            .interface
                            .as_ref()
                            .is_some_and(|port| port.interface.is_none())
                        && let Connection::Explicit(Some(expression)) = actual
                        && let Some(actual) = actual_handle(expression, file)?
                        && let Some(interface) = interface_at(
                            &actual.handle,
                            file.span(RefNode::Expression(expression))?,
                        )
                    {
                        bindings.push((formal.clone(), interface));
                    }
                }
                bindings.sort();
                for (formal, actual) in actuals {
                    // An implicit connection names no interface member.
                    let (Some(port), Connection::Explicit(Some(expression))) =
                        (child.port(&formal), actual)
                    else {
                        continue;
                    };
                    if port.interface.is_none() {
                        if port.writes {
                            written_expression(expression, file, &mut lvalues);
                        }
                    } else if let Some(actual) = actual_handle(expression, file)?
                        && is_port(&actual.handle, file.span(RefNode::Expression(expression))?)
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
        let ports: HashSet<&str> = module
            .ports
            .iter()
            .filter(|port| port.interface.is_some())
            .map(|port| port.name.as_str())
            .collect();
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

/// The names that variable, net, parameter, type, subroutine, subroutine
/// port, loop variable and genvar declarations under `root` declare, with
/// repetitions.
/// A genvar is listed once per use.
fn declared_names(
    root: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<String>, AnalyzerError> {
    declared_identifiers(root)
        .into_iter()
        .map(|identifier| name(identifier, syntax_tree))
        .collect()
}

/// The identifiers of the declarations under `root`, other than struct and
/// union members, which have their own name space.
fn declared_identifiers(root: RefNode<'_>) -> Vec<RefNode<'_>> {
    let mut names = Vec::new();
    let mut members = HashSet::default();
    for node in root {
        let identifier = match node {
            RefNode::StructUnionMember(member) => {
                for node in RefNode::StructUnionMember(member) {
                    if let RefNode::VariableDeclAssignmentVariable(declarator) = node {
                        members.insert(std::ptr::from_ref(declarator));
                    }
                }
                continue;
            }
            RefNode::VariableDeclAssignmentVariable(declarator)
                if members.contains(&std::ptr::from_ref(declarator)) =>
            {
                continue;
            }
            RefNode::VariableDeclAssignmentVariable(declarator) => {
                RefNode::VariableIdentifier(&declarator.nodes.0)
            }
            RefNode::NetDeclAssignment(declarator) => RefNode::NetIdentifier(&declarator.nodes.0),
            RefNode::ParamAssignment(declarator) => {
                RefNode::ParameterIdentifier(&declarator.nodes.0)
            }
            RefNode::TypeAssignment(declarator) => RefNode::TypeIdentifier(&declarator.nodes.0),
            RefNode::TypeDeclarationDataType(declarator) => {
                RefNode::TypeIdentifier(&declarator.nodes.2)
            }
            RefNode::TfPortItem(port) => match &port.nodes.4 {
                Some((identifier, _, _)) => RefNode::PortIdentifier(identifier),
                None => continue,
            },
            RefNode::ForVariableDeclaration(declaration) => {
                for (identifier, _, _) in declaration.nodes.2.contents() {
                    names.push(RefNode::VariableIdentifier(identifier));
                }
                continue;
            }
            RefNode::GenvarIdentifier(identifier) => RefNode::GenvarIdentifier(identifier),
            RefNode::FunctionDeclaration(function) => {
                match unwrap_node!(RefNode::FunctionDeclaration(function), FunctionIdentifier) {
                    Some(identifier) => identifier,
                    None => continue,
                }
            }
            RefNode::TaskDeclaration(task) => {
                match unwrap_node!(RefNode::TaskDeclaration(task), TaskIdentifier) {
                    Some(identifier) => identifier,
                    None => continue,
                }
            }
            _ => continue,
        };
        names.push(identifier);
    }
    names
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
    call: &sv_parser::TfCall,
    local: &Scope,
    handle_interface: &dyn Fn(&str) -> Option<String>,
    file: &File<'_>,
    file_index: usize,
    subroutines: &HashMap<String, Vec<(Scope, usize, SubroutinePorts)>>,
    imports: &[(String, Option<String>)],
    writes: &mut HashSet<usize>,
) -> Result<(), AnalyzerError> {
    let Some((_, arguments, _)) = call.nodes.2.as_ref().map(|paren| &paren.nodes) else {
        return Ok(());
    };
    let text = file.text(file.span(RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0))?);
    let callee = normalize_identifier(text.rsplit(['.', ':']).next().unwrap_or(text).trim());
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
    let declarations = if let Some((package, _)) = text.rsplit_once("::") {
        let package = normalize_identifier(package.rsplit("::").next().unwrap_or(package).trim());
        in_scope(&|scope| match scope {
            Scope::Package(name) => *name == package,
            Scope::Unit => package == "$unit",
            _ => false,
        })
    } else if let Some((handle, _)) = text.split_once('.') {
        let handle = normalize_identifier(handle.split('[').next().unwrap_or(handle).trim());
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
    let mut actuals: Vec<(Result<usize, String>, &sv_parser::Expression)> = Vec::new();
    let named = match arguments {
        sv_parser::ListOfArguments::Ordered(list) => {
            for (index, actual) in list.nodes.0.contents().into_iter().enumerate() {
                if let Some(actual) = actual {
                    actuals.push((Ok(index), actual));
                }
            }
            &list.nodes.1
        }
        sv_parser::ListOfArguments::Named(list) => {
            if let Some(actual) = &list.nodes.2.nodes.1 {
                actuals.push((
                    Err(name(RefNode::Identifier(&list.nodes.1), &file.syntax_tree)?),
                    actual,
                ));
            }
            &list.nodes.3
        }
    };
    for (_, _, formal, actual) in named {
        if let Some(actual) = &actual.nodes.1 {
            actuals.push((
                Err(name(RefNode::Identifier(formal), &file.syntax_tree)?),
                actual,
            ));
        }
    }
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
    root: RefNode<'_>,
    file: &File<'_>,
) -> Result<Vec<(Span, Vec<(String, Option<String>)>)>, AnalyzerError> {
    let root_span = file.span(root.clone())?;
    let mut scopes = Vec::new();
    let mut imports = Vec::new();
    for node in root {
        match node {
            RefNode::FunctionDeclaration(_)
            | RefNode::TaskDeclaration(_)
            | RefNode::SeqBlock(_)
            | RefNode::ParBlock(_)
            | RefNode::GenerateBlock(_) => scopes.extend(file.node_span(node)),
            RefNode::PackageImportDeclaration(import) => imports.push((
                file.span(RefNode::PackageImportDeclaration(import))?,
                imported_items(RefNode::PackageImportDeclaration(import), &file.syntax_tree)?,
            )),
            _ => {}
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
fn written_expression(
    expression: &sv_parser::Expression,
    file: &File<'_>,
    writes: &mut HashSet<usize>,
) {
    let root = RefNode::Expression(expression);
    writes.extend(file.node_span(root.clone()).map(|span| span.0));
    let mut selects = Vec::new();
    for node in root.clone() {
        if matches!(
            node,
            RefNode::BitSelect(_)
                | RefNode::PartSelectRange(_)
                | RefNode::ConstantBitSelect(_)
                | RefNode::ConstantPartSelectRange(_)
        ) {
            selects.extend(file.node_span(node));
        }
    }
    for node in root {
        if let RefNode::PrimaryHierarchical(primary) = node
            && let Some(span) = file.node_span(RefNode::PrimaryHierarchical(primary))
            && !selects
                .iter()
                .any(|select| select.0 <= span.0 && span.1 <= select.1)
        {
            writes.insert(span.0);
        }
    }
}

/// The destination of a `$readmemh` or `$readmemb` call.
fn readmem_destination<'b>(
    call: &'b sv_parser::SystemTfCall,
    file: &File<'_>,
) -> Result<Option<&'b sv_parser::Expression>, AnalyzerError> {
    let (identifier, arguments): (_, Vec<&Option<sv_parser::Expression>>) = match call {
        sv_parser::SystemTfCall::ArgOptionl(call) => (
            &call.nodes.0,
            match call.nodes.1.as_ref().map(|paren| &paren.nodes.1) {
                Some(sv_parser::ListOfArguments::Ordered(list)) => list.nodes.0.contents(),
                _ => return Ok(None),
            },
        ),
        sv_parser::SystemTfCall::ArgExpression(call) => {
            (&call.nodes.0, call.nodes.1.nodes.1.0.contents())
        }
        sv_parser::SystemTfCall::ArgDataType(_) => return Ok(None),
    };
    let name = file.text(file.span(RefNode::SystemTfIdentifier(identifier))?);
    if !matches!(SystemTask::from_name(name), Some(SystemTask::Readmem(_))) {
        return Ok(None);
    }
    Ok(arguments.get(1).and_then(|argument| argument.as_ref()))
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
    let scopes = Scope::collect(file)?;
    let mut imports = Vec::new();
    for node in &file.syntax_tree {
        let RefNode::PackageImportDeclaration(import) = node else {
            continue;
        };
        let span = file.span(RefNode::PackageImportDeclaration(import))?;
        if Scope::of(span, &scopes) == Scope::Unit {
            imports.push((
                span,
                imported_items(RefNode::PackageImportDeclaration(import), &file.syntax_tree)?,
            ));
        }
    }
    Ok(imports)
}

/// What the package imports under `root` import: `(package, item)`, with
/// no item for a wildcard import.
fn imported_items(
    root: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<(String, Option<String>)>, AnalyzerError> {
    let mut items = Vec::new();
    for node in root {
        match node {
            RefNode::PackageImportItemIdentifier(item) => items.push((
                name(RefNode::PackageIdentifier(&item.nodes.0), syntax_tree)?,
                Some(name(RefNode::Identifier(&item.nodes.2), syntax_tree)?),
            )),
            RefNode::PackageImportItemAsterisk(item) => items.push((
                name(RefNode::PackageIdentifier(&item.nodes.0), syntax_tree)?,
                None,
            )),
            _ => {}
        }
    }
    Ok(items)
}

/// The names that the items of `package` declare.
fn package_items(
    package: &sv_parser::PackageDeclaration,
    syntax_tree: &SyntaxTree,
) -> Result<HashSet<String>, AnalyzerError> {
    let mut names = HashSet::default();
    for (_, item) in &package.nodes.6 {
        names.extend(package_item_names(item, syntax_tree)?);
    }
    Ok(names)
}

/// The names that the compilation-unit items of `file` declare.
fn unit_names(file: &File<'_>) -> Result<HashSet<String>, AnalyzerError> {
    let mut names = HashSet::default();
    for node in &file.syntax_tree {
        if let RefNode::DescriptionPackageItem(item) = node {
            names.extend(package_item_names(&item.nodes.1, &file.syntax_tree)?);
        }
    }
    Ok(names)
}

/// The names that a package item declares.
pub(super) fn package_item_names(
    item: &sv_parser::PackageItem,
    syntax_tree: &SyntaxTree,
) -> Result<HashSet<String>, AnalyzerError> {
    let root = RefNode::PackageItem(item);
    // Only the name of a subroutine; its ports and locals are its own.
    let subroutine = unwrap_node!(root.clone(), FunctionDeclaration)
        .and_then(|function| unwrap_node!(function, FunctionIdentifier))
        .or_else(|| {
            unwrap_node!(root.clone(), TaskDeclaration)
                .and_then(|task| unwrap_node!(task, TaskIdentifier))
        });
    let mut names = HashSet::default();
    if let Some(identifier) = subroutine {
        names.insert(name(identifier, syntax_tree)?);
        return Ok(names);
    }
    names.extend(declared_names(root.clone(), syntax_tree)?);
    for node in root {
        if let RefNode::EnumNameDeclaration(declaration) = node {
            names.insert(name(
                RefNode::EnumIdentifier(&declaration.nodes.0),
                syntax_tree,
            )?);
        }
    }
    Ok(names)
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
    fn collect(file: &File<'_>) -> Result<Vec<(Span, Self)>, AnalyzerError> {
        let mut scopes = Vec::new();
        for node in &file.syntax_tree {
            let (identifier, scope): (Option<RefNode<'_>>, fn(String) -> Self) = match node {
                RefNode::ModuleDeclaration(_) => {
                    (unwrap_node!(node.clone(), ModuleIdentifier), Self::Module)
                }
                RefNode::InterfaceDeclaration(_) => (
                    unwrap_node!(node.clone(), InterfaceIdentifier),
                    Self::Interface,
                ),
                RefNode::PackageDeclaration(_) => {
                    (unwrap_node!(node.clone(), PackageIdentifier), Self::Package)
                }
                RefNode::ProgramDeclaration(_) | RefNode::ClassDeclaration(_) => {
                    (None, |_| Self::Other)
                }
                _ => continue,
            };
            let Some(span) = file.node_span(node.clone()) else {
                continue;
            };
            let name = match identifier {
                Some(identifier) => name(identifier, &file.syntax_tree)?,
                None => String::new(),
            };
            scopes.push((span, scope(name)));
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

fn subroutine_ports(
    root: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<SubroutinePorts, AnalyzerError> {
    let writes = |direction: &sv_parser::TfPortDirection| match direction {
        sv_parser::TfPortDirection::PortDirection(direction) => {
            !matches!(direction.as_ref(), sv_parser::PortDirection::Input(_))
        }
        sv_parser::TfPortDirection::ConstRef(_) => false,
    };
    let mut ports = Vec::new();
    // A port without a direction inherits the previous one, initially input.
    let mut inherited = false;
    for node in root {
        match node {
            RefNode::TfPortItem(item) => {
                if let Some(direction) = &item.nodes.1 {
                    inherited = writes(direction);
                }
                let port = item
                    .nodes
                    .4
                    .as_ref()
                    .map(|(identifier, _, _)| {
                        name(RefNode::PortIdentifier(identifier), syntax_tree)
                    })
                    .transpose()?;
                ports.push((port, inherited));
            }
            RefNode::TfPortDeclaration(declaration) => {
                let writes = writes(&declaration.nodes.1);
                for (identifier, _, _) in declaration.nodes.4.nodes.0.contents() {
                    ports.push((
                        Some(name(RefNode::PortIdentifier(identifier), syntax_tree)?),
                        writes,
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(ports)
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
fn has_qualifier(primary: &sv_parser::PrimaryHierarchical, file: &File<'_>) -> bool {
    primary.nodes.0.as_ref().is_some_and(|qualifier| {
        file.node_span(RefNode::ClassQualifierOrPackageScope(qualifier))
            .is_some()
    })
}

/// The interface handle a port connection names: `h`, `h[i]`, or `h.mp`.
fn actual_handle(
    expression: &sv_parser::Expression,
    file: &File<'_>,
) -> Result<Option<Actual>, AnalyzerError> {
    let sv_parser::Expression::Primary(primary) = expression else {
        return Ok(None);
    };
    let sv_parser::Primary::Hierarchical(primary) = primary.as_ref() else {
        return Ok(None);
    };
    if has_qualifier(primary, file) || primary.nodes.1.nodes.0.is_some() {
        return Ok(None);
    }
    let syntax_tree = &file.syntax_tree;
    let identifier = &primary.nodes.1;
    let non_empty = |span: Option<Span>| span.filter(|span| span.1 > span.0);
    let trailing = non_empty(file.node_span(RefNode::Select(&primary.nodes.2)));
    match identifier.nodes.1.as_slice() {
        [] => Ok(Some(Actual {
            handle: name(RefNode::Identifier(&identifier.nodes.2), syntax_tree)?,
            selects: trailing,
            modport: None,
        })),
        [(head, selects, _)] if trailing.is_none() => Ok(Some(Actual {
            handle: name(RefNode::Identifier(head), syntax_tree)?,
            selects: non_empty(file.node_span(RefNode::ConstantBitSelect(selects))),
            modport: Some(name(RefNode::Identifier(&identifier.nodes.2), syntax_tree)?),
        })),
        _ => Ok(None),
    }
}

/// The port connections of `instance` of `module` as `(formal, actual)`.
/// Implicit and wildcard named connections, which cannot name an interface
/// item, are rejected when `strict`, and skipped otherwise.
/// The connections of `instance` to `module` as `(formal, actual)`. An
/// implicit named connection `.x` to an ordinary port is `(x, Implicit)`;
/// unless `strict`, implicit connections are skipped instead.
fn connections<'b>(
    instance: &'b sv_parser::HierarchicalInstance,
    module: &ModuleDecl,
    file: &File<'_>,
    strict: bool,
) -> Result<Vec<(String, Connection<'b>)>, AnalyzerError> {
    let Some(list) = &instance.nodes.1.nodes.1 else {
        return Ok(Vec::new());
    };
    match list {
        sv_parser::ListOfPortConnections::Ordered(list) => {
            let connections = list.nodes.0.contents();
            if connections.len() == 1 && connections[0].nodes.1.is_none() && module.ports.is_empty()
            {
                return Ok(Vec::new());
            }
            if connections.len() > module.ports.len() {
                return Err(unsupported(format!(
                    "more port connections than ports of `{}`",
                    module.name
                )));
            }
            Ok(module
                .ports
                .iter()
                .zip(
                    connections
                        .into_iter()
                        .map(|connection| connection.nodes.1.as_ref())
                        .chain(std::iter::repeat(None)),
                )
                .map(|(port, actual)| (port.name.clone(), Connection::Explicit(actual)))
                .collect())
        }
        sv_parser::ListOfPortConnections::Named(list) => {
            let mut connections: Vec<(String, Connection<'b>)> = list
            .nodes
            .0
            .contents()
            .into_iter()
            .filter_map(|connection| match connection {
                sv_parser::NamedPortConnection::Identifier(connection) => {
                    let formal = match name(RefNode::PortIdentifier(&connection.nodes.2), &file.syntax_tree) {
                        Ok(formal) => formal,
                        Err(error) => return Some(Err(error)),
                    };
                    match &connection.nodes.3 {
                        Some(actual) => Some(Ok((formal, Connection::Explicit(actual.nodes.1.as_ref())))),
                        None if !strict => None,
                        None if module.port(&formal).is_some_and(|port| port.interface.is_none()) => {
                            Some(Ok((formal, Connection::Implicit)))
                        }
                        None => Some(Err(unsupported(format!(
                            "implicit named port connection `.{formal}` to a module with interface ports"
                        )))),
                    }
                }
                sv_parser::NamedPortConnection::Asterisk(_) if !strict => None,
                sv_parser::NamedPortConnection::Asterisk(_) => {
                    Some(Err(unsupported("wildcard port connection")))
                }
            })
            .collect::<Result<_, _>>()?;
            // An omitted interface port is unconnected, which the caller
            // rejects.
            if strict {
                for port in &module.ports {
                    if port.interface.is_some()
                        && !connections.iter().any(|(formal, _)| *formal == port.name)
                    {
                        connections.push((port.name.clone(), Connection::Explicit(None)));
                    }
                }
            }
            Ok(connections)
        }
    }
}

/// The actual of a port connection.
#[derive(Clone, Copy)]
enum Connection<'b> {
    /// `.x(expression)`, or `.x()` and positional gaps without one.
    Explicit(Option<&'b sv_parser::Expression>),
    /// `.x`, connecting the signal of the port's name.
    Implicit,
}

fn parameter_overrides(
    assignment: Option<&sv_parser::ParameterValueAssignment>,
    parameters: &[String],
    file: &File<'_>,
) -> Result<Vec<(String, Span)>, AnalyzerError> {
    let Some(assignment) = assignment else {
        return Ok(Vec::new());
    };
    let Some(list) = &assignment.nodes.1.nodes.1 else {
        return Ok(Vec::new());
    };
    match list {
        sv_parser::ListOfParameterAssignments::Ordered(list) => {
            let values = list.nodes.0.contents();
            if values.len() > parameters.len() {
                return Err(unsupported("more parameter values than parameters"));
            }
            values
                .into_iter()
                .zip(parameters)
                .map(|(value, parameter)| {
                    Ok((
                        parameter.clone(),
                        file.span(RefNode::ParamExpression(&value.nodes.0))?,
                    ))
                })
                .collect()
        }
        sv_parser::ListOfParameterAssignments::Named(list) => list
            .nodes
            .0
            .contents()
            .into_iter()
            .filter_map(|assignment| {
                let parameter = match name(
                    RefNode::ParameterIdentifier(&assignment.nodes.1),
                    &file.syntax_tree,
                ) {
                    Ok(parameter) => parameter,
                    Err(error) => return Some(Err(error)),
                };
                let value = assignment.nodes.2.nodes.1.as_ref()?;
                Some(
                    file.span(RefNode::ParamExpression(value))
                        .map(|value| (parameter, value)),
                )
            })
            .collect(),
    }
}

fn check_no_connections(
    instance: &sv_parser::HierarchicalInstance,
    interface: &str,
) -> Result<(), AnalyzerError> {
    match &instance.nodes.1.nodes.1 {
        None => Ok(()),
        Some(sv_parser::ListOfPortConnections::Ordered(list)) if matches!(list.nodes.0.contents()[..], [connection] if connection.nodes.1.is_none()) => {
            Ok(())
        }
        Some(_) => Err(unsupported(format!("ports of interface `{interface}`"))),
    }
}

impl InterfaceDecl {
    fn collect(
        declaration: &sv_parser::InterfaceDeclarationAnsi,
        file_index: usize,
        file: &File<'_>,
    ) -> Result<Self, AnalyzerError> {
        let syntax_tree = &file.syntax_tree;
        let header = &declaration.nodes.0;
        let interface_name = name(RefNode::InterfaceIdentifier(&header.nodes.3), syntax_tree)?;
        if header
            .nodes
            .6
            .as_ref()
            .is_some_and(|ports| ports.nodes.0.nodes.1.is_some())
        {
            return Err(unsupported(format!(
                "ports of interface `{interface_name}`"
            )));
        }
        if !header.nodes.4.is_empty() {
            return Err(unsupported(format!(
                "package import in the header of interface `{interface_name}`"
            )));
        }
        // Enumerators are declared in the enclosing scope and would not be
        // renamed per instance.
        if unwrap_node!(
            RefNode::InterfaceDeclarationAnsi(declaration),
            EnumNameDeclaration
        )
        .is_some()
        {
            return Err(unsupported(format!(
                "enum type in interface `{interface_name}`"
            )));
        }
        let mut interface = Self {
            name: interface_name.clone(),
            file: file_index,
            start: file.span(RefNode::InterfaceDeclarationAnsi(declaration))?.0,
            has_parameter_port_list: header.nodes.5.is_some(),
            uses_unit_items: false,
            uses_macros: has_directive(
                file.text(file.span(RefNode::InterfaceDeclarationAnsi(declaration))?),
            ),
            parameters: Vec::new(),
            items: Vec::new(),
            members: Vec::new(),
            functions: Vec::new(),
            modports: HashMap::default(),
            names: HashSet::default(),
            references: Vec::new(),
            lvalues: HashSet::default(),
        };
        if let Some(list) = &header.nodes.5 {
            interface.collect_parameter_port_list(list, file)?;
        }
        for item in &declaration.nodes.2 {
            interface.collect_item(item, file)?;
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
        // Declarations in nested scopes must not shadow interface-scope names,
        // which are renamed without regard to scopes.
        let mut declared: HashMap<String, usize> = HashMap::default();
        for declared_name in
            declared_names(RefNode::InterfaceDeclarationAnsi(declaration), syntax_tree)?
        {
            *declared.entry(declared_name).or_default() += 1;
        }
        for (declared_name, count) in &declared {
            if *count > 1 && interface.names.contains(declared_name) {
                return Err(unsupported(format!(
                    "declaration of `{declared_name}` in a nested scope of interface `{interface_name}`, which shadows an interface item"
                )));
            }
        }
        // Unnamed generate blocks get names such as `genblk1` that would not
        // be renamed per instance.
        for node in RefNode::InterfaceDeclarationAnsi(declaration) {
            if let RefNode::Identifier(identifier) = node {
                let identifier_name = name(RefNode::Identifier(identifier), syntax_tree)?;
                if identifier_name
                    .strip_prefix("genblk")
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                    })
                    && !declared.contains_key(&identifier_name)
                {
                    return Err(unsupported(format!(
                        "reference to the implicit generate block name `{identifier_name}` in interface `{interface_name}`"
                    )));
                }
            }
        }
        // Generate block names belong to the interface scope as well.
        for node in RefNode::InterfaceDeclarationAnsi(declaration) {
            if let RefNode::GenerateBlockIdentifier(identifier) = node {
                interface.names.insert(name(
                    RefNode::GenerateBlockIdentifier(identifier),
                    syntax_tree,
                )?);
            }
        }
        // References: identifiers that name an interface-scope item, other
        // than members of a struct or a hierarchical name, named connections,
        // and package-qualified names.
        let mut member_spans = Vec::new();
        let declared: HashSet<String> =
            declared_names(RefNode::InterfaceDeclarationAnsi(declaration), syntax_tree)?
                .into_iter()
                .collect();
        for node in RefNode::InterfaceDeclarationAnsi(declaration) {
            match node {
                RefNode::MemberIdentifier(identifier) => {
                    member_spans.push(file.span(RefNode::MemberIdentifier(identifier))?);
                }
                RefNode::VariableLvalue(lvalue) => {
                    interface.lvalues.extend(
                        file.node_span(RefNode::VariableLvalue(lvalue))
                            .map(|span| span.0),
                    );
                }
                RefNode::NetLvalue(lvalue) => {
                    interface.lvalues.extend(
                        file.node_span(RefNode::NetLvalue(lvalue))
                            .map(|span| span.0),
                    );
                    // An undeclared target would be an implicit net of the
                    // interface, which is not renamed per instance.
                    // With macros the text need not show the target, but
                    // using the interface is rejected anyway.
                    if let sv_parser::NetLvalue::Identifier(target) = lvalue
                        && !interface.uses_macros
                    {
                        let target_name = file.text(
                            file.span(RefNode::PsOrHierarchicalNetIdentifier(&target.nodes.0))?,
                        );
                        if !target_name.contains(['.', ':']) {
                            let target_name = normalize_identifier(target_name.trim());
                            if !interface.names.contains(&target_name)
                                && !declared.contains(&target_name)
                            {
                                return Err(unsupported(format!(
                                    "implicit net `{target_name}` in interface `{interface_name}`"
                                )));
                            }
                        }
                    }
                }
                RefNode::SystemTfCall(call) => {
                    if let Some(destination) = readmem_destination(call, file)? {
                        written_expression(destination, file, &mut interface.lvalues);
                    }
                }
                _ => {}
            }
        }
        for node in RefNode::InterfaceDeclarationAnsi(declaration) {
            let RefNode::Identifier(identifier) = node else {
                continue;
            };
            let identifier_name = name(RefNode::Identifier(identifier), syntax_tree)?;
            if !interface.names.contains(&identifier_name) {
                continue;
            }
            let identifier_span = file.span(RefNode::Identifier(identifier))?;
            if member_spans
                .iter()
                .any(|span| span.0 <= identifier_span.0 && identifier_span.1 <= span.1)
                || matches!(file.previous_token(identifier_span.0), Some("." | "::"))
                || file.next_token(identifier_span.1) == Some("::")
            {
                continue;
            }
            interface
                .references
                .push((identifier_span, identifier_name));
        }
        Ok(interface)
    }

    fn collect_parameter_port_list(
        &mut self,
        list: &sv_parser::ParameterPortList,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        let mut declarations = Vec::new();
        match list {
            sv_parser::ParameterPortList::Assignment(list) => {
                for assignment in list.nodes.1.nodes.1.0.nodes.0.contents() {
                    self.add_parameter(assignment, None, file)?;
                }
                declarations.extend(
                    list.nodes
                        .1
                        .nodes
                        .1
                        .1
                        .iter()
                        .map(|(_, declaration)| declaration),
                );
            }
            sv_parser::ParameterPortList::Declaration(list) => {
                declarations.extend(list.nodes.1.nodes.1.contents());
            }
            sv_parser::ParameterPortList::Empty(_) => {}
        }
        for declaration in declarations {
            match declaration {
                sv_parser::ParameterPortDeclaration::ParameterDeclaration(declaration) => {
                    match declaration.as_ref() {
                        sv_parser::ParameterDeclaration::Param(parameter) => {
                            let data_type =
                                file.node_span(RefNode::DataTypeOrImplicit(&parameter.nodes.1));
                            for assignment in parameter.nodes.2.nodes.0.contents() {
                                self.add_parameter(assignment, data_type, file)?;
                            }
                        }
                        sv_parser::ParameterDeclaration::Type(_) => {
                            return Err(unsupported(format!(
                                "type parameter of interface `{}`",
                                self.name
                            )));
                        }
                    }
                }
                sv_parser::ParameterPortDeclaration::ParamList(list) => {
                    let data_type = Some(file.span(RefNode::DataType(&list.nodes.0))?);
                    for assignment in list.nodes.1.nodes.0.contents() {
                        self.add_parameter(assignment, data_type, file)?;
                    }
                }
                sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(declaration) => {
                    let item_span = file.span(RefNode::LocalParameterDeclaration(declaration))?;
                    self.add_constant((item_span.0, item_span.1), declaration, file)?;
                }
                sv_parser::ParameterPortDeclaration::TypeList(_) => {
                    return Err(unsupported(format!(
                        "type parameter of interface `{}`",
                        self.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn add_parameter(
        &mut self,
        assignment: &sv_parser::ParamAssignment,
        data_type: Option<Span>,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        if !assignment.nodes.1.is_empty() {
            return Err(unsupported(format!(
                "unpacked array parameter of interface `{}`",
                self.name
            )));
        }
        let parameter_name = name(
            RefNode::ParameterIdentifier(&assignment.nodes.0),
            &file.syntax_tree,
        )?;
        self.declare(parameter_name.clone())?;
        self.items.push(Item::Parameter(self.parameters.len()));
        self.parameters.push(InterfaceParameter {
            name: parameter_name,
            data_type,
            default: assignment
                .nodes
                .2
                .as_ref()
                .map(|(_, value)| file.span(RefNode::ConstantParamExpression(value)))
                .transpose()?,
        });
        Ok(())
    }

    /// A `localparam` declaration (from the header or the body).
    fn add_constant(
        &mut self,
        item_span: Span,
        declaration: &sv_parser::LocalParameterDeclaration,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        let header = match declaration {
            sv_parser::LocalParameterDeclaration::Param(parameter) => {
                for assignment in parameter.nodes.2.nodes.0.contents() {
                    self.declare(name(
                        RefNode::ParameterIdentifier(&assignment.nodes.0),
                        &file.syntax_tree,
                    )?)?;
                }
                HeaderForm::Constant
            }
            sv_parser::LocalParameterDeclaration::Type(parameter) => {
                for assignment in parameter.nodes.2.nodes.0.contents() {
                    self.declare(name(
                        RefNode::TypeIdentifier(&assignment.nodes.0),
                        &file.syntax_tree,
                    )?)?;
                }
                HeaderForm::Constant
            }
        };
        self.items.push(Item::Constant {
            span: item_span,
            header,
        });
        Ok(())
    }

    fn collect_item(
        &mut self,
        item: &sv_parser::NonPortInterfaceItem,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        let syntax_tree = &file.syntax_tree;
        let item = match item {
            sv_parser::NonPortInterfaceItem::ModportDeclaration(declaration) => {
                return self.collect_modports(declaration, file);
            }
            sv_parser::NonPortInterfaceItem::TimeunitsDeclaration(_) => return Ok(()),
            sv_parser::NonPortInterfaceItem::GenerateRegion(region) => {
                // Only the items: `generate` regions do not nest, and an
                // instance array places the logic in a generate loop.
                let (_, start) = file.span(RefNode::Keyword(&region.nodes.0))?;
                let (end, _) = file.span(RefNode::Keyword(&region.nodes.2))?;
                self.items.push(Item::Process((start, end)));
                return Ok(());
            }
            sv_parser::NonPortInterfaceItem::InterfaceOrGenerateItem(item) => item,
            _ => {
                return Err(unsupported(format!(
                    "nested declaration in interface `{}`",
                    self.name
                )));
            }
        };
        let sv_parser::InterfaceOrGenerateItem::Module(item) = item.as_ref() else {
            return Err(unsupported(format!(
                "extern subroutine in interface `{}`",
                self.name
            )));
        };
        let common = &item.nodes.1;
        let common_span = file.span(RefNode::ModuleCommonItem(common))?;
        match common {
            sv_parser::ModuleCommonItem::ContinuousAssign(_)
            | sv_parser::ModuleCommonItem::AlwaysConstruct(_)
            | sv_parser::ModuleCommonItem::InitialConstruct(_)
            | sv_parser::ModuleCommonItem::FinalConstruct(_)
            | sv_parser::ModuleCommonItem::LoopGenerateConstruct(_)
            | sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(_)
            | sv_parser::ModuleCommonItem::ElaborationSystemTask(_) => {
                self.items.push(Item::Process(common_span));
            }
            sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) => {
                match declaration.as_ref() {
                    sv_parser::ModuleOrGenerateItemDeclaration::GenvarDeclaration(genvars) => {
                        for node in RefNode::GenvarDeclaration(genvars) {
                            if let RefNode::GenvarIdentifier(identifier) = node {
                                self.names
                                    .insert(name(RefNode::GenvarIdentifier(identifier), syntax_tree)?);
                            }
                        }
                        self.items.push(Item::Process(common_span));
                    }
                    sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(
                        declaration,
                    ) => self.collect_declaration(declaration, common_span, file)?,
                    _ => {
                        return Err(unsupported(format!(
                            "clocking or default disable declaration in interface `{}`",
                            self.name
                        )));
                    }
                }
            }
            _ => {
                return Err(unsupported(format!(
                    "instantiation, assertion, or alias in interface `{}`",
                    self.name
                )));
            }
        }
        Ok(())
    }

    fn collect_declaration(
        &mut self,
        declaration: &sv_parser::PackageOrGenerateItemDeclaration,
        item_span: Span,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        let syntax_tree = &file.syntax_tree;
        match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::Empty(_) => {}
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(declaration) => {
                self.add_constant(item_span, &declaration.0, file)?;
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(declaration) => {
                // A body `parameter` of an interface with a parameter port list
                // is a localparam; without one it can be overridden (IEEE
                // 1800-2023 6.20.1).
                let sv_parser::ParameterDeclaration::Param(parameter) = &declaration.0 else {
                    return Err(unsupported(format!(
                        "type parameter of interface `{}`",
                        self.name
                    )));
                };
                if !self.has_parameter_port_list {
                    let data_type = file.node_span(RefNode::DataTypeOrImplicit(&parameter.nodes.1));
                    for assignment in parameter.nodes.2.nodes.0.contents() {
                        self.add_parameter(assignment, data_type, file)?;
                    }
                    return Ok(());
                }
                for assignment in parameter.nodes.2.nodes.0.contents() {
                    self.declare(name(
                        RefNode::ParameterIdentifier(&assignment.nodes.0),
                        syntax_tree,
                    )?)?;
                }
                self.items.push(Item::Constant {
                    span: item_span,
                    header: HeaderForm::Constant,
                });
            }
            sv_parser::PackageOrGenerateItemDeclaration::FunctionDeclaration(function) => {
                let function_name = name(
                    unwrap_node!(RefNode::FunctionDeclaration(function), FunctionIdentifier)
                        .ok_or_else(|| unsupported("function without a name"))?,
                    syntax_tree,
                )?;
                self.declare(function_name.clone())?;
                self.functions.push(Function {
                    name: function_name,
                    span: item_span,
                });
                self.items.push(Item::Function);
            }
            sv_parser::PackageOrGenerateItemDeclaration::NetDeclaration(declaration) => {
                match declaration.as_ref() {
                    sv_parser::NetDeclaration::NetType(net) => {
                        if net.nodes.1.is_some() || net.nodes.2.is_some() || net.nodes.4.is_some() {
                            return Err(unsupported(format!(
                                "net strength, vectored, or delay in interface `{}`",
                                self.name
                            )));
                        }
                        let data_type = file.node_span(RefNode::DataTypeOrImplicit(&net.nodes.3));
                        for declarator in net.nodes.5.nodes.0.contents() {
                            self.add_net_member(declarator, data_type, file)?;
                        }
                    }
                    sv_parser::NetDeclaration::NetTypeIdentifier(net) => {
                        // `T name;` with a user-defined type parses as a net of
                        // a user net type; it declares a variable of type `T`.
                        if net.nodes.1.is_some() {
                            return Err(unsupported(format!(
                                "delayed net in interface `{}`",
                                self.name
                            )));
                        }
                        let data_type = Some(file.span(RefNode::NetTypeIdentifier(&net.nodes.0))?);
                        for declarator in net.nodes.2.nodes.0.contents() {
                            self.add_net_member(declarator, data_type, file)?;
                        }
                    }
                    sv_parser::NetDeclaration::Interconnect(_) => {
                        return Err(unsupported(format!(
                            "interconnect in interface `{}`",
                            self.name
                        )));
                    }
                }
            }
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(declaration) => {
                match declaration.as_ref() {
                    sv_parser::DataDeclaration::Variable(variable) => {
                        if variable.nodes.0.is_some() {
                            return Err(unsupported(format!(
                                "const variable in interface `{}`",
                                self.name
                            )));
                        }
                        let data_type =
                            file.node_span(RefNode::DataTypeOrImplicit(&variable.nodes.3));
                        for declarator in variable.nodes.4.nodes.0.contents() {
                            let sv_parser::VariableDeclAssignment::Variable(declarator) =
                                declarator
                            else {
                                return Err(unsupported(format!(
                                    "dynamic array or class variable in interface `{}`",
                                    self.name
                                )));
                            };
                            let initializer = declarator
                                .nodes
                                .2
                                .as_ref()
                                .map(|(_, value)| file.span(RefNode::Expression(value)))
                                .transpose()?;
                            let member_name = name(
                                RefNode::VariableIdentifier(&declarator.nodes.0),
                                syntax_tree,
                            )?;
                            let dimensions = declarator
                                .nodes
                                .1
                                .iter()
                                .map(|dimension| file.span(RefNode::VariableDimension(dimension)))
                                .collect::<Result<_, _>>()?;
                            self.add_member(Member {
                                name: member_name,
                                data_type,
                                dimensions,
                                initializer,
                            })?;
                        }
                    }
                    sv_parser::DataDeclaration::TypeDeclaration(declaration) => {
                        let sv_parser::TypeDeclaration::DataType(typedef) = declaration.as_ref()
                        else {
                            return Err(unsupported(format!(
                                "forward type declaration in interface `{}`",
                                self.name
                            )));
                        };
                        if !typedef.nodes.3.is_empty() {
                            return Err(unsupported(format!(
                                "unpacked typedef in interface `{}`",
                                self.name
                            )));
                        }
                        self.declare(name(
                            RefNode::TypeIdentifier(&typedef.nodes.2),
                            syntax_tree,
                        )?)?;
                        self.items.push(Item::Constant {
                            span: item_span,
                            header: HeaderForm::Constant,
                        });
                    }
                    sv_parser::DataDeclaration::PackageImportDeclaration(import) => {
                        self.items.push(Item::Constant {
                            span: item_span,
                            header: HeaderForm::Import {
                                imports: imported_items(
                                    RefNode::PackageImportDeclaration(import),
                                    syntax_tree,
                                )?,
                            },
                        });
                    }
                    sv_parser::DataDeclaration::NetTypeDeclaration(_) => {
                        return Err(unsupported(format!(
                            "nettype declaration in interface `{}`",
                            self.name
                        )));
                    }
                }
            }
            _ => {
                return Err(unsupported(format!(
                    "task, class, or verification declaration in interface `{}`",
                    self.name
                )));
            }
        }
        Ok(())
    }

    fn add_net_member(
        &mut self,
        declarator: &sv_parser::NetDeclAssignment,
        data_type: Option<Span>,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        if declarator.nodes.2.is_some() {
            return Err(unsupported(format!(
                "net declaration assignment in interface `{}`",
                self.name
            )));
        }
        let dimensions = declarator
            .nodes
            .1
            .iter()
            .map(|dimension| file.span(RefNode::UnpackedDimension(dimension)))
            .collect::<Result<_, _>>()?;
        self.add_member(Member {
            name: name(
                RefNode::NetIdentifier(&declarator.nodes.0),
                &file.syntax_tree,
            )?,
            data_type,
            dimensions,
            initializer: None,
        })?;
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

    fn collect_modports(
        &mut self,
        declaration: &sv_parser::ModportDeclaration,
        file: &File<'_>,
    ) -> Result<(), AnalyzerError> {
        let syntax_tree = &file.syntax_tree;
        for modport in declaration.nodes.1.contents() {
            let modport_name = name(RefNode::ModportIdentifier(&modport.nodes.0), syntax_tree)?;
            let mut items = Vec::new();
            for ports in modport.nodes.1.nodes.1.contents() {
                match ports {
                    sv_parser::ModportPortsDeclaration::Simple(simple) => {
                        let direction = match &simple.nodes.1.nodes.0 {
                            sv_parser::PortDirection::Input(_) => Direction::Input,
                            sv_parser::PortDirection::Output(_) => Direction::Output,
                            _ => {
                                return Err(unsupported(format!(
                                    "inout or ref modport port in interface `{}`",
                                    self.name
                                )));
                            }
                        };
                        for port in simple.nodes.1.nodes.1.contents() {
                            let sv_parser::ModportSimplePort::Ordered(port) = port else {
                                return Err(unsupported(format!(
                                    "modport expression in interface `{}`",
                                    self.name
                                )));
                            };
                            items.push(ModportItem::Member(
                                direction,
                                name(RefNode::PortIdentifier(&port.nodes.0), syntax_tree)?,
                            ));
                        }
                    }
                    sv_parser::ModportPortsDeclaration::Tf(tf) => {
                        if matches!(tf.nodes.1.nodes.0, sv_parser::ImportExport::Export(_)) {
                            return Err(unsupported(format!(
                                "modport export in interface `{}`",
                                self.name
                            )));
                        }
                        for port in tf.nodes.1.nodes.1.contents() {
                            let sv_parser::ModportTfPort::TfIdentifier(function) = port else {
                                return Err(unsupported(format!(
                                    "modport import with a prototype in interface `{}`",
                                    self.name
                                )));
                            };
                            items.push(ModportItem::Import(name(
                                RefNode::TfIdentifier(function),
                                syntax_tree,
                            )?));
                        }
                    }
                    sv_parser::ModportPortsDeclaration::Clocking(_) => {
                        return Err(unsupported(format!(
                            "modport clocking in interface `{}`",
                            self.name
                        )));
                    }
                }
            }
            let mut listed = HashSet::default();
            for item in &items {
                let (ModportItem::Member(_, item_name) | ModportItem::Import(item_name)) = item;
                if !listed.insert(item_name) {
                    return Err(AnalyzerError::DuplicateModportItem {
                        interface: self.name.clone(),
                        modport: modport_name,
                        name: item_name.clone(),
                    });
                }
            }
            if self.modports.contains_key(&modport_name) {
                return Err(AnalyzerError::DuplicateModport {
                    interface: self.name.clone(),
                    name: modport_name,
                });
            }
            self.modports.insert(modport_name, items);
        }
        Ok(())
    }
}

impl ModuleDecl {
    fn collect(
        declaration: &sv_parser::ModuleDeclarationAnsi,
        file_index: usize,
        file: &File<'_>,
        interfaces: &HashMap<String, InterfaceDecl>,
    ) -> Result<Self, AnalyzerError> {
        let syntax_tree = &file.syntax_tree;
        let header = &declaration.nodes.0;
        let module_name = name(RefNode::ModuleIdentifier(&header.nodes.3), syntax_tree)?;
        let mut ports = Vec::new();
        if let Some(list) = &header.nodes.6
            && let Some(list) = &list.nodes.0.nodes.1
        {
            // A port without a header inherits the interface and modport of
            // the previous port (IEEE 1800-2023 23.2.2.3).
            let mut previous: Option<InterfacePort> = None;
            // A port without a direction inherits the previous one; the first
            // defaults to inout.
            let mut writes = true;
            for (_, port) in list.contents() {
                let direction = match port {
                    sv_parser::AnsiPortDeclaration::Net(net) => match &net.nodes.0 {
                        Some(sv_parser::NetPortHeaderOrInterfacePortHeader::NetPortHeader(
                            header,
                        )) => header.nodes.0.as_ref(),
                        _ => None,
                    },
                    sv_parser::AnsiPortDeclaration::Variable(variable) => variable
                        .nodes
                        .0
                        .as_ref()
                        .and_then(|header| header.nodes.0.as_ref()),
                    sv_parser::AnsiPortDeclaration::Paren(paren) => paren.nodes.0.as_ref(),
                };
                if let Some(direction) = direction {
                    writes = !matches!(direction, sv_parser::PortDirection::Input(_));
                }
                let (identifier, interface) = match port {
                    sv_parser::AnsiPortDeclaration::Net(net) => {
                        let interface = match &net.nodes.0 {
                            Some(
                                sv_parser::NetPortHeaderOrInterfacePortHeader::InterfacePortHeader(
                                    header,
                                ),
                            ) => match header.as_ref() {
                                sv_parser::InterfacePortHeader::Identifier(header) => {
                                    let interface = name(
                                        RefNode::InterfaceIdentifier(&header.nodes.0),
                                        syntax_tree,
                                    )?;
                                    // `T p` is an interface port only when `T`
                                    // names an interface; otherwise a typed port.
                                    interfaces
                                        .contains_key(&interface)
                                        .then_some((Some(interface), header.nodes.1.as_ref()))
                                }
                                sv_parser::InterfacePortHeader::Interface(header) => {
                                    Some((None, header.nodes.1.as_ref()))
                                }
                            },
                            _ => None,
                        };
                        let interface = match interface {
                            Some((interface, modport)) => Some(InterfacePort {
                                interface,
                                modport: modport
                                    .map(|(_, modport)| {
                                        name(RefNode::ModportIdentifier(modport), syntax_tree)
                                    })
                                    .transpose()?,
                                dimensions: Vec::new(),
                            }),
                            None if net.nodes.0.is_none() => previous.clone(),
                            None => None,
                        };
                        let interface = interface
                            .map(|interface| -> Result<_, AnalyzerError> {
                                if net.nodes.3.is_some() {
                                    return Err(unsupported("default value of an interface port"));
                                }
                                Ok(InterfacePort {
                                    dimensions: net
                                        .nodes
                                        .2
                                        .iter()
                                        .map(|dimension| {
                                            file.span(RefNode::UnpackedDimension(dimension))
                                        })
                                        .collect::<Result<_, _>>()?,
                                    ..interface
                                })
                            })
                            .transpose()?;
                        (RefNode::PortIdentifier(&net.nodes.1), interface)
                    }
                    sv_parser::AnsiPortDeclaration::Variable(variable)
                        if variable.nodes.0.is_none() && previous.is_some() =>
                    {
                        let interface =
                            previous
                                .clone()
                                .map(|previous| -> Result<_, AnalyzerError> {
                                    if variable.nodes.3.is_some() {
                                        return Err(unsupported(
                                            "default value of an interface port",
                                        ));
                                    }
                                    Ok(InterfacePort {
                                        dimensions: variable
                                            .nodes
                                            .2
                                            .iter()
                                            .map(|dimension| {
                                                file.span(RefNode::VariableDimension(dimension))
                                            })
                                            .collect::<Result<_, _>>()?,
                                        ..previous
                                    })
                                });
                        (
                            RefNode::PortIdentifier(&variable.nodes.1),
                            interface.transpose()?,
                        )
                    }
                    sv_parser::AnsiPortDeclaration::Variable(variable) => {
                        (RefNode::PortIdentifier(&variable.nodes.1), None)
                    }
                    sv_parser::AnsiPortDeclaration::Paren(paren) => {
                        (RefNode::PortIdentifier(&paren.nodes.2), None)
                    }
                };
                previous.clone_from(&interface);
                ports.push(PortDecl {
                    name: name(identifier, syntax_tree)?,
                    interface,
                    writes,
                });
            }
        }
        Ok(Self {
            name: module_name,
            file: file_index,
            span: file.span(RefNode::ModuleDeclarationAnsi(declaration))?,
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
    pub children: HashMap<usize, String>,
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
        let declaration = design.module_node(module)?;
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
                parameters: design.interfaces[interface]
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect(),
            });
        }
        let blocks = RefNode::ModuleDeclarationAnsi(declaration)
            .into_iter()
            .filter_map(|node| {
                if let RefNode::GenerateBlock(block) = node {
                    file.node_span(RefNode::GenerateBlock(block))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            let RefNode::ModuleInstantiation(instantiation) = node else {
                continue;
            };
            let interface = name_of_instantiation(instantiation, &file.syntax_tree)?;
            if !design.interfaces.contains_key(&interface) {
                continue;
            }
            let span = file.span(RefNode::ModuleInstantiation(instantiation))?;
            let parameter_names = design.interfaces[&interface]
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            let mut supplied = HashSet::default();
            for (name, _) in
                parameter_overrides(instantiation.nodes.1.as_ref(), &parameter_names, file)?
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
            for instance in instantiation.nodes.2.contents() {
                check_no_connections(instance, &interface)?;
                let handle = identifier_text(
                    RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
                    &file.syntax_tree,
                )
                .unwrap();
                handles.insert(
                    handle,
                    scope,
                    Handle {
                        interface: interface.clone(),
                        port: None,
                        arrayed: !instance.nodes.0.nodes.1.is_empty(),
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
                && (handle.arrayed || reference.selects.is_some())
            {
                return Err(unsupported(format!(
                    "call of a function of the interface array `{}`",
                    reference.handle
                )));
            }
        }
        let mut children = HashMap::default();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            let RefNode::ModuleInstantiation(instantiation) = node else {
                continue;
            };
            let child_name = name_of_instantiation(instantiation, &file.syntax_tree)?;
            let Some(child) = design.modules.get(&child_name) else {
                continue;
            };
            if !child.has_interface_ports() {
                continue;
            }
            for instance in instantiation.nodes.2.contents() {
                let mut generic = Vec::new();
                for (formal, actual) in connections(instance, child, file, true)? {
                    let Some(port) = child.port(&formal).and_then(|port| port.interface.as_ref())
                    else {
                        continue;
                    };
                    let Connection::Explicit(Some(expression)) = actual else {
                        return Err(unsupported(format!(
                            "missing interface actual of `{child_name}.{formal}`"
                        )));
                    };
                    let actual = actual_handle(expression, file)?.ok_or_else(|| {
                        unsupported(format!("interface actual of `{child_name}.{formal}`"))
                    })?;
                    let handle = handles
                        .at(&actual.handle, file.span(RefNode::Expression(expression))?)
                        .ok_or_else(|| {
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
                            let Connection::Explicit(Some(expression)) = actual else {
                                return None;
                            };
                            let actual = actual_handle(expression, file).ok()??;
                            let handle = handles.at(
                                &actual.handle,
                                file.span(RefNode::Expression(expression)).ok()?,
                            )?;
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
                let offset =
                    identifier_locate(RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0))
                        .unwrap()
                        .offset;
                children.insert(offset, child_alias);
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

fn name_of_instantiation(
    instantiation: &sv_parser::ModuleInstantiation,
    tree: &SyntaxTree,
) -> Converted<String> {
    identifier_text(RefNode::ModuleIdentifier(&instantiation.nodes.0), tree)
        .ok_or_else(|| unsupported("instance type name"))
}
