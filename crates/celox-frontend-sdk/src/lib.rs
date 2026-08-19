//! Stable, source-language-independent authoring API for Celox frontends.
//!
//! This crate deliberately does not depend on Celox compiler internals. A
//! frontend parses and elaborates its input, constructs a [`FrontendArtifact`],
//! and hands that artifact to the public `celox` compiler API.

use fxhash::FxHashMap;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current JSON interchange version of [`FrontendArtifact`].
pub const ARTIFACT_FORMAT_VERSION: u32 = 1;

/// Identity of one signal in the elaborated module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignalId(u32);

impl SignalId {
    /// Return the stable module-local numeric identity.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Identity of one expression in the elaborated module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExprId(u32);

impl ExprId {
    /// Return the stable module-local numeric identity.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Public direction of a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Direction {
    Input,
    Output,
    Inout,
    Internal,
}

/// Edge polarity used by clocked storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Posedge,
    Negedge,
}

/// Active level of a reset or enable signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveLevel {
    High,
    Low,
}

/// Source-independent bit-vector type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueType {
    width: usize,
    signed: bool,
    four_state: bool,
}

impl ValueType {
    pub fn bits(width: usize) -> Result<Self, BuildError> {
        Self::new(width, false, false)
    }

    pub fn logic(width: usize) -> Result<Self, BuildError> {
        Self::new(width, false, true)
    }

    pub fn new(width: usize, signed: bool, four_state: bool) -> Result<Self, BuildError> {
        if width == 0 {
            return Err(BuildError::ZeroWidth);
        }
        Ok(Self {
            width,
            signed,
            four_state,
        })
    }

    pub const fn width(self) -> usize {
        self.width
    }

    pub const fn is_signed(self) -> bool {
        self.signed
    }

    pub const fn is_four_state(self) -> bool {
        self.four_state
    }
}

/// One declared signal retained for runtime reflection and TypeScript access.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    id: SignalId,
    name: String,
    direction: Direction,
    value_type: ValueType,
    initial: Option<Constant>,
}

impl Signal {
    pub const fn id(&self) -> SignalId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn direction(&self) -> Direction {
        self.direction
    }

    pub const fn value_type(&self) -> ValueType {
        self.value_type
    }

    pub fn initial(&self) -> Option<&Constant> {
        self.initial.as_ref()
    }
}

/// A fixed bit range of a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SignalSlice {
    signal: SignalId,
    lsb: usize,
    width: usize,
}

impl SignalSlice {
    pub const fn signal(self) -> SignalId {
        self.signal
    }

    pub const fn lsb(self) -> usize {
        self.lsb
    }

    pub const fn width(self) -> usize {
        self.width
    }
}

/// Constant payload and four-state mask. Set mask bits represent X/Z.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constant {
    payload: BigUint,
    mask: BigUint,
    value_type: ValueType,
}

impl Constant {
    pub fn new(payload: BigUint, mask: BigUint, value_type: ValueType) -> Self {
        let bit_mask = (BigUint::from(1u8) << value_type.width()) - BigUint::from(1u8);
        Self {
            payload: payload & &bit_mask,
            mask: mask & bit_mask,
            value_type,
        }
    }

    pub fn two_state(payload: impl Into<BigUint>, width: usize) -> Result<Self, BuildError> {
        Ok(Self::new(
            payload.into(),
            BigUint::default(),
            ValueType::bits(width)?,
        ))
    }

    pub fn four_state(
        payload: impl Into<BigUint>,
        mask: impl Into<BigUint>,
        width: usize,
    ) -> Result<Self, BuildError> {
        Ok(Self::new(
            payload.into(),
            mask.into(),
            ValueType::logic(width)?,
        ))
    }

    pub fn payload(&self) -> &BigUint {
        &self.payload
    }

    pub fn mask(&self) -> &BigUint {
        &self.mask
    }

    pub const fn value_type(&self) -> ValueType {
        self.value_type
    }
}

