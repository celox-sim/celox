use std::{fmt, path::Path};

use crate::nodes::*;
use crate::{Error, IncludePolicy, Options, Preprocessor, SourceMap, Token, TokenKind};

mod assignments;
mod body;
mod classes;
mod constraints;
mod coverage;
mod declarations;
mod dpi;
mod expressions;
mod generate;
mod instances;
mod keywords;
mod modports;
mod nets;
mod numbers;
mod packages;
mod procedures;
#[cfg(test)]
mod source_tests;
mod subroutines;
#[cfg(test)]
mod tests;
mod times;
mod typedefs;
mod types;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseErrorKind {
    MalformedHeader,
    UnsupportedSyntax,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub diagnostic: Error,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(f)
    }
}
impl std::error::Error for ParseError {}

/// Owned native declaration syntax, including authored source-item order and
/// compilation-unit variables/nets. Unit headers, DPI prototypes, package
/// imports/exports, typedefs, time declarations and parameter/assign statements
/// and body data/procedural control statements/function/task declarations have grammar nodes
/// in flat native arenas. Opaque body forms preserve their grammar boundaries;
/// semantic consumers must enforce their supported-language contracts.
#[derive(Clone, Debug)]
pub struct Declarations {
    pub sources: SourceMap,
    pub tokens: Vec<Token>,
    pub units: Vec<Unit>,
    pub source_items: Vec<SourceItem>,
    pub expressions: Vec<Expr>,
    pub types: Vec<DataType>,
    pub nested_units: Vec<Unit>,
    pub dpi_declarations: Vec<DpiDeclaration>,
    pub package_bindings: Vec<PackageBinding>,
    pub modport_declarations: Vec<ModportDeclaration>,
    pub type_aliases: Vec<TypeAlias>,
    pub time_declarations: Vec<TimeDeclaration>,
    pub time_directives: Vec<TimeDirective>,
    pub directives: Vec<TokenRange>,
    pub parameter_declarations: Vec<ParameterDeclaration>,
    pub assign_statements: Vec<AssignSyntax>,
    pub data_declarations: Vec<DataDeclaration>,
    pub net_declarations: Vec<NetDeclaration>,
    pub instantiations: Vec<Instantiation>,
    pub body_items: Vec<BodyItem>,
    pub procedures: Vec<Procedure>,
    pub statements: Vec<Statement>,
    pub timing_controls: Vec<TimingControl>,
    pub subroutines: Vec<Subroutine>,
}

impl Declarations {
    pub fn parse(code: &str, path: &Path) -> Result<Self> {
        let mut pp = Preprocessor::new(Options {
            includes: IncludePolicy::Ignore,
            ..Options::default()
        });
        let source = pp.preprocess(code, path).map_err(|diagnostic| ParseError {
            kind: ParseErrorKind::MalformedHeader,
            diagnostic,
        })?;
        Self::from_tokens(pp.sources, source.tokens)
    }

