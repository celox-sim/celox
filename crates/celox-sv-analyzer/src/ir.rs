//! Analyzer IR produced after SystemVerilog semantic analysis.

use crate::{ast, symbol::ModuleId, typecheck};

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
}

impl Module {
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
        }
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
    ) -> Self {
        Self {
            module_name,
            name,
            parameter_names,
            parameter_overrides,
            condition,
            port_names,
            port_connections,
        }
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
}

impl ParameterOverride {
    pub(crate) fn new(name: String, value: Option<ConstExpr>) -> Self {
        Self { name, value }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
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
}

impl CombProcess {
    pub(crate) fn new(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        assignments: Vec<Assignment>,
    ) -> Self {
        Self {
            kind,
            condition,
            assignments,
        }
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
    assignments: Vec<ConditionalAssignment>,
}

impl FfProcess {
    pub(crate) fn new(events: Vec<FfEvent>, assignments: Vec<ConditionalAssignment>) -> Self {
        Self {
            events,
            assignments,
        }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn assignments(&self) -> &[ConditionalAssignment] {
        &self.assignments
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
pub struct ConditionalAssignment {
    condition: Option<Expr>,
    assignment: Assignment,
}

impl ConditionalAssignment {
    pub(crate) fn new(condition: Option<Expr>, assignment: Assignment) -> Self {
        Self {
            condition,
            assignment,
        }
    }

    pub fn condition(&self) -> Option<&Expr> {
        self.condition.as_ref()
    }

    pub fn assignment(&self) -> &Assignment {
        &self.assignment
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
            ast::LValue::Select { name, msb, lsb, .. } => LValue::Select {
                name,
                msb: msb.into(),
                lsb: lsb.into(),
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
                .assignments()
                .iter()
                .cloned()
                .map(Into::into)
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

impl From<ast::ConditionalAssignment> for ConditionalAssignment {
    fn from(assignment: ast::ConditionalAssignment) -> Self {
        Self::new(
            assignment.condition().cloned().map(Into::into),
            assignment.assignment().clone().into(),
        )
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