/// Binary operation in the frontend expression vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    DivUnsigned,
    DivSigned,
    RemUnsigned,
    RemSigned,
    And,
    Or,
    Xor,
    ShiftLeft,
    ShiftRight,
    ArithmeticShiftRight,
    Equal,
    NotEqual,
    CaseEqual,
    CaseNotEqual,
    LessUnsigned,
    LessSigned,
    LessEqualUnsigned,
    LessEqualSigned,
    GreaterUnsigned,
    GreaterSigned,
    GreaterEqualUnsigned,
    GreaterEqualSigned,
    LogicAnd,
    LogicOr,
}

/// Unary operation in the frontend expression vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum UnaryOp {
    ToTwoState,
    Negate,
    BitNot,
    LogicNot,
    ReduceAnd,
    ReduceOr,
    ReduceXor,
    PopCount,
    CountLeadingZeros,
    CountTrailingZeros,
}

/// One expression node. Node result types are explicit so a netlist frontend
/// does not inherit source-language width inference rules from Celox.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExprNode {
    Signal(SignalSlice),
    Constant(Constant),
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Unary {
        op: UnaryOp,
        input: ExprId,
    },
    Mux {
        condition: ExprId,
        then_expr: ExprId,
        else_expr: ExprId,
    },
    Concat(Vec<ExprId>),
    Slice {
        input: ExprId,
        lsb: usize,
    },
}

/// Typed expression entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expression {
    id: ExprId,
    node: ExprNode,
    value_type: ValueType,
}

impl Expression {
    pub const fn id(&self) -> ExprId {
        self.id
    }

    pub fn node(&self) -> &ExprNode {
        &self.node
    }

    pub const fn value_type(&self) -> ValueType {
        self.value_type
    }
}

/// One continuous/combinational assignment.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Assignment {
    target: SignalSlice,
    value: ExprId,
}

impl Assignment {
    pub const fn target(self) -> SignalSlice {
        self.target
    }

    pub const fn value(self) -> ExprId {
        self.value
    }
}

/// Optional asynchronous reset configuration for a storage element.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AsyncReset {
    signal: SignalId,
    active: ActiveLevel,
    value: ExprId,
}

impl AsyncReset {
    pub const fn signal(self) -> SignalId {
        self.signal
    }

    pub const fn active(self) -> ActiveLevel {
        self.active
    }

    pub const fn value(self) -> ExprId {
        self.value
    }
}

/// Optional synchronous enable configuration for a storage element.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Enable {
    signal: SignalId,
    active: ActiveLevel,
}

impl Enable {
    pub const fn signal(self) -> SignalId {
        self.signal
    }

    pub const fn active(self) -> ActiveLevel {
        self.active
    }
}

/// One edge-triggered storage element in an elaborated netlist.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Register {
    target: SignalSlice,
    next: ExprId,
    clock: SignalId,
    edge: Edge,
    async_reset: Option<AsyncReset>,
    enable: Option<Enable>,
}

impl Register {
    pub const fn target(self) -> SignalSlice {
        self.target
    }

    pub const fn next(self) -> ExprId {
        self.next
    }

    pub const fn clock(self) -> SignalId {
        self.clock
    }

    pub const fn edge(self) -> Edge {
        self.edge
    }

    pub const fn async_reset(self) -> Option<AsyncReset> {
        self.async_reset
    }

    pub const fn enable(self) -> Option<Enable> {
        self.enable
    }
}

/// Fully elaborated frontend result. The first SDK version intentionally
/// models one flattened module; hierarchical netlists can be flattened by the
/// producing frontend without affecting runtime signal names.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrontendArtifact {
    format_version: u32,
    module_name: String,
    signals: Vec<Signal>,
    expressions: Vec<Expression>,
    assignments: Vec<Assignment>,
    registers: Vec<Register>,
    port_order: Vec<SignalId>,
}

impl FrontendArtifact {
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Serialize the artifact for transport to the Celox N-API runtime.
    pub fn to_json(&self) -> Result<String, ArtifactJsonError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Decode and version-check an artifact produced by an external frontend.
    pub fn from_json(json: &str) -> Result<Self, ArtifactJsonError> {
        let artifact: Self = serde_json::from_str(json)?;
        if artifact.format_version != ARTIFACT_FORMAT_VERSION {
            return Err(ArtifactJsonError::UnsupportedVersion {
                expected: ARTIFACT_FORMAT_VERSION,
                actual: artifact.format_version,
            });
        }
        artifact.validate()?;
        Ok(artifact)
    }