    pub fn from_tokens(sources: SourceMap, tokens: Vec<Token>) -> Result<Self> {
        let indices = tokens
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (!t.is_trivia()).then_some(i))
            .collect();
        let mut parser = Parser {
            sources: &sources,
            tokens: &tokens,
            indices,
            at: 0,
            next_parameter_id: 0,
            keyword_versions: Vec::new(),
            source_items: Vec::new(),
            expressions: Vec::new(),
            types: Vec::new(),
            nested_units: Vec::new(),
            dpi_declarations: Vec::new(),
            package_bindings: Vec::new(),
            modport_declarations: Vec::new(),
            type_aliases: Vec::new(),
            time_declarations: Vec::new(),
            time_directives: Vec::new(),
            directives: Vec::new(),
            parameter_declarations: Vec::new(),
            assign_statements: Vec::new(),
            data_declarations: Vec::new(),
            net_declarations: Vec::new(),
            instantiations: Vec::new(),
            body_items: Vec::new(),
            procedures: Vec::new(),
            statements: Vec::new(),
            timing_controls: Vec::new(),
            subroutines: Vec::new(),
        };
        let units = parser.units()?;
        let source_items = parser.source_items;
        let expressions = parser.expressions;
        let types = parser.types;
        let nested_units = parser.nested_units;
        let dpi_declarations = parser.dpi_declarations;
        let package_bindings = parser.package_bindings;
        let modport_declarations = parser.modport_declarations;
        let type_aliases = parser.type_aliases;
        let time_declarations = parser.time_declarations;
        let time_directives = parser.time_directives;
        let directives = parser.directives;
        let parameter_declarations = parser.parameter_declarations;
        let assign_statements = parser.assign_statements;
        let data_declarations = parser.data_declarations;
        let net_declarations = parser.net_declarations;
        let instantiations = parser.instantiations;
        let body_items = parser.body_items;
        let procedures = parser.procedures;
        let statements = parser.statements;
        let timing_controls = parser.timing_controls;
        let subroutines = parser.subroutines;
        Ok(Self {
            source_items,
            expressions,
            types,
            nested_units,
            dpi_declarations,
            package_bindings,
            modport_declarations,
            type_aliases,
            time_declarations,
            time_directives,
            directives,
            parameter_declarations,
            assign_statements,
            data_declarations,
            net_declarations,
            instantiations,
            body_items,
            procedures,
            statements,
            timing_controls,
            subroutines,
            sources,
            tokens,
            units,
        })
    }

    /// Iterate stable expression handles with their arena nodes in storage order.
    pub fn expression_nodes(&self) -> impl Iterator<Item = (ExprId, &Expr)> {
        self.expressions
            .iter()
            .enumerate()
            .map(|(index, node)| (ExprId(index), node))
    }

    /// Resolve an expression handle without recursive tree traversal.
    pub fn expression(&self, id: ExprId) -> &Expr {
        &self.expressions[id.0]
    }

    /// Resolve a structural datatype handle.
    pub fn type_nodes(&self) -> impl Iterator<Item = (TypeId, &DataType)> {
        self.types
            .iter()
            .enumerate()
            .map(|(index, data)| (TypeId(index), data))
    }

    pub fn data_type(&self, id: TypeId) -> &DataType {
        &self.types[id.0]
    }

    /// Resolve a source-level design-unit handle.
    pub fn source_unit(&self, id: SourceUnitId) -> &Unit {
        &self.units[id.0]
    }

    /// Resolve a nested-unit handle.
    pub fn unit(&self, id: UnitId) -> &Unit {
        &self.nested_units[id.0]
    }

    pub fn dpi_declaration(&self, id: DpiId) -> &DpiDeclaration {
        &self.dpi_declarations[id.0]
    }

    pub fn package_binding(&self, id: PackageBindingId) -> &PackageBinding {
        &self.package_bindings[id.0]
    }

    pub fn modport_declaration(&self, id: ModportDeclarationId) -> &ModportDeclaration {
        &self.modport_declarations[id.0]
    }

    pub fn type_alias(&self, id: TypeAliasId) -> &TypeAlias {
        &self.type_aliases[id.0]
    }

    pub fn time_declaration(&self, id: TimeDeclarationId) -> &TimeDeclaration {
        &self.time_declarations[id.0]
    }

    pub fn parameter_declaration(&self, id: ParameterDeclarationId) -> &ParameterDeclaration {
        &self.parameter_declarations[id.0]
    }

    pub fn assign_statement(&self, id: AssignId) -> &AssignSyntax {
        &self.assign_statements[id.0]
    }

    /// Resolve an authored ordinary-net declaration handle.
    pub fn net_declaration(&self, id: NetDeclarationId) -> &NetDeclaration {
        &self.net_declarations[id.0]
    }

    /// Resolve an authored instantiation, including typedef-shaped declarations.
    pub fn instantiation(&self, id: InstantiationId) -> &Instantiation {
        &self.instantiations[id.0]
    }

    pub fn body_item(&self, id: BodyItemId) -> &BodyItem {
        &self.body_items[id.0]
    }

    pub fn data_declaration(&self, id: DataDeclarationId) -> &DataDeclaration {
        &self.data_declarations[id.0]
    }

    pub fn timing_control(&self, id: TimingControlId) -> &TimingControl {
        &self.timing_controls[id.0]
    }

    pub fn procedure(&self, id: ProcedureId) -> &Procedure {
        &self.procedures[id.0]
    }

    pub fn statement(&self, id: StatementId) -> &Statement {
        &self.statements[id.0]
    }

    pub fn subroutine(&self, id: SubroutineId) -> &Subroutine {
        &self.subroutines[id.0]
    }

    /// Original expanded token spelling, used for diagnostics rather than
    /// semantic normalization or reparsing.
    pub fn token_text(&self, range: TokenRange) -> String {
        self.tokens[range.range()]
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    /// Declaration order including nested units, with each parent before its children.
    pub fn all_units(&self) -> impl Iterator<Item = &Unit> {
        let mut stack = self
            .source_items
            .iter()
            .rev()
            .filter_map(|item| match item.kind {
                SourceItemKind::Unit(id) => Some(self.source_unit(id)),
                SourceItemKind::Declaration(_) => None,
            })
            .collect::<Vec<_>>();
        std::iter::from_fn(move || {
            let unit = stack.pop()?;
            stack.extend(unit.children.iter().rev().map(|id| self.unit(*id)));
            Some(unit)
        })
    }
}

