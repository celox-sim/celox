//! Unbound declaration syntax. Names, types, dimensions and defaults retain
//! syntax and source origins; they have not acquired elaboration-time values.

use std::ops::Range;

use crate::Origin;

/// A range in [`crate::Declarations::tokens`], including intervening trivia.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

impl TokenRange {
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Name {
    /// Authored spelling, including an escaped identifier's leading backslash.
    pub spelling: String,
    pub origin: Origin,
}

impl Name {
    pub fn value(&self) -> &str {
        self.spelling.strip_prefix('\\').unwrap_or(&self.spelling)
    }
}

/// Stable expression handle in a declaration syntax arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ExprId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExprKind {
    MinTypMax {
        minimum: ExprId,
        typical: ExprId,
        maximum: ExprId,
    },
    Literal(String),
    Name(Name),
    Type(TypeId),
    Group(ExprId),
    Unary {
        operator: String,
        expression: ExprId,
        postfix: bool,
    },
    Binary {
        left: ExprId,
        operator: String,
        right: ExprId,
    },
    Conditional {
        condition: ExprId,
        then_expression: ExprId,
        else_expression: ExprId,
    },
    /// Parenthesized assignment expression; distinct from a binary value op.
    Assignment {
        left: ExprId,
        operator: String,
        right: ExprId,
    },
    Tagged {
        member: Name,
        value: Option<ExprId>,
    },
    /// `new h`: a shallow copy of the object of handle `h` (8.12).
    Copy(ExprId),
    Member {
        expression: ExprId,
        separator: String,
        name: Name,
    },
    Call {
        expression: ExprId,
        arguments: Vec<Argument>,
    },
    /// Array manipulation method plus its iterator predicate (IEEE 1800-2023 7.12).
    /// The method expression retains optional iterator/index arguments.
    ArrayMethodWith {
        expression: ExprId,
        predicate: ExprId,
    },
    Parameterized {
        expression: ExprId,
        arguments: Vec<Argument>,
    },
    Select {
        expression: ExprId,
        dimension: Box<Dimension>,
    },
    Cast {
        target: ExprId,
        expression: ExprId,
    },
    Concatenation(Vec<ExprId>),
    Replication {
        count: ExprId,
        expressions: Vec<ExprId>,
    },
    Pattern {
        target: Option<ExprId>,
        items: Vec<PatternItem>,
    },
    Set(Vec<Dimension>),
    Streaming {
        operator: String,
        slice: Option<ExprId>,
        expressions: Vec<ExprId>,
    },
    StreamSelect {
        expression: ExprId,
        dimension: Box<Dimension>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Argument {
    pub name: Option<Name>,
    pub value: Option<ExprId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatternItem {
    pub key: Option<ExprId>,
    pub value: ExprId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dimension {
    pub kind: DimensionKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DimensionKind {
    Unsized,
    Index(ExprId),
    Range {
        left: ExprId,
        right: ExprId,
    },
    Indexed {
        base: ExprId,
        operator: String,
        width: ExprId,
    },
    Queue(Option<ExprId>),
    Wildcard,
}

/// Stable type handle in a declaration syntax arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TypeId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataType {
    pub kind: DataTypeKind,
    pub signed: Option<bool>,
    pub packed_dimensions: Vec<Dimension>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataTypeKind {
    Implicit,
    Builtin(String),
    Named(Vec<TypeSegment>),
    Interface {
        name: Option<Name>,
        modport: Option<Name>,
    },
    VirtualInterface {
        name: Name,
        arguments: Option<Vec<Argument>>,
        modport: Option<Name>,
    },
    TypeOf(ExprId),
    Struct {
        union: bool,
        soft: bool,
        tagged: bool,
        packed: bool,
        fields: Vec<Field>,
    },
    Enum {
        base: Option<TypeId>,
        items: Vec<EnumItem>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeSegment {
    pub name: Name,
    pub arguments: Option<Vec<Argument>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RandomQualifier {
    Rand,
    Randc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    pub name: Name,
    pub data_type: TypeId,
    pub random: Option<RandomQualifier>,
    pub dimensions: Vec<Dimension>,
    pub default: Option<ExprId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumItem {
    pub name: Name,
    pub dimensions: Vec<Dimension>,
    pub value: Option<ExprId>,
}

/// Stable identity of a native parameter, independent of source positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ParameterId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    pub id: ParameterId,
    pub name: Name,
    pub local: bool,
    pub kind: ParameterKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParameterKind {
    Type {
        default: Option<TypeId>,
    },
    Value {
        data_type: TypeId,
        dimensions: Vec<Dimension>,
        default: Option<ExprId>,
    },
}

/// Stable handle for authored `assign` syntax. Its enclosing body scope
/// determines whether it is continuous or procedural assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AssignId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriveStrength {
    pub first: String,
    pub second: String,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssignmentDelay {
    pub values: Vec<ExprId>,
    pub parenthesized: bool,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssignmentSyntax {
    pub left: ExprId,
    pub right: ExprId,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssignSyntax {
    pub strength: Option<DriveStrength>,
    pub delay: Option<AssignmentDelay>,
    pub assignments: Vec<AssignmentSyntax>,
    pub tokens: TokenRange,
}

/// Stable handle for an authored parameter/localparam statement in any scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ParameterDeclarationId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterDeclaration {
    pub parameters: Vec<Parameter>,
    pub tokens: TokenRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Input,
    Output,
    Inout,
    Ref,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Port {
    pub kind: PortKind,
    pub constant: bool,
    pub direction: Option<Direction>,
    pub net_type: Option<String>,
    pub variable: bool,
    pub data_type: Option<TypeId>,
    pub dimensions: Vec<Dimension>,
    pub default: Option<ExprId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortKind {
    Named(Name),
    Explicit {
        name: Name,
        expression: Option<ExprId>,
    },
    NonAnsi(Option<ExprId>),
    Wildcard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitKind {
    Module,
    Interface,
    Package,
    Program,
}

impl UnitKind {
    /// A module or a program, which IEEE 1800-2023 24.3 makes a leaf module
    /// with its own scheduling: both elaborate as design units.
    pub fn is_module_like(self) -> bool {
        matches!(self, Self::Module | Self::Program)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortStyle {
    Ansi,
    NonAnsi,
    Wildcard,
}

/// Stable handle for a design unit declared directly in a source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SourceUnitId(pub(crate) usize);

/// Authored source order, including compilation-unit declarations. Payloads
/// reuse the existing flat unit/body arenas; no synthetic package is parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceItem {
    pub kind: SourceItemKind,
    /// Includes any declaration attributes preceding the payload.
    pub tokens: TokenRange,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SourceItemKind {
    Unit(SourceUnitId),
    Declaration(BodyItemId),
}

/// Stable nested-unit handle; roots live in `Declarations::units`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct UnitId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeValue {
    pub magnitude: String,
    pub unit: String,
    pub tokens: TokenRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TimeDeclarationId(pub(crate) usize);

/// One authored timeunit/timeprecision statement; adjacent statements remain
/// separate rather than inheriting an external parser's grouping convention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeDeclaration {
    pub unit: Option<TimeValue>,
    pub precision: Option<TimeValue>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeDirectiveKind {
    Scale {
        unit: TimeValue,
        precision: TimeValue,
    },
    Reset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeDirective {
    pub kind: TimeDirectiveKind,
    pub tokens: TokenRange,
}

/// Stable body/compilation-unit typedef declaration handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TypeAliasId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardType {
    Enum,
    Struct,
    Union,
    Class,
    InterfaceClass,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeAliasTarget {
    DataType(TypeId),
    Forward(Option<ForwardType>),
    InterfaceMember(ExprId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeAlias {
    pub name: Name,
    pub target: TypeAliasTarget,
    pub dimensions: Vec<Dimension>,
    pub tokens: TokenRange,
}

/// Authored interface modport declaration with native member/header ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ModportDeclarationId(pub(crate) usize);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModportDeclaration {
    pub modports: Vec<Modport>,
    pub tokens: TokenRange,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Modport {
    pub name: Name,
    pub ports: Vec<ModportPorts>,
    pub tokens: TokenRange,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModportPorts {
    Members {
        direction: Direction,
        ports: Vec<ModportMember>,
    },
    Subroutines {
        export: bool,
        ports: Vec<ModportSubroutine>,
    },
    Clocking(Name),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModportMember {
    Named(Name),
    Expression {
        name: Name,
        expression: Option<ExprId>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModportSubroutine {
    Named(Name),
    /// Explicit prototypes remain a distinct unsupported interface contract.
    Prototype {
        kind: SubroutineKind,
        return_type: Option<TypeId>,
        name: Name,
        ports: Option<Vec<SubroutinePort>>,
        tokens: TokenRange,
    },
}

/// A package import/export declaration, independent of its enclosing scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PackageBindingId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageBindingKind {
    Import,
    Export,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageBinding {
    pub kind: PackageBindingKind,
    pub items: Vec<PackageBindingItem>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageBindingItem {
    pub kind: PackageBindingItemKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageBindingItemKind {
    /// `export *::*;`
    All,
    Wildcard(Name),
    Named {
        package: Name,
        name: Name,
    },
}

/// An import/export prototype in the declaration arena, including declarations
/// inside generate scopes. Elaboration selects which declarations are active.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DpiId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpiKind {
    Function,
    Task,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpiProperty {
    Pure,
    Context,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpiPort {
    pub name: Option<Name>,
    pub direction: Option<Direction>,
    pub constant: bool,
    pub variable: bool,
    pub data_type: TypeId,
    pub dimensions: Vec<Dimension>,
    pub default: Option<ExprId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpiDeclaration {
    pub import: bool,
    /// String literal spelling; the full grammar validates the DPI specifier.
    pub specifier: String,
    pub property: Option<DpiProperty>,
    pub c_name: Option<Name>,
    pub kind: DpiKind,
    pub return_type: Option<TypeId>,
    pub name: Name,
    pub ports: Vec<DpiPort>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unit {
    pub kind: UnitKind,
    pub name: Name,
    pub lifetime: Option<String>,
    pub external: bool,
    pub parameters: Option<Vec<Parameter>>,
    pub ports: Vec<Port>,
    pub port_style: PortStyle,
    pub imports: Vec<TokenRange>,
    pub header: TokenRange,
    /// Authored body range. Unconverted item classes still require full validation.
    pub body: TokenRange,
    /// Direct-scope projection used for positional/header metadata only.
    pub body_parameters: Vec<Parameter>,
    /// Authored parameter statements throughout this unit's body scopes.
    pub parameter_declarations: Vec<ParameterDeclarationId>,
    pub assign_statements: Vec<AssignId>,
    pub data_declarations: Vec<DataDeclarationId>,
    pub net_declarations: Vec<NetDeclarationId>,
    pub instantiations: Vec<InstantiationId>,
    pub items: Vec<BodyItemId>,
    pub procedures: Vec<ProcedureId>,
    pub subroutines: Vec<SubroutineId>,
    pub children: Vec<UnitId>,
    pub dpi_declarations: Vec<DpiId>,
    pub package_bindings: Vec<PackageBindingId>,
    pub type_aliases: Vec<TypeAliasId>,
    pub time_declarations: Vec<TimeDeclarationId>,
    pub end_label: Option<Name>,
    pub tokens: TokenRange,
}

impl Unit {
    /// Parameters that can be bound positionally in an instance, in declaration
    /// order. Even an empty parameter-port list makes body parameters local.
    pub fn overridable_parameters(&self) -> impl Iterator<Item = &Parameter> {
        self.parameters
            .as_ref()
            .unwrap_or(&self.body_parameters)
            .iter()
            .filter(|parameter| !parameter.local)
    }

    /// Interface-typed header ports are a component-scheduling dependency.
    /// Explicit direction/net/variable declarations denote data ports instead.
    pub fn interface_references<'a>(
        &'a self,
        declarations: &'a crate::Declarations,
    ) -> impl Iterator<Item = &'a Name> {
        self.ports.iter().filter_map(|port| {
            if port.direction.is_some() || port.net_type.is_some() || port.variable {
                return None;
            }
            match &declarations.data_type(port.data_type?).kind {
                DataTypeKind::Interface { name, .. } => name.as_ref(),
                DataTypeKind::Named(path) if path.len() == 1 => {
                    path.first().map(|segment| &segment.name)
                }
                _ => None,
            }
        })
    }
}

/// A design-unit/generate body item in a flat syntax arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BodyItemId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyItem {
    pub kind: BodyItemKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerateCaseArm {
    /// Empty for a default item.
    pub labels: Vec<ExprId>,
    pub colon: bool,
    pub body: BodyItemId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyItemKind {
    Data(DataDeclarationId),
    Net(NetDeclarationId),
    Instance(InstantiationId),
    Dpi(DpiId),
    Parameter(ParameterDeclarationId),
    Assign(AssignId),
    Procedure(ProcedureId),
    Subroutine(SubroutineId),
    TypeAlias(TypeAliasId),
    PackageBinding(PackageBindingId),
    Modport(ModportDeclarationId),
    Time(TimeDeclarationId),
    Unit(UnitId),
    Region(Vec<BodyItemId>),
    Block {
        leading_label: Option<Name>,
        label: Option<Name>,
        end_label: Option<Name>,
        items: Vec<BodyItemId>,
    },
    Conditional {
        condition: ExprId,
        then_body: BodyItemId,
        otherwise: Option<BodyItemId>,
    },
    Case {
        selector: ExprId,
        arms: Vec<GenerateCaseArm>,
    },
    Loop {
        declared: bool,
        variable: Name,
        initial: ExprId,
        condition: ExprId,
        step: ForStep,
        body: BodyItemId,
    },
    /// An explicitly unconverted body-item grammar class, not a validation claim.
    Unconverted {
        category: String,
        /// Captured native body ownership inside an otherwise unconverted
        /// scope. This does not claim validation or semantics for that scope.
        items: Vec<BodyItemId>,
    },
}

/// Authored design-unit data declaration in the flat syntax arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DataDeclarationId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataDeclaration {
    Variables(VariableDeclaration),
    /// Parsed constructed declarations retain ordinary supported siblings.
    Constructed(VariableDeclaration),
    /// Constructed/dynamic variable declarations remain typed forms.
    Unconverted(TokenRange),
}

/// Authored ordinary net declaration, independent of semantic name binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct NetDeclarationId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetStrength {
    Drive(DriveStrength),
    Charge { name: String, tokens: TokenRange },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetDeclaration {
    pub net_type: String,
    pub strength: Option<NetStrength>,
    pub vector: Option<String>,
    pub data_type: TypeId,
    pub delay: Option<AssignmentDelay>,
    pub variables: Vec<VariableDeclarator>,
    pub tokens: TokenRange,
}

/// An authored module/interface instantiation. Whether the name denotes a
/// module, interface or a type alias belongs to semantic binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct InstantiationId(pub(crate) usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instantiation {
    pub name: Name,
    pub parameters: Option<Vec<Argument>>,
    pub instances: Vec<InstanceDeclarator>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceDeclarator {
    pub name: Name,
    pub dimensions: Vec<Dimension>,
    pub connections: Vec<InstanceConnection>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionValue {
    Implicit,
    Empty,
    Expression(ExprId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceConnectionKind {
    Ordered(Option<ExprId>),
    Named { name: Name, value: ConnectionValue },
    Wildcard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceConnection {
    pub kind: InstanceConnectionKind,
    pub tokens: TokenRange,
}

/// A procedural body in the flat native statement arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct StatementId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProcedureId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcedureKind {
    Initial,
    Always,
    AlwaysComb,
    AlwaysLatch,
    AlwaysFf,
    Final,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TimingControlId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingEdge {
    Any,
    Pos,
    Neg,
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventItem {
    pub edge: TimingEdge,
    pub expression: ExprId,
    pub iff: Option<ExprId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventControl {
    Wildcard,
    Named(ExprId),
    Items(Vec<EventItem>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimingControlKind {
    Delay { value: ExprId, parenthesized: bool },
    Event(EventControl),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimingControl {
    pub kind: TimingControlKind,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Procedure {
    pub kind: ProcedureKind,
    pub body: StatementId,
    /// Authored body membership, independent of transformed execution children.
    /// Opaque statements own their captured descendants separately.
    pub authored_statements: Vec<StatementId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredicateItem {
    pub expression: ExprId,
    /// Pattern grammar is retained explicitly until its dedicated parser moves.
    pub pattern: Option<TokenRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Predicate {
    pub items: Vec<PredicateItem>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalBranch {
    pub predicate: Predicate,
    pub body: StatementId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaseMatch {
    Expression(ExprId),
    Range {
        left: ExprId,
        right: ExprId,
    },
    /// Tagged pattern syntax remains an explicit unconverted grammar leaf.
    Pattern(TokenRange),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseArm {
    pub matches: Vec<CaseMatch>,
    pub default: bool,
    pub body: StatementId,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoopSyntax {
    Forever,
    Repeat(ExprId),
    While(ExprId),
    DoWhile(ExprId),
    For(ForHeader),
    Foreach(ForeachHeader),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeachHeader {
    pub array: ExprId,
    /// Slots retain their dimension positions, including empty/trailing slots.
    pub variables: Vec<Option<Name>>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForVariable {
    pub name: Name,
    pub value: ExprId,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForVariableDeclaration {
    pub var: bool,
    pub data_type: TypeId,
    pub variables: Vec<ForVariable>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForInitialization {
    Assignments(Vec<AssignmentSyntax>),
    Declarations(Vec<ForVariableDeclaration>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForStep {
    Assignment {
        left: ExprId,
        operator: String,
        right: ExprId,
        tokens: TokenRange,
    },
    Expression(ExprId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForHeader {
    pub initialization: Option<ForInitialization>,
    pub condition: Option<ExprId>,
    pub steps: Vec<ForStep>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalDeclaration {
    Parameters(ParameterDeclarationId),
    TypeAlias(TypeAliasId),
    PackageBinding(PackageBindingId),
    Time(TimeDeclarationId),
    Variables(VariableDeclaration),
    /// Allocation/dynamic variables remain unsupported; ordinary siblings bind.
    Constructed(VariableDeclaration),
    /// Let/nettype and constructed/dynamic variable forms remain typed leaves.
    Unconverted(TokenRange),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableDeclarator {
    pub name: Name,
    pub dimensions: Vec<Dimension>,
    pub value: Option<ExprId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableDeclaration {
    pub constant: bool,
    pub var: bool,
    pub lifetime: Option<String>,
    pub data_type: TypeId,
    pub variables: Vec<VariableDeclarator>,
    pub tokens: TokenRange,
}

/// A class declaration (IEEE 1800-2023 8.3). Its body also remains an
/// unconverted body item covering the same tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassDeclaration {
    pub name: Name,
    /// `virtual class`: abstract (8.21).
    pub virtual_class: bool,
    /// `interface class` (8.26).
    pub interface: bool,
    /// `extends` of a class, not of an interface class.
    pub base: Option<ClassBase>,
    /// The interface classes of `implements`, or those an interface class
    /// extends (8.26).
    pub interfaces: Vec<TypeId>,
    /// Whether the class declares constraints, including prototypes.
    pub has_constraints: bool,
    /// The parameter port list `#(...)`, after which the parameters the
    /// body declares are local (8.25).
    pub parameter_ports: Option<ParameterDeclarationId>,
    /// The `parameter` and `localparam` declarations of the body.
    pub parameters: Vec<ParameterDeclarationId>,
    /// The `typedef` declarations of the body.
    pub type_aliases: Vec<TypeAliasId>,
    pub properties: Vec<ClassProperty>,
    pub methods: Vec<ClassMethod>,
    pub tokens: TokenRange,
}

/// The class a class extends, with the arguments of its constructor when
/// they are given in the `extends` clause (8.17).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassBase {
    pub data_type: TypeId,
    pub arguments: Option<Vec<Argument>>,
}

/// Qualifiers of a class item (8.3, 8.18, 8.20).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClassQualifiers {
    pub is_static: bool,
    pub is_virtual: bool,
    pub is_pure: bool,
    pub is_extern: bool,
    pub local: bool,
    pub protected: bool,
    pub rand: bool,
    pub randc: bool,
}

/// A property declaration of a class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassProperty {
    pub qualifiers: ClassQualifiers,
    pub declaration: VariableDeclaration,
}

/// A method of a class: a function, a task or the constructor `new`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassMethod {
    pub qualifiers: ClassQualifiers,
    pub subroutine: SubroutineId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SubroutineId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubroutineKind {
    Function,
    Task,
    Constructor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubroutineScope {
    pub name: Name,
    pub arguments: Option<Vec<Argument>>,
    pub separator: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One authored formal. Absent direction/type are deliberately retained:
/// direction and datatype inheritance belongs to semantic declaration binding.
pub struct SubroutinePort {
    pub name: Option<Name>,
    pub direction: Option<Direction>,
    pub constant: bool,
    pub static_reference: bool,
    pub variable: bool,
    pub data_type: TypeId,
    pub dimensions: Vec<Dimension>,
    pub default: Option<ExprId>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubroutinePortDeclaration {
    pub ports: Vec<SubroutinePort>,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A function/task declaration or extern/pure prototype. Statements and types
/// use flat arenas, including ordered declaration/body items. Constructors are
/// a separate class; prototype bodies are empty. Scope separators/arguments and
/// physical/macro origins remain authored syntax.
pub struct Subroutine {
    pub kind: SubroutineKind,
    pub name: Name,
    pub scope: Vec<SubroutineScope>,
    pub lifetime: Option<String>,
    pub return_type: Option<TypeId>,
    pub ports: Option<Vec<SubroutinePort>>,
    pub port_declarations: Vec<SubroutinePortDeclaration>,
    /// Declarations and statements in authored order, with flat ownership.
    pub items: Vec<StatementId>,
    /// Authored body membership retained when execution roots/children change.
    /// Opaque statements own their captured descendants separately.
    pub authored_statements: Vec<StatementId>,
    pub prototype: bool,
    pub end_label: Option<Name>,
    pub body_tokens: TokenRange,
    pub tokens: TokenRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssertionHeader {
    Immediate {
        condition: ExprId,
        deferred: Option<AssertionDeferral>,
        tokens: TokenRange,
    },
    /// Concurrent/property/sequence and expect headers retain an explicit
    /// grammar boundary; consumers must not treat them as immediate predicates.
    Unconverted(TokenRange),
}

impl AssertionHeader {
    pub fn tokens(&self) -> TokenRange {
        match self {
            Self::Immediate { tokens, .. } | Self::Unconverted(tokens) => *tokens,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssertionDeferral {
    Zero,
    Final,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementKind {
    Empty,
    Block {
        label: Option<Name>,
        end_label: Option<Name>,
        items: Vec<StatementId>,
    },
    Parallel {
        label: Option<Name>,
        end_label: Option<Name>,
        join: String,
        items: Vec<StatementId>,
    },
    Conditional {
        branches: Vec<ConditionalBranch>,
        otherwise: Option<StatementId>,
    },
    Case {
        keyword: String,
        mode: Option<String>,
        expression: Option<ExprId>,
        arms: Vec<CaseArm>,
    },
    Loop {
        kind: LoopSyntax,
        body: StatementId,
    },
    Assignment {
        left: ExprId,
        operator: String,
        right: ExprId,
        nonblocking: bool,
        timing: Option<TokenRange>,
        /// Native event payload within an otherwise unsupported intra-assignment
        /// control. Delay/cycle and repeat-count syntax retains its token range.
        event_control: Option<TimingControlId>,
    },
    Expression(ExprId),
    Declaration(LocalDeclaration),
    ProceduralAssign(AssignId),
    Return(Option<ExprId>),
    Break,
    Continue,
    Timing {
        control: TimingControlId,
        body: StatementId,
    },
    Wait {
        condition: ExprId,
        body: StatementId,
    },
    WaitFork,
    WaitOrder {
        header: TokenRange,
        success: Option<StatementId>,
        failure: Option<StatementId>,
    },
    Assertion {
        keyword: String,
        header: AssertionHeader,
        success: Option<StatementId>,
        failure: Option<StatementId>,
    },
    Disable(TokenRange),
    EventTrigger(TokenRange),
    ProceduralRelease(TokenRange),
    Unconverted {
        category: String,
        tokens: TokenRange,
        /// Captured embedded statements, without semantics for the container.
        items: Vec<StatementId>,
    },
}

impl StatementKind {
    /// Immediate child statement handles in authored order. Headers and value
    /// expressions remain separate arenas; traversal does not recurse.
    pub fn children(&self) -> impl Iterator<Item = StatementId> + '_ {
        let mut sequence: &[StatementId] = &[];
        let mut branches: &[ConditionalBranch] = &[];
        let mut cases: &[CaseArm] = &[];
        let mut direct = [None, None];
        match self {
            Self::Block { items, .. }
            | Self::Parallel { items, .. }
            | Self::Unconverted { items, .. } => sequence = items,
            Self::Conditional {
                branches: items,
                otherwise,
            } => {
                branches = items;
                direct[0] = *otherwise;
            }
            Self::Case { arms, .. } => cases = arms,
            Self::Loop { body, .. } | Self::Timing { body, .. } | Self::Wait { body, .. } => {
                direct[0] = Some(*body)
            }
            Self::Assertion {
                success, failure, ..
            }
            | Self::WaitOrder {
                success, failure, ..
            } => direct = [*success, *failure],
            _ => {}
        }
        sequence
            .iter()
            .copied()
            .chain(branches.iter().map(|branch| branch.body))
            .chain(cases.iter().map(|arm| arm.body))
            .chain(direct.into_iter().flatten())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Statement {
    /// Statement label before attributes/item, separate from a begin-block label.
    pub label: Option<Name>,
    pub attributes: Option<TokenRange>,
    pub qualifier: Option<String>,
    pub kind: StatementKind,
    pub tokens: TokenRange,
}
