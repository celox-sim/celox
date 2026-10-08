//! Interface elaboration by flattening.
//!
//! The declaration collectors work on modules, so every interface is expanded
//! into the modules that use it before analysis, as source text:
//!
//! - An interface instance `I #(...) h [N] ();` is replaced by the interface
//!   items with each name declared in the interface scope renamed `h$name`:
//!   the parameters become localparams, the variables become signals of the
//!   instantiating module (the instance array dimensions in front of their
//!   own), and the processes and functions are copied. The processes of an
//!   instance array run once per element in a generate loop.
//! - An interface port `I.mp p` becomes one port `p$m` per member the modport
//!   lists, in the modport direction, plus an input for each member that an
//!   imported function reads. The interface parameters become parameters of
//!   the module (`p$P`), its localparams and typedefs localparams of the
//!   module header, and the imported functions functions of the module.
//! - A port without a modport (`I p`, or a generic `interface p`) exposes
//!   every member. A member is an output when the module, or a child the port
//!   is passed to, writes it, and an input otherwise.
//! - A module with a generic `interface` port is copied once per interface it
//!   is bound to, as `M$I`.
//! - `h[i].m` becomes `h$m[i]`, `h.f(...)` becomes `h$f(...)`, and a port
//!   connection `.p(h[i])` becomes one connection per member and parameter.
//!
//! `$` may continue but not start an identifier (IEEE 1800-2023 5.6), and the
//! generated names keep the SystemVerilog scoping of the names they replace.
//! A design with interfaces may not use `$` in its own identifiers, so that
//! generated names cannot collide with them.

use std::path::Path;

use super::instances::normalize_identifier;
use super::*;

/// Joins an interface handle (instance or port) and an interface item name.
const SEPARATOR: &str = "$";

type Span = (usize, usize);

fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}

fn name(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Result<String, AnalyzerError> {
    identifier_text(node, syntax_tree)
        .ok_or_else(|| unsupported("interface elaboration of an unnamed node"))
}

fn joined(handle: &str, item: &str) -> String {
    format!("{handle}{SEPARATOR}{item}")
}

/// Rewrite `sources` so that they no longer use interfaces, or return `None`
/// when no source declares an interface.
pub fn elaborate_interfaces(
    sources: &[(&str, &Path)],
) -> Result<Option<Vec<String>>, AnalyzerError> {
    let files = sources
        .iter()
        .map(|(code, path)| File::new(code, path))
        .collect::<Result<Vec<_>, _>>()?;
    let design = Design::collect(&files)?;
    if design.interfaces.is_empty() {
        return Ok(None);
    }
    // Generated names contain the separator; a source name containing it
    // could collide with one.
    for file in &files {
        for &start in &file.identifiers {
            let index = file.tokens.partition_point(|&(token, _)| token < start);
            let Some(&token) = file.tokens.get(index) else {
                continue;
            };
            let identifier = normalize_identifier(file.text(token));
            if identifier.contains(SEPARATOR) {
                return Err(unsupported(format!(
                    "identifier `{identifier}` containing `{SEPARATOR}` in a design with interfaces, which interface elaboration reserves for generated names"
                )));
            }
        }
    }
    design.elaborate().map(Some)
}

/// A parsed source and its tokens.
struct File<'a> {
    code: &'a str,
    syntax_tree: SyntaxTree,
    /// The spans of all tokens other than white space and comments, in order.
    tokens: Vec<Span>,
    /// The start offsets of the identifier tokens.
    identifiers: HashSet<usize>,
}