type Result<T> = std::result::Result<T, ParseError>;

struct Parser<'a> {
    sources: &'a SourceMap,
    tokens: &'a [Token],
    indices: Vec<usize>,
    at: usize,
    next_parameter_id: usize,
    keyword_versions: Vec<keywords::KeywordVersion>,
    source_items: Vec<SourceItem>,
    expressions: Vec<Expr>,
    types: Vec<DataType>,
    nested_units: Vec<Unit>,
    dpi_declarations: Vec<DpiDeclaration>,
    package_bindings: Vec<PackageBinding>,
    modport_declarations: Vec<ModportDeclaration>,
    type_aliases: Vec<TypeAlias>,
    time_declarations: Vec<TimeDeclaration>,
    time_directives: Vec<TimeDirective>,
    directives: Vec<TokenRange>,
    parameter_declarations: Vec<ParameterDeclaration>,
    assign_statements: Vec<AssignSyntax>,
    data_declarations: Vec<DataDeclaration>,
    net_declarations: Vec<NetDeclaration>,
    instantiations: Vec<Instantiation>,
    body_items: Vec<BodyItem>,
    procedures: Vec<Procedure>,
    statements: Vec<Statement>,
    timing_controls: Vec<TimingControl>,
    subroutines: Vec<Subroutine>,
}

impl Parser<'_> {
    fn peek(&self) -> &str {
        self.indices
            .get(self.at)
            .map_or("", |i| self.tokens[*i].text.as_str())
    }
    fn nth(&self, offset: usize) -> &str {
        self.indices
            .get(self.at + offset)
            .map_or("", |i| self.tokens[*i].text.as_str())
    }
    fn token(&self) -> Option<&Token> {
        self.indices.get(self.at).map(|i| &self.tokens[*i])
    }
    fn eat(&mut self, text: &str) -> bool {
        if self.peek() == text {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, text: &str) -> Result<()> {
        if self.eat(text) {
            Ok(())
        } else {
            Err(self.error(format!("expected `{text}`, found `{}`", self.peek())))
        }
    }
    fn range(&self, start: usize) -> TokenRange {
        let first = self
            .indices
            .get(start)
            .copied()
            .unwrap_or(self.tokens.len());
        let end = if self.at > start {
            self.indices[self.at - 1] + 1
        } else {
            first
        };
        TokenRange { start: first, end }
    }
    fn error(&self, message: impl Into<String>) -> ParseError {
        let token = self
            .token()
            .or_else(|| self.tokens.last())
            .expect("parser errors require an input token");
        ParseError {
            kind: ParseErrorKind::MalformedHeader,
            diagnostic: self.sources.error(token.origin, message),
        }
    }
    fn unsupported(&self, message: impl Into<String>) -> ParseError {
        let mut error = self.error(message);
        error.kind = ParseErrorKind::UnsupportedSyntax;
        error
    }
    fn name(&mut self) -> Result<Name> {
        self.contextual_name(&[])
    }
    fn contextual_name(&mut self, keywords: &[&str]) -> Result<Name> {
        let token = self
            .token()
            .ok_or_else(|| self.error("expected an identifier"))?;
        if !matches!(
            token.kind,
            TokenKind::Identifier | TokenKind::EscapedIdentifier
        ) {
            return Err(self.error("expected an identifier"));
        }
        if self.reserved_identifier(token) && !keywords.contains(&token.text.as_str()) {
            return Err(self.error("reserved keyword cannot be an identifier"));
        }
        let name = Name {
            spelling: token.text.clone(),
            origin: token.origin,
        };
        self.at += 1;
        Ok(name)
    }
    fn attributes(&mut self) -> Result<()> {
        self.expression_machine(expressions::Job::Attributes)?;
        Ok(())
    }
}