    /// Recheck all identities, ranges, widths, and expression ordering at a
    /// trust boundary. Compiler consumers call this even for in-process values.
    pub fn validate(&self) -> Result<(), BuildError> {
        if self.module_name.is_empty() {
            return Err(BuildError::EmptyModuleName);
        }
        let mut names = FxHashMap::default();
        for (index, signal) in self.signals.iter().enumerate() {
            if signal.id.index() as usize != index {
                return Err(BuildError::InvalidSignalIdentity {
                    expected: index as u32,
                    actual: signal.id.index(),
                });
            }
            if signal.name.is_empty() {
                return Err(BuildError::EmptySignalName);
            }
            if names.insert(signal.name.clone(), signal.id).is_some() {
                return Err(BuildError::DuplicateSignal(signal.name.clone()));
            }
            if signal.value_type.width() == 0 {
                return Err(BuildError::ZeroWidth);
            }
            if let Some(initial) = &signal.initial
                && initial.value_type().width() != signal.value_type.width()
            {
                return Err(BuildError::WidthMismatch {
                    expected: signal.value_type.width(),
                    actual: initial.value_type().width(),
                });
            }
        }
        let validate_slice = |slice: SignalSlice| -> Result<(), BuildError> {
            let signal = self
                .signal(slice.signal)
                .ok_or(BuildError::UnknownSignal(slice.signal.index()))?;
            if slice.width == 0 {
                return Err(BuildError::ZeroWidth);
            }
            if slice
                .lsb
                .checked_add(slice.width)
                .is_none_or(|end| end > signal.value_type.width())
            {
                return Err(BuildError::InvalidSlice {
                    lsb: slice.lsb,
                    width: slice.width,
                    signal_width: signal.value_type.width(),
                });
            }
            Ok(())
        };
        for (index, expression) in self.expressions.iter().enumerate() {
            if expression.id.index() as usize != index {
                return Err(BuildError::InvalidExpressionIdentity {
                    expected: index as u32,
                    actual: expression.id.index(),
                });
            }
            if expression.value_type.width() == 0 {
                return Err(BuildError::ZeroWidth);
            }
            let mut references = Vec::new();
            match &expression.node {
                ExprNode::Signal(slice) => validate_slice(*slice)?,
                ExprNode::Constant(value) => {
                    if value.value_type().width() != expression.value_type.width() {
                        return Err(BuildError::WidthMismatch {
                            expected: expression.value_type.width(),
                            actual: value.value_type().width(),
                        });
                    }
                }
                ExprNode::Binary { lhs, rhs, .. } => references.extend([*lhs, *rhs]),
                ExprNode::Unary { input, .. } | ExprNode::Slice { input, .. } => {
                    references.push(*input);
                }
                ExprNode::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => references.extend([*condition, *then_expr, *else_expr]),
                ExprNode::Concat(parts) => references.extend(parts),
            }
            for reference in references {
                if reference.index() as usize >= index {
                    return Err(BuildError::ForwardExpressionReference {
                        expression: expression.id.index(),
                        referenced: reference.index(),
                    });
                }
            }
        }
        for assignment in &self.assignments {
            validate_slice(assignment.target)?;
            let expression = self
                .expression(assignment.value)
                .ok_or(BuildError::UnknownExpression(assignment.value.index()))?;
            if assignment.target.width != expression.value_type.width() {
                return Err(BuildError::WidthMismatch {
                    expected: assignment.target.width,
                    actual: expression.value_type.width(),
                });
            }
        }
        for register in &self.registers {
            validate_slice(register.target)?;
            let target = self
                .signal(register.target.signal)
                .ok_or(BuildError::UnknownSignal(register.target.signal.index()))?;
            if register.target.lsb != 0 || register.target.width != target.value_type.width() {
                return Err(BuildError::PartialRegisterTarget {
                    name: target.name.clone(),
                });
            }
            let next = self
                .expression(register.next)
                .ok_or(BuildError::UnknownExpression(register.next.index()))?;
            if next.value_type.width() != register.target.width {
                return Err(BuildError::WidthMismatch {
                    expected: register.target.width,
                    actual: next.value_type.width(),
                });
            }
            for control in std::iter::once(register.clock)
                .chain(register.async_reset.map(|reset| reset.signal))
                .chain(register.enable.map(|enable| enable.signal))
            {
                let signal = self
                    .signal(control)
                    .ok_or(BuildError::UnknownSignal(control.index()))?;
                if signal.value_type.width() != 1 {
                    return Err(BuildError::InvalidControlWidth {
                        name: signal.name.clone(),
                    });
                }
            }
            if let Some(reset) = register.async_reset {
                let value = self
                    .expression(reset.value)
                    .ok_or(BuildError::UnknownExpression(reset.value.index()))?;
                if value.value_type.width() != register.target.width {
                    return Err(BuildError::WidthMismatch {
                        expected: register.target.width,
                        actual: value.value_type.width(),
                    });
                }
            }
        }
        for signal in &self.port_order {
            let signal = self
                .signal(*signal)
                .ok_or(BuildError::UnknownSignal(signal.index()))?;
            if matches!(signal.direction, Direction::Internal) {
                return Err(BuildError::InternalSignalInPortOrder {
                    name: signal.name.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    pub fn expressions(&self) -> &[Expression] {
        &self.expressions
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    pub fn registers(&self) -> &[Register] {
        &self.registers
    }

    pub fn port_order(&self) -> &[SignalId] {
        &self.port_order
    }

    pub fn signal(&self, id: SignalId) -> Option<&Signal> {
        self.signals.get(id.index() as usize)
    }

    pub fn expression(&self, id: ExprId) -> Option<&Expression> {
        self.expressions.get(id.index() as usize)
    }
}

/// Errors detected while constructing a frontend artifact.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    #[error("signal and expression widths must be non-zero")]
    ZeroWidth,
    #[error("duplicate signal name `{0}`")]
    DuplicateSignal(String),
    #[error("unknown signal id {0}")]
    UnknownSignal(u32),
    #[error("unknown expression id {0}")]
    UnknownExpression(u32),
    #[error("bit range {lsb} +: {width} exceeds signal width {signal_width}")]
    InvalidSlice {
        lsb: usize,
        width: usize,
        signal_width: usize,
    },
    #[error("width mismatch: expected {expected}, got {actual}")]
    WidthMismatch { expected: usize, actual: usize },
    #[error("control signal `{name}` must be one bit wide")]
    InvalidControlWidth { name: String },
    #[error("register target `{name}` must cover the complete signal")]
    PartialRegisterTarget { name: String },
    #[error("module name must not be empty")]
    EmptyModuleName,
    #[error("signal name must not be empty")]
    EmptySignalName,
    #[error("signal identity mismatch: expected {expected}, got {actual}")]
    InvalidSignalIdentity { expected: u32, actual: u32 },
    #[error("expression identity mismatch: expected {expected}, got {actual}")]
    InvalidExpressionIdentity { expected: u32, actual: u32 },
    #[error("expression {expression} references non-prior expression {referenced}")]
    ForwardExpressionReference { expression: u32, referenced: u32 },
    #[error("internal signal `{name}` appears in the module port order")]
    InternalSignalInPortOrder { name: String },
}

/// JSON interchange failures for frontend artifacts.
#[derive(Debug, Error)]
pub enum ArtifactJsonError {
    #[error("invalid frontend artifact JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported frontend artifact version {actual}; expected {expected}")]
    UnsupportedVersion { expected: u32, actual: u32 },
    #[error("invalid frontend artifact: {0}")]
    InvalidArtifact(#[from] BuildError),
}

/// Builder for one flattened, elaborated netlist module.
pub struct ModuleBuilder {
    name: String,
    signals: Vec<Signal>,
    signal_names: FxHashMap<String, SignalId>,
    expressions: Vec<Expression>,
    assignments: Vec<Assignment>,
    registers: Vec<Register>,
    port_order: Vec<SignalId>,
}

impl ModuleBuilder {
    pub fn new(name: impl Into<String>) -> Result<Self, BuildError> {
        let name = name.into();
        if name.is_empty() {
            return Err(BuildError::EmptyModuleName);
        }
        Ok(Self {
            name,
            signals: Vec::new(),
            signal_names: FxHashMap::default(),
            expressions: Vec::new(),
            assignments: Vec::new(),
            registers: Vec::new(),
            port_order: Vec::new(),
        })
    }

    pub fn signal(
        &mut self,
        name: impl Into<String>,
        direction: Direction,
        value_type: ValueType,
    ) -> Result<SignalId, BuildError> {
        let name = name.into();
        if name.is_empty() {
            return Err(BuildError::EmptySignalName);
        }
        if self.signal_names.contains_key(&name) {
            return Err(BuildError::DuplicateSignal(name));
        }
        let id = SignalId(self.signals.len() as u32);
        self.signal_names.insert(name.clone(), id);
        self.signals.push(Signal {
            id,
            name,
            direction,
            value_type,
            initial: None,
        });
        if !matches!(direction, Direction::Internal) {
            self.port_order.push(id);
        }
        Ok(id)
    }

    pub fn input(
        &mut self,
        name: impl Into<String>,
        value_type: ValueType,
    ) -> Result<SignalId, BuildError> {
        self.signal(name, Direction::Input, value_type)
    }

    pub fn output(
        &mut self,
        name: impl Into<String>,
        value_type: ValueType,
    ) -> Result<SignalId, BuildError> {
        self.signal(name, Direction::Output, value_type)
    }

    pub fn internal(
        &mut self,
        name: impl Into<String>,
        value_type: ValueType,
    ) -> Result<SignalId, BuildError> {
        self.signal(name, Direction::Internal, value_type)
    }

    pub fn set_initial(&mut self, signal: SignalId, value: Constant) -> Result<(), BuildError> {
        let signal_info = self.signal_info(signal)?;
        if signal_info.value_type.width() != value.value_type().width() {
            return Err(BuildError::WidthMismatch {
                expected: signal_info.value_type.width(),
                actual: value.value_type().width(),
            });
        }
        self.signals[signal.index() as usize].initial = Some(value);
        Ok(())
    }

    pub fn whole(&self, signal: SignalId) -> Result<SignalSlice, BuildError> {
        let info = self.signal_info(signal)?;
        Ok(SignalSlice {
            signal,
            lsb: 0,
            width: info.value_type.width(),
        })
    }

    pub fn slice(
        &self,
        signal: SignalId,
        lsb: usize,
        width: usize,
    ) -> Result<SignalSlice, BuildError> {
        let info = self.signal_info(signal)?;
        if width == 0 {
            return Err(BuildError::ZeroWidth);
        }
        if lsb
            .checked_add(width)
            .is_none_or(|end| end > info.value_type.width())
        {
            return Err(BuildError::InvalidSlice {
                lsb,
                width,
                signal_width: info.value_type.width(),
            });
        }
        Ok(SignalSlice { signal, lsb, width })
    }

    pub fn read(&mut self, signal: SignalId) -> Result<ExprId, BuildError> {
        let slice = self.whole(signal)?;
        self.read_slice(slice)
    }

    pub fn read_slice(&mut self, slice: SignalSlice) -> Result<ExprId, BuildError> {
        let info = self.signal_info(slice.signal)?;
        self.validate_slice(slice)?;
        let value_type = ValueType::new(
            slice.width,
            info.value_type.is_signed() && slice.width == info.value_type.width(),
            info.value_type.is_four_state(),
        )?;
        Ok(self.push_expr(ExprNode::Signal(slice), value_type))
    }

    pub fn constant(&mut self, value: Constant) -> ExprId {
        let value_type = value.value_type();
        self.push_expr(ExprNode::Constant(value), value_type)
    }

    pub fn binary(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        result_type: ValueType,
    ) -> Result<ExprId, BuildError> {
        self.expr_info(lhs)?;
        self.expr_info(rhs)?;
        Ok(self.push_expr(ExprNode::Binary { op, lhs, rhs }, result_type))
    }

    pub fn unary(
        &mut self,
        op: UnaryOp,
        input: ExprId,
        result_type: ValueType,
    ) -> Result<ExprId, BuildError> {
        self.expr_info(input)?;
        Ok(self.push_expr(ExprNode::Unary { op, input }, result_type))
    }

    pub fn mux(
        &mut self,
        condition: ExprId,
        then_expr: ExprId,
        else_expr: ExprId,
    ) -> Result<ExprId, BuildError> {
        let condition_type = self.expr_info(condition)?.value_type;
        if condition_type.width() != 1 {
            return Err(BuildError::WidthMismatch {
                expected: 1,
                actual: condition_type.width(),
            });
        }
        let then_type = self.expr_info(then_expr)?.value_type;
        let else_type = self.expr_info(else_expr)?.value_type;
        if then_type.width() != else_type.width() {
            return Err(BuildError::WidthMismatch {
                expected: then_type.width(),
                actual: else_type.width(),
            });
        }
        let result_type = ValueType::new(
            then_type.width(),
            then_type.is_signed() && else_type.is_signed(),
            then_type.is_four_state() || else_type.is_four_state(),
        )?;
        Ok(self.push_expr(
            ExprNode::Mux {
                condition,
                then_expr,
                else_expr,
            },
            result_type,
        ))
    }

    pub fn concat(&mut self, parts: Vec<ExprId>) -> Result<ExprId, BuildError> {
        let mut width = 0usize;
        let mut four_state = false;
        for part in &parts {
            let value_type = self.expr_info(*part)?.value_type;
            width = width
                .checked_add(value_type.width())
                .ok_or(BuildError::ZeroWidth)?;
            four_state |= value_type.is_four_state();
        }
        let result_type = ValueType::new(width, false, four_state)?;
        Ok(self.push_expr(ExprNode::Concat(parts), result_type))
    }

    pub fn expr_slice(
        &mut self,
        input: ExprId,
        lsb: usize,
        width: usize,
    ) -> Result<ExprId, BuildError> {
        let input_type = self.expr_info(input)?.value_type;
        if width == 0 {
            return Err(BuildError::ZeroWidth);
        }
        if lsb
            .checked_add(width)
            .is_none_or(|end| end > input_type.width())
        {
            return Err(BuildError::InvalidSlice {
                lsb,
                width,
                signal_width: input_type.width(),
            });
        }
        let result_type = ValueType::new(width, false, input_type.is_four_state())?;
        Ok(self.push_expr(ExprNode::Slice { input, lsb }, result_type))
    }

    pub fn assign(&mut self, target: SignalSlice, value: ExprId) -> Result<(), BuildError> {
        self.validate_slice(target)?;
        let value_width = self.expr_info(value)?.value_type.width();
        if target.width != value_width {
            return Err(BuildError::WidthMismatch {
                expected: target.width,
                actual: value_width,
            });
        }
        self.assignments.push(Assignment { target, value });
        Ok(())
    }

    pub fn register(
        &mut self,
        target: SignalSlice,
        next: ExprId,
        clock: SignalId,
        edge: Edge,
        async_reset: Option<AsyncReset>,
        enable: Option<Enable>,
    ) -> Result<(), BuildError> {
        self.validate_slice(target)?;
        let target_signal = self.signal_info(target.signal)?;
        if target.lsb != 0 || target.width != target_signal.value_type.width() {
            return Err(BuildError::PartialRegisterTarget {
                name: target_signal.name.clone(),
            });
        }
        let next_width = self.expr_info(next)?.value_type.width();
        if next_width != target.width {
            return Err(BuildError::WidthMismatch {
                expected: target.width,
                actual: next_width,
            });
        }
        self.validate_control(clock)?;
        if let Some(reset) = async_reset {
            self.validate_control(reset.signal)?;
            let reset_width = self.expr_info(reset.value)?.value_type.width();
            if reset_width != target.width {
                return Err(BuildError::WidthMismatch {
                    expected: target.width,
                    actual: reset_width,
                });
            }
        }
        if let Some(enable) = enable {
            self.validate_control(enable.signal)?;
        }
        self.registers.push(Register {
            target,
            next,
            clock,
            edge,
            async_reset,
            enable,
        });
        Ok(())
    }

    pub fn async_reset(
        &self,
        signal: SignalId,
        active: ActiveLevel,
        value: ExprId,
    ) -> Result<AsyncReset, BuildError> {
        self.validate_control(signal)?;
        self.expr_info(value)?;
        Ok(AsyncReset {
            signal,
            active,
            value,
        })
    }

    pub fn enable(&self, signal: SignalId, active: ActiveLevel) -> Result<Enable, BuildError> {
        self.validate_control(signal)?;
        Ok(Enable { signal, active })
    }

    pub fn finish(self) -> FrontendArtifact {
        FrontendArtifact {
            format_version: ARTIFACT_FORMAT_VERSION,
            module_name: self.name,
            signals: self.signals,
            expressions: self.expressions,
            assignments: self.assignments,
            registers: self.registers,
            port_order: self.port_order,
        }
    }

    fn signal_info(&self, signal: SignalId) -> Result<&Signal, BuildError> {
        self.signals
            .get(signal.index() as usize)
            .ok_or(BuildError::UnknownSignal(signal.index()))
    }

    fn expr_info(&self, expression: ExprId) -> Result<&Expression, BuildError> {
        self.expressions
            .get(expression.index() as usize)
            .ok_or(BuildError::UnknownExpression(expression.index()))
    }

    fn validate_slice(&self, slice: SignalSlice) -> Result<(), BuildError> {
        let signal = self.signal_info(slice.signal)?;
        if slice.width == 0 {
            return Err(BuildError::ZeroWidth);
        }
        if slice
            .lsb
            .checked_add(slice.width)
            .is_none_or(|end| end > signal.value_type.width())
        {
            return Err(BuildError::InvalidSlice {
                lsb: slice.lsb,
                width: slice.width,
                signal_width: signal.value_type.width(),
            });
        }
        Ok(())
    }

    fn validate_control(&self, signal: SignalId) -> Result<(), BuildError> {
        let signal = self.signal_info(signal)?;
        if signal.value_type.width() != 1 {
            return Err(BuildError::InvalidControlWidth {
                name: signal.name.clone(),
            });
        }
        Ok(())
    }

    fn push_expr(&mut self, node: ExprNode, value_type: ValueType) -> ExprId {
        let id = ExprId(self.expressions.len() as u32);
        self.expressions.push(Expression {
            id,
            node,
            value_type,
        });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_flat_combinational_artifact() {
        let mut module = ModuleBuilder::new("Adder").unwrap();
        let ty = ValueType::bits(8).unwrap();
        let a = module.input("a", ty).unwrap();
        let b = module.input("b", ty).unwrap();
        let y = module.output("y", ty).unwrap();
        let a = module.read(a).unwrap();
        let b = module.read(b).unwrap();
        let sum = module.binary(BinaryOp::Add, a, b, ty).unwrap();
        let y = module.whole(y).unwrap();
        module.assign(y, sum).unwrap();

        let artifact = module.finish();
        assert_eq!(artifact.module_name(), "Adder");
        assert_eq!(artifact.signals().len(), 3);
        assert_eq!(artifact.assignments().len(), 1);
        assert_eq!(artifact.port_order().len(), 3);
    }

    #[test]
    fn rejects_partial_register_target() {
        let mut module = ModuleBuilder::new("Counter").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let byte = ValueType::bits(8).unwrap();
        let clock = module.input("clock", bit).unwrap();
        let q = module.output("q", byte).unwrap();
        let q_expr = module.read(q).unwrap();
        let partial = module.slice(q, 0, 4).unwrap();
        let error = module
            .register(partial, q_expr, clock, Edge::Posedge, None, None)
            .unwrap_err();
        assert!(matches!(error, BuildError::PartialRegisterTarget { .. }));
    }
}