impl<'a> File<'a> {
    fn new(code: &'a str, path: &Path) -> Result<Self, AnalyzerError> {
        let syntax_tree = crate::syntax::parse_source(code, path)?;
        let mut blanks = Vec::new();
        let mut locates = Vec::new();
        let mut identifiers = HashSet::default();
        for node in &syntax_tree {
            match node {
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
                    if let Some(span) =
                        super::packages::node_span(RefNode::WhiteSpace(white_space), &syntax_tree)
                    {
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
        Ok(Self {
            code,
            syntax_tree,
            tokens,
            identifiers,
        })
    }

    /// The span of the tokens of `node`, without the white space and comments
    /// that the parser attaches to its last token.
    fn node_span(&self, node: RefNode<'_>) -> Option<Span> {
        let (start, end) = super::packages::node_span(node, &self.syntax_tree)?;
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
            if !self.identifiers.contains(&token.0) {
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
                Some(_) => self.code[token.1..tokens[next].0].trim().to_string(),
                None => String::new(),
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

/// A source text replacement.
#[derive(Debug, Clone)]
struct Edit {
    span: Span,
    text: String,
}

/// `text[span]` with the edits inside it applied. An edit nested in an
/// earlier one is dropped: the outer edit's text already accounts for it.
fn apply(text: &str, span: Span, edits: &[Edit]) -> String {
    let mut edits: Vec<&Edit> = edits
        .iter()
        .filter(|edit| edit.span.0 >= span.0 && edit.span.1 <= span.1)
        .collect();
    // At one offset, an insertion goes before a replacement that starts there.
    edits.sort_by_key(|edit| {
        (
            edit.span.0,
            edit.span.1 != edit.span.0,
            std::cmp::Reverse(edit.span.1),
        )
    });
    let mut result = String::new();
    let mut cursor = span.0;
    for edit in edits {
        if edit.span.0 < cursor {
            continue;
        }
        result.push_str(&text[cursor..edit.span.0]);
        result.push_str(&edit.text);
        cursor = edit.span.1;
    }
    result.push_str(&text[cursor..span.1]);
    result
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

/// How a constant item of an interface is declared in a module header.
enum HeaderForm {
    /// `localparam <text>` for a `localparam` or `parameter` declaration;
    /// the span covers the data type and the assignments.
    Parameter(Span),
    /// `localparam type <name> = <type>` for a `typedef`.
    TypeAlias { name: Span, data_type: Span },
    /// `localparam type <text>`; the span covers the type assignments.
    LocalType(Span),
    /// A package import, inserted in the module header.
    Import(Span),
}

enum MemberKind {
    Variable,
    /// A net, with its net type keyword.
    Net(Span),
}

/// A variable or net declared in the interface scope.
struct Member {
    name: String,
    kind: MemberKind,
    /// The declared data type, unless implicit.
    data_type: Option<Span>,
    /// The unpacked dimensions of the declarator.
    dimensions: Vec<Span>,
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
    Function(usize),
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
    /// Whether the header has a parameter port list, which makes a body
    /// `parameter` a localparam.
    has_parameter_port_list: bool,
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
    fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name == name)
    }

    fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
    }

    fn is_constant(&self, name: &str) -> bool {
        self.parameters
            .iter()
            .any(|parameter| parameter.name == name)
            || (self.names.contains(name)
                && self.member(name).is_none()
                && self.function(name).is_none())
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
    span: Span,
    interface: Option<InterfacePort>,
}

struct ModuleDecl {
    name: String,
    file: usize,
    span: Span,
    ports: Vec<PortDecl>,
    /// The parameters an instantiation may override, in order.
    parameters: Vec<String>,
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
    functions: Vec<String>,
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
    /// The text of the element selects after the handle name.
    selects: String,
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

/// A child instantiation found in a module body: its module, the formal
/// interface port and the name of the handle connected to it.
struct ChildBinding {
    module: String,
    formal: String,
    handle: String,
    /// Whether the instantiation is inside a generate construct.
    in_generate: bool,
}

struct Design<'a> {
    files: &'a [File<'a>],
    interfaces: HashMap<String, InterfaceDecl>,
    modules: HashMap<String, ModuleDecl>,
    expansions: std::cell::RefCell<HashMap<(String, String, String), Expansion>>,
    /// The expansions being computed, to reject a recursive instantiation.
    expanding: std::cell::RefCell<HashSet<(String, String, String)>>,
}

impl<'a> Design<'a> {
    fn collect(files: &'a [File<'a>]) -> Result<Self, AnalyzerError> {
        let mut interfaces = HashMap::default();
        for (index, file) in files.iter().enumerate() {
            for node in &file.syntax_tree {
                match node {
                    RefNode::InterfaceDeclarationAnsi(declaration) => {
                        let interface = InterfaceDecl::collect(declaration, index, file)?;
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
        if !interfaces.is_empty() {
            for (index, file) in files.iter().enumerate() {
                for node in &file.syntax_tree {
                    if let RefNode::ModuleDeclarationAnsi(declaration) = node {
                        let module = ModuleDecl::collect(declaration, index, file, &interfaces)?;
                        modules.insert(module.name.clone(), module);
                    }
                }
            }
        }
        Ok(Self {
            files,
            interfaces,
            modules,
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

    fn elaborate(&self) -> Result<Vec<String>, AnalyzerError> {
        let mut edits: Vec<Vec<Edit>> = vec![Vec::new(); self.files.len()];
        let mut appended: Vec<Vec<String>> = vec![Vec::new(); self.files.len()];
        let mut names: Vec<String> = self.modules.keys().cloned().collect();
        names.sort();
        let mut clones: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut cloned: HashSet<String> = HashSet::default();
        for name in &names {
            let module = &self.modules[name];
            if module.ports.iter().any(|port| {
                port.interface
                    .as_ref()
                    .is_some_and(|port| port.interface.is_none())
            }) {
                // Copied per binding below; the original is not usable.
                continue;
            }
            if let Some(text) = self.rewrite_module(name, &[], None, &mut clones)? {
                let module = &self.modules[name];
                edits[module.file].push(Edit {
                    span: module.span,
                    text,
                });
            }
        }
        while let Some((name, bindings)) = clones.pop() {
            let clone_name = clone_module_name(&name, &bindings);
            if !cloned.insert(clone_name.clone()) {
                continue;
            }
            let text =
                match self.rewrite_module(&name, &bindings, Some(&clone_name), &mut clones)? {
                    Some(text) => text,
                    None => {
                        let module = &self.modules[&name];
                        self.files[module.file].text(module.span).to_string()
                    }
                };
            appended[self.modules[&name].file].push(text);
        }
        Ok(self
            .files
            .iter()
            .zip(edits)
            .zip(appended)
            .map(|((file, edits), appended)| {
                let mut text = apply(file.code, (0, file.code.len()), &edits);
                for module in appended {
                    text.push('\n');
                    text.push_str(&module);
                    text.push('\n');
                }
                text
            })
            .collect())
    }

    /// The members and functions that `port` of `module` carries when it is
    /// bound to `interface`.
    fn expansion(
        &self,
        module: &str,
        port: &str,
        interface: &str,
    ) -> Result<Expansion, AnalyzerError> {
        let key = (module.to_string(), port.to_string(), interface.to_string());
        if let Some(expansion) = self.expansions.borrow().get(&key) {
            return Ok(expansion.clone());
        }
        if !self.expanding.borrow_mut().insert(key.clone()) {
            return Err(unsupported(format!(
                "recursive instantiation through interface port `{port}` of module `{module}`"
            )));
        }
        let expansion = self.compute_expansion(module, port, interface);
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
        let interface = self.interface(interface_name)?;
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
            let functions = interface.function_closure(&imports);
            // An imported function runs in the interface scope, so a member it
            // assigns is driven through the port whatever the modport lists.
            for (member, direction) in interface.captured_members(&functions) {
                let listed = listed.entry(member).or_insert(direction);
                if direction == Direction::Output {
                    *listed = Direction::Output;
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
            });
        }
        // Without a modport every member is reachable. A member the module or
        // a child writes is an output.
        let uses = self.module_uses(module_name)?;
        let interface = self.interface(interface_name)?;
        let called: HashSet<String> = uses
            .references
            .iter()
            .filter(|(handle, item)| handle == port_name && interface.function(item).is_some())
            .map(|(_, item)| item.clone())
            .collect();
        let functions = interface.function_closure(&called);
        // The directions do not depend on generate conditions or loop bounds,
        // which are not evaluated here, so a write that depends on them needs
        // a modport.
        let generate_write = |member: &str| {
            unsupported(format!(
                "write of `{port_name}.{member}` inside a generate construct of module `{module_name}`, whose port `{port_name}` has no modport"
            ))
        };
        let mut written: HashSet<String> = HashSet::default();
        for (handle, member, in_generate) in &uses.writes {
            if handle == port_name {
                if *in_generate {
                    return Err(generate_write(member));
                }
                written.insert(member.clone());
            }
        }
        written.extend(
            interface
                .captured_members(&functions)
                .into_iter()
                .filter(|(_, direction)| *direction == Direction::Output)
                .map(|(member, _)| member),
        );
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
            let child_expansion = self.expansion(&child.module, &child.formal, interface_name)?;
            for (member, direction) in child_expansion.members {
                if direction == Direction::Output {
                    if child.in_generate {
                        return Err(generate_write(&member));
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
        })
    }

    /// The interface accesses of a module body, by handle name.
    fn module_uses(&self, module_name: &str) -> Result<ModuleUses, AnalyzerError> {
        let module = &self.modules[module_name];
        let file = &self.files[module.file];
        let declaration = self.module_node(module)?;
        let mut uses = ModuleUses::default();
        // An assignment target starts where its lvalue starts.
        let mut lvalues = HashSet::default();
        let mut generates = Vec::new();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            match node {
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
        let in_generate = |(start, end): Span| {
            generates
                .iter()
                .any(|generate| generate.0 <= start && end <= generate.1)
        };
        let ports: HashSet<&str> = module
            .ports
            .iter()
            .filter(|port| port.interface.is_some())
            .map(|port| port.name.as_str())
            .collect();
        for reference in file.handle_references(module.span, |name| ports.contains(name)) {
            if let Some((item, _)) = reference.item {
                if lvalues.contains(&reference.span.0) {
                    uses.writes.push((
                        reference.handle.clone(),
                        item.clone(),
                        in_generate(reference.span),
                    ));
                }
                uses.references.push((reference.handle, item));
            }
        }
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            let RefNode::ModuleInstantiation(instantiation) = node else {
                continue;
            };
            let child_name = name(
                RefNode::ModuleIdentifier(&instantiation.nodes.0),
                &file.syntax_tree,
            )?;
            let Some(child) = self.modules.get(&child_name) else {
                continue;
            };
            if !child.has_interface_ports() {
                continue;
            }
            for instance in instantiation.nodes.2.contents() {
                for (formal, actual) in connections(instance, child, file)? {
                    if child
                        .port(&formal)
                        .is_some_and(|port| port.interface.is_some())
                        && let Some(expression) = actual
                        && let Some(actual) = actual_handle(expression, file)?
                    {
                        uses.children.push(ChildBinding {
                            module: child_name.clone(),
                            formal,
                            handle: actual.handle,
                            in_generate: in_generate(
                                file.span(RefNode::ModuleInstantiation(instantiation))?,
                            ),
                        });
                    }
                }
            }
        }
        Ok(uses)
    }

    /// The text of `module_name` with its interfaces expanded, or `None` when
    /// it uses none. A clone is renamed `clone_name` and binds its generic
    /// ports as `bindings` says. Clones of children are pushed to `clones`.
    fn rewrite_module(
        &self,
        module_name: &str,
        bindings: &[(String, String)],
        clone_name: Option<&str>,
        clones: &mut Vec<(String, Vec<(String, String)>)>,
    ) -> Result<Option<String>, AnalyzerError> {
        let module = &self.modules[module_name];
        let file = &self.files[module.file];
        let declaration = self.module_node(module)?;
        let module_span = module.span;

        // The interface handles: ports first, then instances.
        let mut handles: HashMap<String, Handle> = HashMap::default();
        let interface_ports: Vec<(String, InterfacePort, Span)> = module
            .ports
            .iter()
            .filter_map(|port| {
                port.interface
                    .clone()
                    .map(|interface| (port.name.clone(), interface, port.span))
            })
            .collect();
        let mut port_interfaces = Vec::new();
        for (port, interface_port, port_span) in &interface_ports {
            let interface = match &interface_port.interface {
                Some(interface) => interface.clone(),
                None => bindings
                    .iter()
                    .find(|(name, _)| name == port)
                    .map(|(_, interface)| interface.clone())
                    .ok_or_else(|| {
                        unsupported(format!(
                            "generic interface port `{port}` of module `{module_name}` without a binding"
                        ))
                    })?,
            };
            let expansion = self.expansion(module_name, port, &interface)?;
            handles.insert(
                port.clone(),
                Handle {
                    interface: interface.clone(),
                    port: Some(expansion.clone()),
                    arrayed: !interface_port.dimensions.is_empty(),
                },
            );
            port_interfaces.push((
                port.clone(),
                interface,
                expansion,
                interface_port.clone(),
                *port_span,
            ));
        }
        let mut interface_instantiations = Vec::new();
        let mut child_instantiations = Vec::new();
        for node in RefNode::ModuleDeclarationAnsi(declaration) {
            if let RefNode::ModuleInstantiation(instantiation) = node {
                let type_name = name(
                    RefNode::ModuleIdentifier(&instantiation.nodes.0),
                    &file.syntax_tree,
                )?;
                if self.interfaces.contains_key(&type_name) {
                    for instance in instantiation.nodes.2.contents() {
                        let instance_name = name(
                            RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
                            &file.syntax_tree,
                        )?;
                        let handle = Handle {
                            interface: type_name.clone(),
                            port: None,
                            arrayed: !instance.nodes.0.nodes.1.is_empty(),
                        };
                        if let Some(previous) = handles.get(&instance_name)
                            && (previous.interface != handle.interface || previous.port.is_some())
                        {
                            return Err(unsupported(format!(
                                "interface name `{instance_name}` declared twice with different interfaces in module `{module_name}`"
                            )));
                        }
                        handles.insert(instance_name, handle);
                    }
                    interface_instantiations.push(instantiation);
                } else if self
                    .modules
                    .get(&type_name)
                    .is_some_and(ModuleDecl::has_interface_ports)
                {
                    child_instantiations.push(instantiation);
                }
            }
        }
        if handles.is_empty() && child_instantiations.is_empty() && clone_name.is_none() {
            return Ok(None);
        }
        // Handle references are found by name, so no declaration may reuse
        // the name of a handle.
        for declared in declared_names(
            RefNode::ModuleDeclarationAnsi(declaration),
            &file.syntax_tree,
        )? {
            if handles.contains_key(&declared) {
                return Err(unsupported(format!(
                    "declaration of `{declared}` in module `{module_name}`, which shadows an interface"
                )));
            }
        }

        // References to interface items through a handle: `h[i].m`, `h.f(...)`.
        let mut references = Vec::new();
        let mut consumed: Vec<Span> = Vec::new();
        // Bare handles and modport selections, valid only as port connections.
        let mut pending: Vec<(Span, String)> = Vec::new();
        for reference in file.handle_references(module_span, |name| handles.contains_key(name)) {
            let handle = &handles[&reference.handle];
            let interface = self.interface(&handle.interface)?;
            let Some((item, item_span)) = reference.item else {
                pending.push((reference.span, reference.handle));
                continue;
            };
            let span = (reference.span.0, item_span.1);
            let text = if interface.member(&item).is_some() {
                if let Some(expansion) = &handle.port
                    && expansion.direction(&item).is_none()
                {
                    return Err(unsupported(format!(
                        "access of `{}.{item}`, which the modport of port `{}` does not list",
                        reference.handle, reference.handle
                    )));
                }
                format!("{}{}", joined(&reference.handle, &item), reference.selects)
            } else if interface.function(&item).is_some() {
                let reachable = match &handle.port {
                    Some(expansion) => expansion.functions.contains(&item),
                    None => true,
                };
                if !reachable {
                    return Err(unsupported(format!(
                        "call of `{}.{item}`, which the modport of port `{}` does not import",
                        reference.handle, reference.handle
                    )));
                }
                if !reference.selects.is_empty() || handle.arrayed {
                    return Err(unsupported(format!(
                        "call of a function of the interface array `{}`",
                        reference.handle
                    )));
                }
                joined(&reference.handle, &item)
            } else if interface.is_constant(&item) {
                joined(&reference.handle, &item)
            } else if interface.modports.contains_key(&item) {
                pending.push((span, reference.handle));
                continue;
            } else {
                return Err(unsupported(format!(
                    "`{}.{item}`: interface `{}` has no item `{item}`",
                    reference.handle, handle.interface
                )));
            };
            references.push(Edit { span, text });
        }

        let mut edits: Vec<Edit> = Vec::new();
        let render = |span: Span, references: &[Edit]| apply(file.code, span, references);

        // Interface instances are replaced by their items.
        for instantiation in &interface_instantiations {
            let statement = file.span(RefNode::ModuleInstantiation(instantiation))?;
            let interface = self.interface(&name(
                RefNode::ModuleIdentifier(&instantiation.nodes.0),
                &file.syntax_tree,
            )?)?;
            let overrides = parameter_overrides(
                instantiation.nodes.1.as_ref(),
                &interface
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect::<Vec<_>>(),
                file,
            )?;
            let mut text = String::new();
            for instance in instantiation.nodes.2.contents() {
                check_no_connections(instance, &interface.name)?;
                let instance_name = name(
                    RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
                    &file.syntax_tree,
                )?;
                let dimensions = instance
                    .nodes
                    .0
                    .nodes
                    .1
                    .iter()
                    .map(|dimension| {
                        Ok((file.span(RefNode::UnpackedDimension(dimension))?, dimension))
                    })
                    .collect::<Result<Vec<_>, AnalyzerError>>()?;
                let overrides: Vec<(String, String)> = overrides
                    .iter()
                    .map(|(parameter, value)| (parameter.clone(), render(*value, &references)))
                    .collect();
                text.push_str(&self.instance_text(
                    interface,
                    &instance_name,
                    &dimensions,
                    &overrides,
                    file,
                )?);
            }
            edits.push(Edit {
                span: statement,
                text,
            });
            consumed.push(statement);
        }

        // Child instantiations pass interfaces as members and parameters.
        for instantiation in &child_instantiations {
            let child_name = name(
                RefNode::ModuleIdentifier(&instantiation.nodes.0),
                &file.syntax_tree,
            )?;
            let instances = instantiation.nodes.2.contents();
            if instances.len() != 1 {
                return Err(unsupported(format!(
                    "several instances of `{child_name}` in one instantiation, which has interface ports"
                )));
            }
            let instance = instances[0];
            let child_ports: Vec<(String, Option<InterfacePort>)> = self.modules[&child_name]
                .ports
                .iter()
                .map(|port| (port.name.clone(), port.interface.clone()))
                .collect();
            let child_parameters = self.modules[&child_name].parameters.clone();
            let mut child_bindings = Vec::new();
            let mut connection_texts = Vec::new();
            let mut parameter_texts = Vec::new();
            let actuals = connections(instance, &self.modules[&child_name], file)?;
            for (formal, actual) in actuals {
                let Some((_, Some(formal_port))) =
                    child_ports.iter().find(|(name, _)| *name == formal)
                else {
                    connection_texts.push(match actual {
                        Some(expression) => format!(
                            ".{formal}({})",
                            render(file.span(RefNode::Expression(expression))?, &references)
                        ),
                        None => format!(".{formal}()"),
                    });
                    continue;
                };
                let expression = actual.ok_or_else(|| {
                    unsupported(format!(
                        "unconnected interface port `{formal}` of instance of `{child_name}`"
                    ))
                })?;
                let actual = actual_handle(expression, file)?.ok_or_else(|| {
                    unsupported(format!(
                        "interface port `{formal}` of `{child_name}` connected to an expression that is not an interface"
                    ))
                })?;
                let handle = handles.get(&actual.handle).cloned().ok_or_else(|| {
                    unsupported(format!(
                        "interface port `{formal}` of `{child_name}` connected to `{}`, which is not an interface",
                        actual.handle
                    ))
                })?;
                // Only the actuals of interface ports may name a bare handle.
                consumed.push(file.span(RefNode::Expression(expression))?);
                match &formal_port.interface {
                    Some(interface) if *interface != handle.interface => {
                        return Err(unsupported(format!(
                            "interface port `{formal}` of `{child_name}` of interface `{interface}` connected to an instance of `{}`",
                            handle.interface
                        )));
                    }
                    Some(_) => {}
                    None => child_bindings.push((formal.clone(), handle.interface.clone())),
                }
                if let Some(modport) = &actual.modport
                    && formal_port.modport.as_ref() != Some(modport)
                {
                    return Err(unsupported(format!(
                        "modport `{modport}` selected in the connection of port `{formal}` of `{child_name}`, which does not declare it"
                    )));
                }
                let child_expansion = self.expansion(&child_name, &formal, &handle.interface)?;
                for (member, direction) in &child_expansion.members {
                    if let Some(expansion) = &handle.port {
                        match expansion.direction(member) {
                            None => {
                                return Err(unsupported(format!(
                                    "port `{formal}` of `{child_name}` needs member `{member}`, which port `{}` of `{module_name}` does not expose",
                                    actual.handle
                                )));
                            }
                            Some(Direction::Input) if *direction == Direction::Output => {
                                return Err(unsupported(format!(
                                    "port `{formal}` of `{child_name}` drives member `{member}`, an input of port `{}` of `{module_name}`",
                                    actual.handle
                                )));
                            }
                            Some(_) => {}
                        }
                    }
                    connection_texts.push(format!(
                        ".{}({}{})",
                        joined(&formal, member),
                        joined(&actual.handle, member),
                        actual.selects
                    ));
                }
                for parameter in &self.interface(&handle.interface)?.parameters {
                    parameter_texts.push(format!(
                        ".{}({})",
                        joined(&formal, &parameter.name),
                        joined(&actual.handle, &parameter.name)
                    ));
                }
            }
            // Connections.
            let connections_span = file.span(RefNode::HierarchicalInstance(instance))?;
            let name_span = file.span(RefNode::NameOfInstance(&instance.nodes.0))?;
            edits.push(Edit {
                span: (name_span.1, connections_span.1),
                text: format!(" ({})", connection_texts.join(", ")),
            });
            // Parameters.
            let mut existing = Vec::new();
            for (parameter, value) in
                parameter_overrides(instantiation.nodes.1.as_ref(), &child_parameters, file)?
            {
                existing.push(format!(".{parameter}({})", render(value, &references)));
            }
            existing.extend(parameter_texts);
            let module_identifier = file.span(RefNode::ModuleIdentifier(&instantiation.nodes.0))?;
            let module_text = if child_bindings.is_empty() {
                child_name.clone()
            } else {
                child_bindings.sort();
                let clone = clone_module_name(&child_name, &child_bindings);
                clones.push((child_name.clone(), child_bindings));
                clone
            };
            let parameter_end = match &instantiation.nodes.1 {
                Some(assignment) => file.span(RefNode::ParameterValueAssignment(assignment))?.1,
                None => module_identifier.1,
            };
            let parameters = if existing.is_empty() {
                String::new()
            } else {
                format!(" #({})", existing.join(", "))
            };
            edits.push(Edit {
                span: (module_identifier.0, parameter_end),
                text: format!("{module_text}{parameters}"),
            });
        }

        // The header: module name, parameters, ports, and imported functions.
        let header = &declaration.nodes.0;
        if let Some(clone_name) = clone_name {
            edits.push(Edit {
                span: file.span(RefNode::ModuleIdentifier(&header.nodes.3))?,
                text: clone_name.to_string(),
            });
            if let Some((_, label)) = &declaration.nodes.4 {
                edits.push(Edit {
                    span: file.span(RefNode::ModuleIdentifier(label))?,
                    text: clone_name.to_string(),
                });
            }
        }
        let mut header_parameters = Vec::new();
        let mut header_imports = Vec::new();
        let mut functions_text = String::new();
        let mut port_texts: HashMap<String, Vec<String>> = HashMap::default();
        for (port, interface_name, expansion, interface_port, _) in &port_interfaces {
            let interface = self.interface(interface_name)?;
            let rename = |item: &str| Some(joined(port, item));
            let interface_file = file_of(self.files, interface);
            for item in &interface.items {
                let header = match item {
                    Item::Parameter(index) => {
                        let parameter = &interface.parameters[*index];
                        let data_type = parameter
                            .data_type
                            .map(|data_type| {
                                format!("{} ", interface.render(interface_file, data_type, &rename))
                            })
                            .unwrap_or_default();
                        let default = parameter
                            .default
                            .map(|default| {
                                format!(" = {}", interface.render(interface_file, default, &rename))
                            })
                            .unwrap_or_default();
                        header_parameters.push(format!(
                            "parameter {data_type}{}{default}",
                            joined(port, &parameter.name)
                        ));
                        continue;
                    }
                    Item::Constant { header, .. } => header,
                    _ => continue,
                };
                match header {
                    HeaderForm::Parameter(span) => header_parameters.push(format!(
                        "localparam {}",
                        interface.render(interface_file, *span, &rename)
                    )),
                    HeaderForm::TypeAlias { name, data_type } => header_parameters.push(format!(
                        "localparam type {} = {}",
                        interface.render(interface_file, *name, &rename),
                        interface.render(interface_file, *data_type, &rename)
                    )),
                    HeaderForm::LocalType(span) => header_parameters.push(format!(
                        "localparam type {}",
                        interface.render(interface_file, *span, &rename)
                    )),
                    HeaderForm::Import(span) => {
                        let import = interface_file.text(*span).to_string();
                        if !header_imports.contains(&import) {
                            header_imports.push(import);
                        }
                    }
                }
            }
            if !expansion.functions.is_empty() && !interface_port.dimensions.is_empty() {
                return Err(unsupported(format!(
                    "functions of the interface port array `{port}`"
                )));
            }
            for function in &expansion.functions {
                let function = interface.function(function).expect("expanded function");
                functions_text.push_str("\n    ");
                functions_text.push_str(&interface.render(
                    file_of(self.files, interface),
                    function.span,
                    &rename,
                ));
            }
            let dimensions: String = interface_port
                .dimensions
                .iter()
                .map(|&dimension| render(dimension, &references))
                .collect::<Vec<_>>()
                .join("");
            let mut texts = Vec::new();
            for (member_name, direction) in &expansion.members {
                let member = interface.member(member_name).expect("expanded member");
                texts.push(format!(
                    "{} {}",
                    interface.member_port_type(
                        file_of(self.files, interface),
                        member,
                        *direction,
                        &rename
                    ),
                    interface.member_declarator(
                        file_of(self.files, interface),
                        member,
                        &joined(port, member_name),
                        &dimensions,
                        &rename
                    )
                ));
            }
            // A port that carries only parameters or functions disappears.
            port_texts.insert(port.clone(), texts);
        }
        if !port_texts.is_empty() {
            let list = header
                .nodes
                .6
                .as_ref()
                .ok_or_else(|| unsupported("interface ports outside an ANSI port list"))?;
            let list_span = file.span(RefNode::ListOfPortDeclarations(list))?;
            let mut texts = Vec::new();
            for port in &module.ports {
                match port_texts.get(&port.name) {
                    Some(expanded) => texts.extend(expanded.iter().cloned()),
                    None => texts.push(render(port.span, &references)),
                }
            }
            let text = if texts.is_empty() {
                "()".to_string()
            } else {
                format!("(\n    {}\n)", texts.join(",\n    "))
            };
            edits.push(Edit {
                span: list_span,
                text,
            });
            consumed.push(list_span);
        }
        if !header_parameters.is_empty() {
            let text = header_parameters.join(",\n    ");
            match &header.nodes.5 {
                Some(sv_parser::ParameterPortList::Empty(empty)) => {
                    let (start, _) = file.span(RefNode::Symbol(&empty.0))?;
                    let (_, end) = file.span(RefNode::Symbol(&empty.2))?;
                    edits.push(Edit {
                        span: (start, end),
                        text: format!("#(\n    {text}\n)"),
                    });
                }
                Some(list) => {
                    let (_, end) = file.span(RefNode::ParameterPortList(list))?;
                    edits.push(Edit {
                        span: (end - 1, end - 1),
                        text: format!(",\n    {text}\n"),
                    });
                }
                None => {
                    let (_, end) = file.span(RefNode::ModuleIdentifier(&header.nodes.3))?;
                    edits.push(Edit {
                        span: (end, end),
                        text: format!(" #(\n    {text}\n)"),
                    });
                }
            }
        }
        if !header_imports.is_empty() {
            let (_, end) = file.span(RefNode::ModuleIdentifier(&header.nodes.3))?;
            edits.push(Edit {
                span: (end, end),
                text: format!(" {}", header_imports.join(" ")),
            });
        }
        if !functions_text.is_empty() {
            let (_, end) = file.span(RefNode::Symbol(&header.nodes.7))?;
            edits.push(Edit {
                span: (end, end),
                text: functions_text,
            });
        }
        // Every remaining bare handle is an unsupported use of an interface.
        if let Some((_, handle)) = pending.iter().find(|(reference, _)| {
            !consumed
                .iter()
                .any(|span| span.0 <= reference.0 && reference.1 <= span.1)
        }) {
            return Err(unsupported(format!(
                "use of interface `{handle}` other than as a port connection or through a member"
            )));
        }

        edits.extend(references);
        Ok(Some(apply(file.code, module_span, &edits)))
    }

    /// The items of interface `interface` for an instance `instance` with
    /// unpacked `dimensions` and parameter `overrides` (already rendered).
    fn instance_text(
        &self,
        interface: &InterfaceDecl,
        instance: &str,
        dimensions: &[(Span, &sv_parser::UnpackedDimension)],
        overrides: &[(String, String)],
        file: &File<'_>,
    ) -> Result<String, AnalyzerError> {
        let interface_file = file_of(self.files, interface);
        let genvars: Vec<String> = (0..dimensions.len())
            .map(|index| joined(instance, &format!("i{index}")))
            .collect();
        let element: String = genvars.iter().map(|genvar| format!("[{genvar}]")).collect();
        let plain = |item: &str| Some(joined(instance, item));
        let per_element = |item: &str| {
            Some(if interface.member(item).is_some() {
                format!("{}{element}", joined(instance, item))
            } else {
                joined(instance, item)
            })
        };
        let instance_dimensions: String = dimensions
            .iter()
            .map(|(span, _)| file.text(*span))
            .collect::<Vec<_>>()
            .join("");
        let mut declarations = Vec::new();
        let mut processes = Vec::new();
        for item in &interface.items {
            match item {
                Item::Parameter(index) => {
                    let parameter = &interface.parameters[*index];
                    let value = match overrides.iter().find(|(name, _)| *name == parameter.name) {
                        Some((_, value)) => value.clone(),
                        None => interface.render(
                            interface_file,
                            parameter.default.ok_or_else(|| {
                                unsupported(format!(
                                    "parameter `{}` of interface `{}` without a value",
                                    parameter.name, interface.name
                                ))
                            })?,
                            &plain,
                        ),
                    };
                    let data_type = parameter
                        .data_type
                        .map(|data_type| {
                            format!("{} ", interface.render(interface_file, data_type, &plain))
                        })
                        .unwrap_or_default();
                    declarations.push(format!(
                        "localparam {data_type}{} = {value};",
                        joined(instance, &parameter.name)
                    ));
                }
                Item::Constant { span, .. } => {
                    // A localparam of the parameter port list has no `;`.
                    let mut constant = interface.render(interface_file, *span, &plain);
                    if !constant.trim_end().ends_with(';') {
                        constant.push(';');
                    }
                    declarations.push(constant);
                }
                Item::Member(index) => {
                    let member = &interface.members[*index];
                    declarations.push(format!(
                        "{} {};",
                        interface.member_type(interface_file, member, &plain),
                        interface.member_declarator(
                            interface_file,
                            member,
                            &joined(instance, &member.name),
                            &instance_dimensions,
                            &plain
                        )
                    ));
                }
                Item::Function(index) => {
                    // The functions of an instance array cannot be called, as
                    // an element would need its own copy.
                    if dimensions.is_empty() {
                        declarations.push(interface.render(
                            interface_file,
                            interface.functions[*index].span,
                            &plain,
                        ));
                    }
                }
                Item::Process(span) => {
                    if dimensions.is_empty() {
                        declarations.push(interface.render(interface_file, *span, &plain));
                    } else {
                        processes.push(interface.render(interface_file, *span, &per_element));
                    }
                }
            }
        }
        let mut text = declarations.join("\n    ");
        if !processes.is_empty() {
            let mut body = processes.join("\n    ");
            for (index, (_, dimension)) in dimensions.iter().enumerate().rev() {
                let genvar = &genvars[index];
                let (low, high) = match dimension {
                    sv_parser::UnpackedDimension::Expression(size) => (
                        "0".to_string(),
                        format!(
                            "({}) - 1",
                            file.text(
                                file.span(RefNode::ConstantExpression(&size.nodes.0.nodes.1))?
                            )
                        ),
                    ),
                    sv_parser::UnpackedDimension::Range(range) => {
                        let left = file.text(
                            file.span(RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.0))?,
                        );
                        let right = file.text(
                            file.span(RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.2))?,
                        );
                        (
                            format!("(({left}) < ({right}) ? ({left}) : ({right}))"),
                            format!("(({left}) < ({right}) ? ({right}) : ({left}))"),
                        )
                    }
                };
                body = format!(
                    "for (genvar {genvar} = {low}; {genvar} <= {high}; {genvar}++) begin : {}\n    {body}\n    end",
                    joined(instance, &format!("g{index}"))
                );
            }
            text.push_str("\n    ");
            text.push_str(&body);
        }
        Ok(text)
    }
}

/// The names that variable, net, parameter, type, subroutine port, loop
/// variable and genvar declarations under `root` declare, with repetitions.
/// A genvar is listed once per use.
fn declared_names(
    root: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<String>, AnalyzerError> {
    let mut names = Vec::new();
    for node in root {
        let identifier = match node {
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
                    names.push(name(RefNode::VariableIdentifier(identifier), syntax_tree)?);
                }
                continue;
            }
            RefNode::GenvarIdentifier(identifier) => RefNode::GenvarIdentifier(identifier),
            _ => continue,
        };
        names.push(name(identifier, syntax_tree)?);
    }
    Ok(names)
}

fn file_of<'a, 'b>(files: &'b [File<'a>], interface: &InterfaceDecl) -> &'b File<'a> {
    &files[interface.file]
}

fn clone_module_name(module: &str, bindings: &[(String, String)]) -> String {
    let mut name = module.to_string();
    for (_, interface) in bindings {
        name.push_str(SEPARATOR);
        name.push_str(interface);
    }
    name
}

#[derive(Default)]
struct ModuleUses {
    /// `(handle, member, inside a generate construct)` assignment targets.
    writes: Vec<(String, String, bool)>,
    /// `(handle, item)` references, including function calls.
    references: Vec<(String, String)>,
    children: Vec<ChildBinding>,
}

/// A reference to an interface handle found in the tokens.
struct TokenReference {
    handle: String,
    /// The text of the element selects between the handle and the item.
    selects: String,
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
    let trailing = file
        .node_span(RefNode::Select(&primary.nodes.2))
        .map(|span| file.text(span).to_string())
        .unwrap_or_default();
    match identifier.nodes.1.as_slice() {
        [] => Ok(Some(Actual {
            handle: name(RefNode::Identifier(&identifier.nodes.2), syntax_tree)?,
            selects: trailing,
            modport: None,
        })),
        [(head, selects, _)] if trailing.is_empty() => Ok(Some(Actual {
            handle: name(RefNode::Identifier(head), syntax_tree)?,
            selects: file
                .node_span(RefNode::ConstantBitSelect(selects))
                .map(|span| file.text(span).to_string())
                .unwrap_or_default(),
            modport: Some(name(RefNode::Identifier(&identifier.nodes.2), syntax_tree)?),
        })),
        _ => Ok(None),
    }
}

/// The port connections of `instance` of `module` as `(formal, actual)`.
fn connections<'b>(
    instance: &'b sv_parser::HierarchicalInstance,
    module: &ModuleDecl,
    file: &File<'_>,
) -> Result<Vec<(String, Option<&'b sv_parser::Expression>)>, AnalyzerError> {
    let Some(list) = &instance.nodes.1.nodes.1 else {
        return Ok(Vec::new());
    };
    match list {
        sv_parser::ListOfPortConnections::Ordered(list) => {
            let connections = list.nodes.0.contents();
            if connections.len() == 1
                && connections[0].nodes.1.is_none()
                && module.ports.is_empty()
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
                .zip(connections.into_iter().map(|connection| connection.nodes.1.as_ref()).chain(std::iter::repeat(None)))
                .map(|(port, actual)| (port.name.clone(), actual))
                .collect())
        }
        sv_parser::ListOfPortConnections::Named(list) => list
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|connection| match connection {
                sv_parser::NamedPortConnection::Identifier(connection) => {
                    let formal = name(RefNode::PortIdentifier(&connection.nodes.2), &file.syntax_tree)?;
                    match &connection.nodes.3 {
                        Some(actual) => Ok((formal, actual.nodes.1.as_ref())),
                        None => Err(unsupported(format!(
                            "implicit named port connection `.{formal}` to a module with interface ports"
                        ))),
                    }
                }
                sv_parser::NamedPortConnection::Asterisk(_) => {
                    Err(unsupported("wildcard port connection"))
                }
            })
            .collect(),
    }
}

/// The parameter value assignments of an instantiation as `(name, value span)`.
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
        Some(sv_parser::ListOfPortConnections::Ordered(list))
            if list
                .nodes
                .0
                .contents()
                .iter()
                .all(|connection| connection.nodes.1.is_none()) =>
        {
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
        let mut interface = Self {
            name: interface_name.clone(),
            file: file_index,
            has_parameter_port_list: header.nodes.5.is_some(),
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
        // Declarations in nested scopes must not shadow interface-scope names,
        // which are renamed without regard to scopes.
        let mut declared: HashMap<String, usize> = HashMap::default();
        for declared_name in
            declared_names(RefNode::InterfaceDeclarationAnsi(declaration), syntax_tree)?
        {
            *declared.entry(declared_name).or_default() += 1;
        }
        for (declared_name, count) in declared {
            if count > 1 && interface.names.contains(&declared_name) {
                return Err(unsupported(format!(
                    "declaration of `{declared_name}` in a nested scope of interface `{interface_name}`, which shadows an interface item"
                )));
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
        self.names.insert(parameter_name.clone());
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
                    self.names.insert(name(
                        RefNode::ParameterIdentifier(&assignment.nodes.0),
                        &file.syntax_tree,
                    )?);
                }
                let start = file
                    .node_span(RefNode::DataTypeOrImplicit(&parameter.nodes.1))
                    .map_or(
                        file.span(RefNode::ListOfParamAssignments(&parameter.nodes.2))?
                            .0,
                        |span| span.0,
                    );
                HeaderForm::Parameter((
                    start,
                    file.span(RefNode::ListOfParamAssignments(&parameter.nodes.2))?
                        .1,
                ))
            }
            sv_parser::LocalParameterDeclaration::Type(parameter) => {
                for assignment in parameter.nodes.2.nodes.0.contents() {
                    self.names.insert(name(
                        RefNode::TypeIdentifier(&assignment.nodes.0),
                        &file.syntax_tree,
                    )?);
                }
                HeaderForm::LocalType(
                    file.span(RefNode::ListOfTypeAssignments(&parameter.nodes.2))?,
                )
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
                self.items
                    .push(Item::Process(file.span(RefNode::GenerateRegion(region))?));
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
                    self.names.insert(name(
                        RefNode::ParameterIdentifier(&assignment.nodes.0),
                        syntax_tree,
                    )?);
                }
                let start = file
                    .node_span(RefNode::DataTypeOrImplicit(&parameter.nodes.1))
                    .map_or(
                        file.span(RefNode::ListOfParamAssignments(&parameter.nodes.2))?
                            .0,
                        |span| span.0,
                    );
                self.items.push(Item::Constant {
                    span: item_span,
                    header: HeaderForm::Parameter((
                        start,
                        file.span(RefNode::ListOfParamAssignments(&parameter.nodes.2))?
                            .1,
                    )),
                });
            }
            sv_parser::PackageOrGenerateItemDeclaration::FunctionDeclaration(function) => {
                let function_name = name(
                    unwrap_node!(RefNode::FunctionDeclaration(function), FunctionIdentifier)
                        .ok_or_else(|| unsupported("function without a name"))?,
                    syntax_tree,
                )?;
                self.names.insert(function_name.clone());
                self.functions.push(Function {
                    name: function_name,
                    span: item_span,
                });
                self.items.push(Item::Function(self.functions.len() - 1));
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
                        let net_type = file.span(RefNode::NetType(&net.nodes.0))?;
                        let data_type = file.node_span(RefNode::DataTypeOrImplicit(&net.nodes.3));
                        for declarator in net.nodes.5.nodes.0.contents() {
                            self.add_net_member(
                                declarator,
                                MemberKind::Net(net_type),
                                data_type,
                                file,
                            )?;
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
                            self.add_net_member(declarator, MemberKind::Variable, data_type, file)?;
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
                            if declarator.nodes.2.is_some() {
                                return Err(unsupported(
                                    "variable declaration initializer in an interface",
                                ));
                            }
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
                                kind: MemberKind::Variable,
                                data_type,
                                dimensions,
                            });
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
                        if unwrap_node!(RefNode::DataType(&typedef.nodes.1), EnumNameDeclaration)
                            .is_some()
                        {
                            return Err(unsupported(format!(
                                "enum type in interface `{}`",
                                self.name
                            )));
                        }
                        self.names.insert(name(
                            RefNode::TypeIdentifier(&typedef.nodes.2),
                            syntax_tree,
                        )?);
                        self.items.push(Item::Constant {
                            span: item_span,
                            header: HeaderForm::TypeAlias {
                                name: file.span(RefNode::TypeIdentifier(&typedef.nodes.2))?,
                                data_type: file.span(RefNode::DataType(&typedef.nodes.1))?,
                            },
                        });
                    }
                    sv_parser::DataDeclaration::PackageImportDeclaration(import) => {
                        self.items.push(Item::Constant {
                            span: item_span,
                            header: HeaderForm::Import(
                                file.span(RefNode::PackageImportDeclaration(import))?,
                            ),
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
        kind: MemberKind,
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
            kind,
            data_type,
            dimensions,
        });
        Ok(())
    }

    fn add_member(&mut self, member: Member) {
        self.names.insert(member.name.clone());
        self.members.push(member);
        self.items.push(Item::Member(self.members.len() - 1));
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
            self.modports.insert(modport_name, items);
        }
        Ok(())
    }

    /// The text of `span` of the interface source with each interface-scope
    /// name renamed by `rename`.
    fn render(
        &self,
        file: &File<'_>,
        span: Span,
        rename: &dyn Fn(&str) -> Option<String>,
    ) -> String {
        let edits: Vec<Edit> = self
            .references
            .iter()
            .filter(|((start, end), _)| *start >= span.0 && *end <= span.1)
            .filter_map(|(span, name)| rename(name).map(|text| Edit { span: *span, text }))
            .collect();
        apply(file.code, span, &edits)
    }

    /// The data type of a member declaration with its kind keyword.
    fn member_type(
        &self,
        file: &File<'_>,
        member: &Member,
        rename: &dyn Fn(&str) -> Option<String>,
    ) -> String {
        let data_type = member
            .data_type
            .map(|span| self.render(file, span, rename))
            .unwrap_or_else(|| "logic".to_string());
        match member.kind {
            MemberKind::Variable => format!("var {data_type}"),
            MemberKind::Net(net_type) => format!("{} {data_type}", file.text(net_type)),
        }
    }

    fn member_port_type(
        &self,
        file: &File<'_>,
        member: &Member,
        direction: Direction,
        rename: &dyn Fn(&str) -> Option<String>,
    ) -> String {
        format!(
            "{} {}",
            direction.keyword(),
            self.member_type(file, member, rename)
        )
    }

    /// `name` followed by `outer` dimensions and the member's own.
    fn member_declarator(
        &self,
        file: &File<'_>,
        member: &Member,
        name: &str,
        outer: &str,
        rename: &dyn Fn(&str) -> Option<String>,
    ) -> String {
        let own: String = member
            .dimensions
            .iter()
            .map(|&dimension| self.render(file, dimension, rename))
            .collect();
        format!("{name}{outer}{own}")
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
            for (_, port) in list.contents() {
                let port_span = file.span(RefNode::AnsiPortDeclaration(port))?;
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
                    span: port_span,
                    interface,
                });
            }
        }
        let mut parameters = Vec::new();
        if let Some(list) = &header.nodes.5 {
            let mut declarations = Vec::new();
            match list {
                sv_parser::ParameterPortList::Assignment(list) => {
                    for assignment in list.nodes.1.nodes.1.0.nodes.0.contents() {
                        parameters.push(name(
                            RefNode::ParameterIdentifier(&assignment.nodes.0),
                            syntax_tree,
                        )?);
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
                if matches!(
                    declaration,
                    sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(_)
                ) {
                    continue;
                }
                for node in RefNode::ParameterPortDeclaration(declaration) {
                    match node {
                        RefNode::ParamAssignment(assignment) => parameters.push(name(
                            RefNode::ParameterIdentifier(&assignment.nodes.0),
                            syntax_tree,
                        )?),
                        RefNode::TypeAssignment(assignment) => parameters.push(name(
                            RefNode::TypeIdentifier(&assignment.nodes.0),
                            syntax_tree,
                        )?),
                        _ => {}
                    }
                }
            }
        }
        Ok(Self {
            name: module_name,
            file: file_index,
            span: file.span(RefNode::ModuleDeclarationAnsi(declaration))?,
            ports,
            parameters,
        })
    }
}
