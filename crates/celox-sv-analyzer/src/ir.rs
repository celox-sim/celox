//! Analyzer IR produced after SystemVerilog semantic analysis.

use crate::{ast, symbol::ModuleId, typecheck};

pub use crate::procedural::{
    CaseKind, CaseLabel, LoopKind, ParamDirection, StmtBase, SystemTaskArg,
};

/// A procedural statement of the analyzed IR.
pub type Stmt = StmtBase<Expr, LValue>;
pub type CaseItem = crate::procedural::CaseItemBase<Expr, LValue>;
pub type LocalVariable = crate::procedural::LocalVariableBase<Type>;
pub type Subroutine = crate::procedural::SubroutineBase<Expr, LValue, Type>;
pub type SubroutineParam = crate::procedural::SubroutineParamBase<Expr, Type>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ir {
    modules: Vec<Module>,
}

impl Ir {
    pub(crate) fn new(modules: Vec<Module>) -> Self {
        Self { modules }
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    id: ModuleId,
    name: String,
    parameters: Vec<Parameter>,
    ports: Vec<Port>,
    signals: Vec<Signal>,
    instances: Vec<Instance>,
    assignments: Vec<Assignment>,
    comb_processes: Vec<CombProcess>,
    ff_processes: Vec<FfProcess>,
    initial_processes: Vec<InitialProcess>,
    locals: Vec<LocalVariable>,
    subroutines: Vec<Subroutine>,
    dpi_imports: Vec<DpiImport>,
}

impl Module {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: ModuleId,
        name: String,
        parameters: Vec<Parameter>,
        ports: Vec<Port>,
        signals: Vec<Signal>,
        instances: Vec<Instance>,
        assignments: Vec<Assignment>,
        comb_processes: Vec<CombProcess>,
        ff_processes: Vec<FfProcess>,
        initial_processes: Vec<InitialProcess>,
        locals: Vec<LocalVariable>,
        subroutines: Vec<Subroutine>,
    ) -> Self {
        Self {
            id,
            name,
            parameters,
            ports,
            signals,
            instances,
            assignments,
            comb_processes,
            ff_processes,
            initial_processes,
            locals,
            subroutines,
            dpi_imports: Vec::new(),
        }
    }

    pub(crate) fn with_dpi_imports(mut self, dpi_imports: Vec<DpiImport>) -> Self {
        self.dpi_imports = dpi_imports;
        self
    }

    /// Functions imported from C through DPI-C.
    pub fn dpi_imports(&self) -> &[DpiImport] {
        &self.dpi_imports
    }

    /// `initial` processes, which run once at the start of simulation.
    pub fn initial_processes(&self) -> &[InitialProcess] {
        &self.initial_processes
    }

    /// Variables declared inside procedural blocks and subroutines.
    pub fn locals(&self) -> &[LocalVariable] {
        &self.locals
    }

    /// Functions and tasks, with their statement bodies.
    pub fn subroutines(&self) -> &[Subroutine] {
        &self.subroutines
    }

    pub fn id(&self) -> ModuleId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    pub fn comb_processes(&self) -> &[CombProcess] {
        &self.comb_processes
    }

    pub fn ff_processes(&self) -> &[FfProcess] {
        &self.ff_processes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    name: String,
    value: Option<ConstExpr>,
    resolved_value: Option<i128>,
    resolved_width: Option<usize>,
    resolved_signed: Option<bool>,
    declared_width: Option<usize>,
    declared_signed: Option<bool>,
}

impl Parameter {
    pub(crate) fn new(
        name: String,
        value: Option<ConstExpr>,
        resolved_value: Option<i128>,
        resolved_width: Option<usize>,
        resolved_signed: Option<bool>,
        declared_width: Option<usize>,
        declared_signed: Option<bool>,
    ) -> Self {
        Self {
            name,
            value,
            resolved_value,
            resolved_width,
            resolved_signed,
            declared_width,
            declared_signed,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub fn resolved_value(&self) -> Option<i128> {
        self.resolved_value
    }

    pub fn resolved_width(&self) -> Option<usize> {
        self.resolved_width
    }

    pub fn resolved_signed(&self) -> Option<bool> {
        self.resolved_signed
    }

    pub fn declared_width(&self) -> Option<usize> {
        self.declared_width
    }

    pub fn declared_signed(&self) -> Option<bool> {
        self.declared_signed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    name: String,
    direction: PortDirection,
    r#type: Type,
    is_net: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    name: String,
    r#type: Type,
    is_net: bool,
}

impl Signal {
    pub(crate) fn new(name: String, r#type: Type, is_net: bool) -> Self {
        Self {
            name,
            r#type,
            is_net,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub fn is_net(&self) -> bool {
        self.is_net
    }
}

impl Port {
    pub(crate) fn new(name: String, direction: PortDirection, r#type: Type, is_net: bool) -> Self {
        Self {
            name,
            direction,
            r#type,
            is_net,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn direction(&self) -> PortDirection {
        self.direction
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub fn is_net(&self) -> bool {
        self.is_net
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    module_name: String,
    name: String,
    parameter_names: Vec<String>,
    parameter_overrides: Vec<ParameterOverride>,
    condition: Option<ConstExpr>,
    port_names: Vec<String>,
    port_connections: Vec<PortConnection>,
    /// The declared `[left:right]` bounds of an instance array.
    array_range: Option<(i128, i128)>,
}

impl Instance {
    pub(crate) fn new(
        module_name: String,
        name: String,
        parameter_names: Vec<String>,
        parameter_overrides: Vec<ParameterOverride>,
        condition: Option<ConstExpr>,
        port_names: Vec<String>,
        port_connections: Vec<PortConnection>,
        array_range: Option<(i128, i128)>,
    ) -> Self {
        Self {
            module_name,
            name,
            parameter_names,
            parameter_overrides,
            condition,
            port_names,
            port_connections,
            array_range,
        }
    }

    /// The number of elements of an instance array, if this is one.
    pub fn array_len(&self) -> Option<usize> {
        self.array_range
            .and_then(|(left, right)| usize::try_from(left.abs_diff(right)).ok()?.checked_add(1))
    }

    /// The declared `[left:right]` bounds of an instance array, if this is one.
    pub fn array_range(&self) -> Option<(i128, i128)> {
        self.array_range
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn parameter_names(&self) -> &[String] {
        &self.parameter_names
    }

    pub fn parameter_overrides(&self) -> &[ParameterOverride] {
        &self.parameter_overrides
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn port_names(&self) -> &[String] {
        &self.port_names
    }

    pub fn port_connections(&self) -> &[PortConnection] {
        &self.port_connections
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterOverride {
    name: String,
    value: Option<ConstExpr>,
    type_text: Option<String>,
}

impl ParameterOverride {
    pub(crate) fn new(name: String, value: Option<ConstExpr>, type_text: Option<String>) -> Self {
        Self {
            name,
            value,
            type_text,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    /// The source text of the data type bound to a `parameter type`.
    pub fn type_text(&self) -> Option<&str> {
        self.type_text.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConnection {
    formal: String,
    actual: String,
    actual_expr: Option<Expr>,
}

impl PortConnection {
    pub(crate) fn new(formal: String, actual: String, actual_expr: Option<Expr>) -> Self {
        Self {
            formal,
            actual,
            actual_expr,
        }
    }

    pub fn formal(&self) -> &str {
        &self.formal
    }

    pub fn actual(&self) -> &str {
        &self.actual
    }

    pub fn actual_expr(&self) -> Option<&Expr> {
        self.actual_expr.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
    Inout,
    Ref,
    Unspecified,
}

impl From<ast::PortDirection> for PortDirection {
    fn from(direction: ast::PortDirection) -> Self {
        match direction {
            ast::PortDirection::Input => PortDirection::Input,
            ast::PortDirection::Output => PortDirection::Output,
            ast::PortDirection::Inout => PortDirection::Inout,
            ast::PortDirection::Ref => PortDirection::Ref,
            ast::PortDirection::Unspecified => PortDirection::Unspecified,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    kind: TypeKind,
    is_signed: bool,
    packed_ranges: Vec<PackedRange>,
    unpacked_ranges: Vec<UnpackedRange>,
    resolved_width: Option<usize>,
}

impl Type {
    pub fn kind(&self) -> TypeKind {
        self.kind
    }

    pub fn is_signed(&self) -> bool {
        self.is_signed
    }

    pub fn packed_ranges(&self) -> &[PackedRange] {
        &self.packed_ranges
    }

    pub fn unpacked_ranges(&self) -> &[UnpackedRange] {
        &self.unpacked_ranges
    }

    pub fn resolved_width(&self) -> Option<usize> {
        self.resolved_width
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Bit,
    Logic,
    Reg,
    Implicit,
}

impl Type {
    pub(crate) fn from_ast(r#type: ast::Type, constants: &fxhash::FxHashMap<String, i128>) -> Self {
        let kind = r#type.kind().into();
        let is_signed = r#type.is_signed();
        let (packed_ranges, unpacked_ranges, resolved_width) = convert_type(r#type, constants);
        Self {
            kind,
            is_signed,
            packed_ranges,
            unpacked_ranges,
            resolved_width,
        }
    }
}

fn convert_type(
    r#type: ast::Type,
    constants: &fxhash::FxHashMap<String, i128>,
) -> (Vec<PackedRange>, Vec<UnpackedRange>, Option<usize>) {
    let packed_ranges: Vec<_> = r#type
        .packed_ranges()
        .iter()
        .map(|range| PackedRange::new(range.left().clone().into(), range.right().clone().into()))
        .collect();
    let unpacked_ranges: Vec<_> = r#type
        .unpacked_ranges()
        .iter()
        .map(|range| UnpackedRange::new(range.left().clone().into(), range.right().clone().into()))
        .collect();
    let packed_width = typecheck::resolve_packed_width_with_env(&packed_ranges, constants);
    let unpacked_width = unpacked_ranges.iter().try_fold(1usize, |acc, range| {
        let left = typecheck::eval_const_expr(range.left(), constants)?;
        let right = typecheck::eval_const_expr(range.right(), constants)?;
        let width = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
        acc.checked_mul(width)
    });
    let resolved_width = packed_width.and_then(|width| width.checked_mul(unpacked_width?));
    (packed_ranges, unpacked_ranges, resolved_width)
}

impl From<ast::TypeKind> for TypeKind {
    fn from(kind: ast::TypeKind) -> Self {
        match kind {
            ast::TypeKind::Bit => TypeKind::Bit,
            ast::TypeKind::Logic => TypeKind::Logic,
            ast::TypeKind::Reg => TypeKind::Reg,
            ast::TypeKind::Implicit => TypeKind::Implicit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedRange {
    left: ConstExpr,
    right: ConstExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackedRange {
    left: ConstExpr,
    right: ConstExpr,
}

impl UnpackedRange {
    pub(crate) fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self { left, right }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }
}

impl PackedRange {
    pub(crate) fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self { left, right }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstExpr {
    Literal(String),
    Ident(String),
    Select {
        expr: Box<ConstExpr>,
        bit: Box<ConstExpr>,
    },
    Function {
        name: String,
        args: Vec<ConstExpr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<ConstExpr>,
    },
    Binary {
        left: Box<ConstExpr>,
        op: BinaryOp,
        right: Box<ConstExpr>,
    },
    Mux {
        condition: Box<ConstExpr>,
        then_expr: Box<ConstExpr>,
        else_expr: Box<ConstExpr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Plus,
    Minus,
    BitNot,
    LogicNot,
    ToTwoState,
    RedAnd,
    RedOr,
    RedXor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    Sar,
    BitAnd,
    BitOr,
    BitXor,
    LogicAnd,
    LogicOr,
    Eq,
    Ne,
    EqCase,
    NeCase,
    EqWildcard,
    NeWildcard,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    lhs: LValue,
    rhs: Expr,
}

impl Assignment {
    pub(crate) fn new(lhs: LValue, rhs: Expr) -> Self {
        Self { lhs, rhs }
    }

    pub fn lhs(&self) -> &str {
        self.lhs.name()
    }

    pub fn lhs_value(&self) -> &LValue {
        &self.lhs
    }

    pub fn rhs(&self) -> &Expr {
        &self.rhs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LValue {
    Ident(String),
    Select {
        name: String,
        msb: ConstExpr,
        lsb: ConstExpr,
        array_slice_width: Option<ConstExpr>,
        array_slice_reversed: bool,
    },
}

impl LValue {
    pub fn name(&self) -> &str {
        match self {
            LValue::Ident(name) | LValue::Select { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombProcess {
    kind: CombProcessKind,
    condition: Option<ConstExpr>,
    assignments: Vec<Assignment>,
    body: Vec<Stmt>,
}

impl CombProcess {
    pub(crate) fn new(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        assignments: Vec<Assignment>,
        body: Vec<Stmt>,
    ) -> Self {
        Self {
            kind,
            condition,
            assignments,
            body,
        }
    }

    /// The statements of an `always_comb` (or `always @*`) process. A
    /// continuous assignment has one blocking assignment statement.
    pub fn body(&self) -> &[Stmt] {
        &self.body
    }

    pub fn kind(&self) -> CombProcessKind {
        self.kind
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombProcessKind {
    ContinuousAssign,
    AlwaysComb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfProcess {
    events: Vec<FfEvent>,
    body: Vec<Stmt>,
}

impl FfProcess {
    pub(crate) fn new(events: Vec<FfEvent>, body: Vec<Stmt>) -> Self {
        Self { events, body }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

/// An `initial` process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialProcess {
    condition: Option<ConstExpr>,
    body: Vec<Stmt>,
}

impl InitialProcess {
    pub(crate) fn new(condition: Option<ConstExpr>, body: Vec<Stmt>) -> Self {
        Self { condition, body }
    }

    /// The condition of the enclosing conditional generate block, if any.
    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfEdge {
    Pos,
    Neg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfEvent {
    edge: FfEdge,
    signal: String,
}

impl FfEvent {
    pub(crate) fn new(edge: FfEdge, signal: String) -> Self {
        Self { edge, signal }
    }

    pub fn edge(&self) -> FfEdge {
        self.edge
    }

    pub fn signal(&self) -> &str {
        &self.signal
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    Literal(String),
    Select {
        expr: Box<Expr>,
        msb: ConstExpr,
        lsb: ConstExpr,
        signed: bool,
    },
    Concat(Vec<Expr>),
    RepeatConcat {
        count: ConstExpr,
        parts: Vec<Expr>,
    },
    Resize {
        expr: Box<Expr>,
        width: usize,
        signed: bool,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Mux {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// `expr inside { items }`: true when `expr` matches any item.
    Inside {
        expr: Box<Expr>,
        items: Vec<InsideItem>,
    },
}

/// One item of an `inside` set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsideItem {
    /// A value matched with wildcard equality (`==?`).
    Value(Expr),
    /// An inclusive range `[low:high]`.
    Range { low: Expr, high: Expr },
}

impl InsideItem {
    /// The operand expressions of the item.
    pub fn exprs(&self) -> Vec<&Expr> {
        match self {
            InsideItem::Value(value) => vec![value],
            InsideItem::Range { low, high } => vec![low, high],
        }
    }

    /// Rebuild the item with `f` applied to each operand.
    pub fn map(&self, f: &mut impl FnMut(&Expr) -> Expr) -> InsideItem {
        match self {
            InsideItem::Value(value) => InsideItem::Value(f(value)),
            InsideItem::Range { low, high } => InsideItem::Range {
                low: f(low),
                high: f(high),
            },
        }
    }
}

impl From<ast::InsideItem> for InsideItem {
    fn from(item: ast::InsideItem) -> Self {
        match item {
            ast::InsideItem::Value(value) => InsideItem::Value(value.into()),
            ast::InsideItem::Range { low, high } => InsideItem::Range {
                low: low.into(),
                high: high.into(),
            },
        }
    }
}

impl From<ast::ConstExpr> for ConstExpr {
    fn from(expr: ast::ConstExpr) -> Self {
        match expr {
            ast::ConstExpr::Literal(value) => ConstExpr::Literal(value),
            ast::ConstExpr::Ident(value) => ConstExpr::Ident(value),
            ast::ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new((*expr).into()),
                bit: Box::new((*bit).into()),
            },
            ast::ConstExpr::Function { name, args } => ConstExpr::Function {
                name,
                args: args.into_iter().map(Into::into).collect(),
            },
            ast::ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: op.into(),
                expr: Box::new((*expr).into()),
            },
            ast::ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new((*left).into()),
                op: op.into(),
                right: Box::new((*right).into()),
            },
            ast::ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new((*condition).into()),
                then_expr: Box::new((*then_expr).into()),
                else_expr: Box::new((*else_expr).into()),
            },
        }
    }
}

impl From<ConstExpr> for ast::ConstExpr {
    fn from(expr: ConstExpr) -> Self {
        match expr {
            ConstExpr::Literal(value) => ast::ConstExpr::Literal(value),
            ConstExpr::Ident(value) => ast::ConstExpr::Ident(value),
            ConstExpr::Select { expr, bit } => ast::ConstExpr::Select {
                expr: Box::new((*expr).into()),
                bit: Box::new((*bit).into()),
            },
            ConstExpr::Function { name, args } => ast::ConstExpr::Function {
                name,
                args: args.into_iter().map(Into::into).collect(),
            },
            ConstExpr::Unary { op, expr } => ast::ConstExpr::Unary {
                op: op.into(),
                expr: Box::new((*expr).into()),
            },
            ConstExpr::Binary { left, op, right } => ast::ConstExpr::Binary {
                left: Box::new((*left).into()),
                op: op.into(),
                right: Box::new((*right).into()),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ast::ConstExpr::Mux {
                condition: Box::new((*condition).into()),
                then_expr: Box::new((*then_expr).into()),
                else_expr: Box::new((*else_expr).into()),
            },
        }
    }
}

impl From<ast::Assignment> for Assignment {
    fn from(assignment: ast::Assignment) -> Self {
        Assignment::new(
            assignment.lhs_value().clone().into(),
            assignment.rhs().clone().into(),
        )
    }
}

impl From<ast::LValue> for LValue {
    fn from(value: ast::LValue) -> Self {
        match value {
            ast::LValue::Ident(name) => LValue::Ident(name),
            ast::LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
                ..
            } => LValue::Select {
                name,
                msb: msb.into(),
                lsb: lsb.into(),
                array_slice_width: array_slice_width.map(Into::into),
                array_slice_reversed,
            },
        }
    }
}

impl From<ast::CombProcess> for CombProcess {
    fn from(process: ast::CombProcess) -> Self {
        CombProcess::new(
            process.kind().into(),
            process.condition().cloned().map(Into::into),
            process
                .assignments()
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            process
                .body()
                .iter()
                .cloned()
                .map(|stmt| stmt.map(&mut Into::into, &mut Into::into))
                .collect(),
        )
    }
}

impl From<ast::CombProcessKind> for CombProcessKind {
    fn from(kind: ast::CombProcessKind) -> Self {
        match kind {
            ast::CombProcessKind::ContinuousAssign => CombProcessKind::ContinuousAssign,
            ast::CombProcessKind::AlwaysComb => CombProcessKind::AlwaysComb,
        }
    }
}

impl From<ast::FfProcess> for FfProcess {
    fn from(process: ast::FfProcess) -> Self {
        Self::new(
            process.events().iter().cloned().map(Into::into).collect(),
            process
                .body()
                .iter()
                .cloned()
                .map(|stmt| stmt.map(&mut Into::into, &mut Into::into))
                .collect(),
        )
    }
}

impl From<ast::InitialProcess> for InitialProcess {
    fn from(process: ast::InitialProcess) -> Self {
        Self::new(
            process.condition().cloned().map(Into::into),
            process
                .body()
                .iter()
                .cloned()
                .map(|stmt| stmt.map(&mut Into::into, &mut Into::into))
                .collect(),
        )
    }
}

impl From<ast::FfEdge> for FfEdge {
    fn from(edge: ast::FfEdge) -> Self {
        match edge {
            ast::FfEdge::Pos => FfEdge::Pos,
            ast::FfEdge::Neg => FfEdge::Neg,
        }
    }
}

impl From<ast::FfEvent> for FfEvent {
    fn from(event: ast::FfEvent) -> Self {
        Self::new(event.edge().into(), event.signal().to_string())
    }
}

impl From<ast::Expr> for Expr {
    fn from(expr: ast::Expr) -> Self {
        match expr {
            ast::Expr::Ident(name) => Expr::Ident(name),
            ast::Expr::Literal(value) => Expr::Literal(value),
            ast::Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: Box::new((*expr).into()),
                msb: msb.into(),
                lsb: lsb.into(),
                signed,
            },
            ast::Expr::Concat(parts) => Expr::Concat(parts.into_iter().map(Into::into).collect()),
            ast::Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.into(),
                parts: parts.into_iter().map(Into::into).collect(),
            },
            ast::Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new((*expr).into()),
                width,
                signed,
            },
            ast::Expr::Unary { op, expr } => Expr::Unary {
                op: op.into(),
                expr: Box::new((*expr).into()),
            },
            ast::Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new((*left).into()),
                op: op.into(),
                right: Box::new((*right).into()),
            },
            ast::Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new((*condition).into()),
                then_expr: Box::new((*then_expr).into()),
                else_expr: Box::new((*else_expr).into()),
            },
            ast::Expr::Call { name, args } => Expr::Call {
                name,
                args: args.into_iter().map(Into::into).collect(),
            },
            ast::Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new((*expr).into()),
                items: items.into_iter().map(Into::into).collect(),
            },
        }
    }
}

impl From<ast::UnaryOp> for UnaryOp {
    fn from(op: ast::UnaryOp) -> Self {
        match op {
            ast::UnaryOp::Plus => UnaryOp::Plus,
            ast::UnaryOp::Minus => UnaryOp::Minus,
            ast::UnaryOp::BitNot => UnaryOp::BitNot,
            ast::UnaryOp::LogicNot => UnaryOp::LogicNot,
            ast::UnaryOp::ToTwoState => UnaryOp::ToTwoState,
            ast::UnaryOp::RedAnd => UnaryOp::RedAnd,
            ast::UnaryOp::RedOr => UnaryOp::RedOr,
            ast::UnaryOp::RedXor => UnaryOp::RedXor,
        }
    }
}

impl From<UnaryOp> for ast::UnaryOp {
    fn from(op: UnaryOp) -> Self {
        match op {
            UnaryOp::Plus => ast::UnaryOp::Plus,
            UnaryOp::Minus => ast::UnaryOp::Minus,
            UnaryOp::BitNot => ast::UnaryOp::BitNot,
            UnaryOp::LogicNot => ast::UnaryOp::LogicNot,
            UnaryOp::ToTwoState => ast::UnaryOp::ToTwoState,
            UnaryOp::RedAnd => ast::UnaryOp::RedAnd,
            UnaryOp::RedOr => ast::UnaryOp::RedOr,
            UnaryOp::RedXor => ast::UnaryOp::RedXor,
        }
    }
}

impl From<ast::BinaryOp> for BinaryOp {
    fn from(op: ast::BinaryOp) -> Self {
        match op {
            ast::BinaryOp::Add => BinaryOp::Add,
            ast::BinaryOp::Sub => BinaryOp::Sub,
            ast::BinaryOp::Mul => BinaryOp::Mul,
            ast::BinaryOp::Div => BinaryOp::Div,
            ast::BinaryOp::Mod => BinaryOp::Mod,
            ast::BinaryOp::Pow => BinaryOp::Pow,
            ast::BinaryOp::Shl => BinaryOp::Shl,
            ast::BinaryOp::Shr => BinaryOp::Shr,
            ast::BinaryOp::Sar => BinaryOp::Sar,
            ast::BinaryOp::BitAnd => BinaryOp::BitAnd,
            ast::BinaryOp::BitOr => BinaryOp::BitOr,
            ast::BinaryOp::BitXor => BinaryOp::BitXor,
            ast::BinaryOp::LogicAnd => BinaryOp::LogicAnd,
            ast::BinaryOp::LogicOr => BinaryOp::LogicOr,
            ast::BinaryOp::Eq => BinaryOp::Eq,
            ast::BinaryOp::Ne => BinaryOp::Ne,
            ast::BinaryOp::EqCase => BinaryOp::EqCase,
            ast::BinaryOp::NeCase => BinaryOp::NeCase,
            ast::BinaryOp::EqWildcard => BinaryOp::EqWildcard,
            ast::BinaryOp::NeWildcard => BinaryOp::NeWildcard,
            ast::BinaryOp::Lt => BinaryOp::Lt,
            ast::BinaryOp::Le => BinaryOp::Le,
            ast::BinaryOp::Gt => BinaryOp::Gt,
            ast::BinaryOp::Ge => BinaryOp::Ge,
        }
    }
}

impl From<BinaryOp> for ast::BinaryOp {
    fn from(op: BinaryOp) -> Self {
        match op {
            BinaryOp::Add => ast::BinaryOp::Add,
            BinaryOp::Sub => ast::BinaryOp::Sub,
            BinaryOp::Mul => ast::BinaryOp::Mul,
            BinaryOp::Div => ast::BinaryOp::Div,
            BinaryOp::Mod => ast::BinaryOp::Mod,
            BinaryOp::Pow => ast::BinaryOp::Pow,
            BinaryOp::Shl => ast::BinaryOp::Shl,
            BinaryOp::Shr => ast::BinaryOp::Shr,
            BinaryOp::Sar => ast::BinaryOp::Sar,
            BinaryOp::BitAnd => ast::BinaryOp::BitAnd,
            BinaryOp::BitOr => ast::BinaryOp::BitOr,
            BinaryOp::BitXor => ast::BinaryOp::BitXor,
            BinaryOp::LogicAnd => ast::BinaryOp::LogicAnd,
            BinaryOp::LogicOr => ast::BinaryOp::LogicOr,
            BinaryOp::Eq => ast::BinaryOp::Eq,
            BinaryOp::Ne => ast::BinaryOp::Ne,
            BinaryOp::EqCase => ast::BinaryOp::EqCase,
            BinaryOp::NeCase => ast::BinaryOp::NeCase,
            BinaryOp::EqWildcard => ast::BinaryOp::EqWildcard,
            BinaryOp::NeWildcard => ast::BinaryOp::NeWildcard,
            BinaryOp::Lt => ast::BinaryOp::Lt,
            BinaryOp::Le => ast::BinaryOp::Le,
            BinaryOp::Gt => ast::BinaryOp::Gt,
            BinaryOp::Ge => ast::BinaryOp::Ge,
        }
    }
}

/// The C type a DPI-C import passes or returns by value (IEEE 1800-2023
/// 35.5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiType {
    /// `bit`, passed as `svBit`.
    Bit,
    /// `logic` or `reg`, passed as `svLogic`.
    Logic,
    /// `byte`, `shortint`, `int` or `longint`, passed as the C integer of
    /// that width.
    Integer { width: usize, signed: bool },
}

impl DpiType {
    pub fn width(&self) -> usize {
        match self {
            DpiType::Bit | DpiType::Logic => 1,
            DpiType::Integer { width, .. } => *width,
        }
    }

    pub fn is_signed(&self) -> bool {
        matches!(self, DpiType::Integer { signed: true, .. })
    }

    pub fn is_4state(&self) -> bool {
        matches!(self, DpiType::Logic)
    }
}

/// An argument of a DPI-C import. Only `input` arguments are supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpiArgument {
    name: String,
    r#type: DpiType,
}

impl DpiArgument {
    pub(crate) fn new(name: String, r#type: DpiType) -> Self {
        Self { name, r#type }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn r#type(&self) -> DpiType {
        self.r#type
    }
}

/// A function imported from C through DPI-C (IEEE 1800-2023 35.5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpiImport {
    name: String,
    c_name: String,
    pure: bool,
    return_type: Option<DpiType>,
    arguments: Vec<DpiArgument>,
}

impl DpiImport {
    pub(crate) fn new(
        name: String,
        c_name: String,
        pure: bool,
        return_type: Option<DpiType>,
        arguments: Vec<DpiArgument>,
    ) -> Self {
        Self {
            name,
            c_name,
            pure,
            return_type,
            arguments,
        }
    }

    /// The name SystemVerilog calls the function by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The C symbol the function is linked by.
    pub fn c_name(&self) -> &str {
        &self.c_name
    }

    /// Whether the import is declared `pure`.
    pub fn is_pure(&self) -> bool {
        self.pure
    }

    /// The result type, or `None` for a `void` function.
    pub fn return_type(&self) -> Option<DpiType> {
        self.return_type
    }

    pub fn arguments(&self) -> &[DpiArgument] {
        &self.arguments
    }
}
