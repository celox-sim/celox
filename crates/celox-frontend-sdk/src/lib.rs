//! Stable, source-language-independent authoring API for Celox frontends.
//!
//! This crate deliberately does not depend on Celox compiler internals. A
//! frontend parses and elaborates its input, constructs a [`FrontendArtifact`],
//! and hands that artifact to the public `celox` compiler API.

use std::collections::BTreeMap;

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
    /// Reserved for a future artifact version; version 1 validation rejects it.
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
        let mask = if value_type.is_four_state() {
            mask & &bit_mask
        } else {
            BigUint::default()
        };
        Self {
            payload: payload & &bit_mask,
            mask,
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

fn signal_name_prefixes(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('.').map(|(index, _)| &name[..index])
}

fn insert_driver_target(
    driver_ranges: &mut FxHashMap<SignalId, BTreeMap<usize, usize>>,
    target: SignalSlice,
    signal_name: &str,
) -> Result<(), BuildError> {
    let end = target.lsb + target.width;
    let ranges = driver_ranges.entry(target.signal).or_default();
    if ranges
        .range(..end)
        .next_back()
        .is_some_and(|(_, existing_end)| *existing_end > target.lsb)
    {
        return Err(BuildError::OverlappingDrivers {
            name: signal_name.to_string(),
        });
    }
    ranges.insert(target.lsb, end);
    Ok(())
}

fn validate_constant_state(value: &Constant) -> Result<(), BuildError> {
    let width = value.value_type().width() as u64;
    if value.payload().bits() > width || value.mask().bits() > width {
        return Err(BuildError::ConstantOutOfRange {
            width: value.value_type().width(),
        });
    }
    if !value.value_type().is_four_state() && value.mask() != &BigUint::default() {
        return Err(BuildError::TwoStateConstantMask);
    }
    Ok(())
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
        let mut names: FxHashMap<String, SignalId> = FxHashMap::default();
        let mut namespace_prefixes: FxHashMap<String, String> = FxHashMap::default();
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
            if matches!(signal.direction, Direction::Inout) {
                return Err(BuildError::UnsupportedInout {
                    name: signal.name.clone(),
                });
            }
            if names.contains_key(&signal.name) {
                return Err(BuildError::DuplicateSignal(signal.name.clone()));
            }
            if let Some(existing) = namespace_prefixes.get(&signal.name) {
                return Err(BuildError::SignalNamespaceCollision {
                    first: existing.clone(),
                    second: signal.name.clone(),
                });
            }
            if let Some(existing) =
                signal_name_prefixes(&signal.name).find(|prefix| names.contains_key(*prefix))
            {
                return Err(BuildError::SignalNamespaceCollision {
                    first: existing.to_string(),
                    second: signal.name.clone(),
                });
            }
            names.insert(signal.name.clone(), signal.id);
            for prefix in signal_name_prefixes(&signal.name) {
                namespace_prefixes
                    .entry(prefix.to_string())
                    .or_insert_with(|| signal.name.clone());
            }
            if signal.value_type.width() == 0 {
                return Err(BuildError::ZeroWidth);
            }
            if let Some(initial) = &signal.initial {
                validate_constant_state(initial)?;
                if initial.value_type().width() != signal.value_type.width() {
                    return Err(BuildError::WidthMismatch {
                        expected: signal.value_type.width(),
                        actual: initial.value_type().width(),
                    });
                }
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
                ExprNode::Signal(slice) => {
                    validate_slice(*slice)?;
                    let signal_type = self
                        .signal(slice.signal)
                        .ok_or(BuildError::UnknownSignal(slice.signal.index()))?
                        .value_type();
                    let expected_type = ValueType::new(
                        slice.width,
                        signal_type.is_signed() && slice.width == signal_type.width(),
                        signal_type.is_four_state(),
                    )?;
                    if expression.value_type() != expected_type {
                        return Err(BuildError::TypeMismatch {
                            expected: expected_type,
                            actual: expression.value_type(),
                        });
                    }
                }
                ExprNode::Constant(value) => {
                    validate_constant_state(value)?;
                    if value.value_type() != expression.value_type() {
                        return Err(BuildError::TypeMismatch {
                            expected: value.value_type(),
                            actual: expression.value_type(),
                        });
                    }
                }
                ExprNode::Binary { lhs, rhs, .. } => references.extend([*lhs, *rhs]),
                ExprNode::Unary { input, .. } => {
                    references.push(*input);
                }
                ExprNode::Slice { input, lsb } => {
                    if input.index() as usize >= index {
                        return Err(BuildError::ForwardExpressionReference {
                            expression: expression.id.index(),
                            referenced: input.index(),
                        });
                    }
                    let input_type = self
                        .expression(*input)
                        .ok_or(BuildError::UnknownExpression(input.index()))?
                        .value_type();
                    if lsb
                        .checked_add(expression.value_type().width())
                        .is_none_or(|end| end > input_type.width())
                    {
                        return Err(BuildError::InvalidSlice {
                            lsb: *lsb,
                            width: expression.value_type().width(),
                            signal_width: input_type.width(),
                        });
                    }
                    let expected_type = ValueType::new(
                        expression.value_type().width(),
                        false,
                        input_type.is_four_state(),
                    )?;
                    if expression.value_type() != expected_type {
                        return Err(BuildError::TypeMismatch {
                            expected: expected_type,
                            actual: expression.value_type(),
                        });
                    }
                }
                ExprNode::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => {
                    for reference in [*condition, *then_expr, *else_expr] {
                        if reference.index() as usize >= index {
                            return Err(BuildError::ForwardExpressionReference {
                                expression: expression.id.index(),
                                referenced: reference.index(),
                            });
                        }
                    }
                    let condition_type = self
                        .expression(*condition)
                        .ok_or(BuildError::UnknownExpression(condition.index()))?
                        .value_type();
                    if condition_type.width() != 1 {
                        return Err(BuildError::WidthMismatch {
                            expected: 1,
                            actual: condition_type.width(),
                        });
                    }
                    let then_type = self
                        .expression(*then_expr)
                        .ok_or(BuildError::UnknownExpression(then_expr.index()))?
                        .value_type();
                    let else_type = self
                        .expression(*else_expr)
                        .ok_or(BuildError::UnknownExpression(else_expr.index()))?
                        .value_type();
                    if then_type.width() != else_type.width() {
                        return Err(BuildError::WidthMismatch {
                            expected: then_type.width(),
                            actual: else_type.width(),
                        });
                    }
                    let expected_type = ValueType {
                        width: then_type.width(),
                        signed: then_type.is_signed() && else_type.is_signed(),
                        four_state: then_type.is_four_state() || else_type.is_four_state(),
                    };
                    if expression.value_type() != expected_type {
                        return Err(BuildError::TypeMismatch {
                            expected: expected_type,
                            actual: expression.value_type(),
                        });
                    }
                }
                ExprNode::Concat(parts) => {
                    if parts.is_empty() {
                        return Err(BuildError::ZeroWidth);
                    }
                    let mut width = 0usize;
                    let mut four_state = false;
                    for part in parts {
                        if part.index() as usize >= index {
                            return Err(BuildError::ForwardExpressionReference {
                                expression: expression.id.index(),
                                referenced: part.index(),
                            });
                        }
                        let part_type = self
                            .expression(*part)
                            .ok_or(BuildError::UnknownExpression(part.index()))?
                            .value_type();
                        width = width
                            .checked_add(part_type.width())
                            .ok_or(BuildError::ZeroWidth)?;
                        four_state |= part_type.is_four_state();
                    }
                    let expected_type = ValueType::new(width, false, four_state)?;
                    if expression.value_type() != expected_type {
                        return Err(BuildError::TypeMismatch {
                            expected: expected_type,
                            actual: expression.value_type(),
                        });
                    }
                }
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
        let mut driver_ranges = FxHashMap::default();
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
            let signal = self
                .signal(assignment.target.signal)
                .ok_or(BuildError::UnknownSignal(assignment.target.signal.index()))?;
            validate_driver_target(signal)?;
            insert_driver_target(&mut driver_ranges, assignment.target, &signal.name)?;
        }
        for register in &self.registers {
            validate_slice(register.target)?;
            let target = self
                .signal(register.target.signal)
                .ok_or(BuildError::UnknownSignal(register.target.signal.index()))?;
            validate_driver_target(target)?;
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
            insert_driver_target(&mut driver_ranges, register.target, &target.name)?;
        }
        let mut ordered_ports = FxHashMap::default();
        for signal in &self.port_order {
            let signal = self
                .signal(*signal)
                .ok_or(BuildError::UnknownSignal(signal.index()))?;
            if matches!(signal.direction, Direction::Internal) {
                return Err(BuildError::InternalSignalInPortOrder {
                    name: signal.name.clone(),
                });
            }
            if ordered_ports.insert(signal.id, ()).is_some() {
                return Err(BuildError::DuplicatePortOrder {
                    name: signal.name.clone(),
                });
            }
        }
        for signal in &self.signals {
            if !matches!(signal.direction, Direction::Internal)
                && !ordered_ports.contains_key(&signal.id)
            {
                return Err(BuildError::MissingPortOrder {
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
    #[error("type mismatch: expected {expected:?}, got {actual:?}")]
    TypeMismatch {
        expected: ValueType,
        actual: ValueType,
    },
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
    #[error("signal `{name}` appears more than once in the module port order")]
    DuplicatePortOrder { name: String },
    #[error("public signal `{name}` is missing from the module port order")]
    MissingPortOrder { name: String },
    #[error("signal `{name}` has overlapping continuous or register drivers")]
    OverlappingDrivers { name: String },
    #[error("input signal `{name}` cannot be driven by artifact logic")]
    InvalidDriverTarget { name: String },
    #[error("two-state constants cannot contain X/Z mask bits")]
    TwoStateConstantMask,
    #[error("constant payload or mask does not fit its declared width {width}")]
    ConstantOutOfRange { width: usize },
    #[error("signal names `{first}` and `{second}` collide in the DUT namespace")]
    SignalNamespaceCollision { first: String, second: String },
    #[error("inout signal `{name}` is not supported by frontend artifact format version 1")]
    UnsupportedInout { name: String },
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
    signal_namespace_prefixes: FxHashMap<String, String>,
    expressions: Vec<Expression>,
    assignments: Vec<Assignment>,
    registers: Vec<Register>,
    driver_ranges: FxHashMap<SignalId, BTreeMap<usize, usize>>,
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
            signal_namespace_prefixes: FxHashMap::default(),
            expressions: Vec::new(),
            assignments: Vec::new(),
            registers: Vec::new(),
            driver_ranges: FxHashMap::default(),
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
        if matches!(direction, Direction::Inout) {
            return Err(BuildError::UnsupportedInout { name });
        }
        if self.signal_names.contains_key(&name) {
            return Err(BuildError::DuplicateSignal(name));
        }
        if let Some(existing) = self.signal_namespace_prefixes.get(&name) {
            return Err(BuildError::SignalNamespaceCollision {
                first: existing.clone(),
                second: name,
            });
        }
        if let Some(existing) =
            signal_name_prefixes(&name).find(|prefix| self.signal_names.contains_key(*prefix))
        {
            return Err(BuildError::SignalNamespaceCollision {
                first: existing.to_string(),
                second: name.clone(),
            });
        }
        let id = SignalId(self.signals.len() as u32);
        self.signal_names.insert(name.clone(), id);
        for prefix in signal_name_prefixes(&name) {
            self.signal_namespace_prefixes
                .entry(prefix.to_string())
                .or_insert_with(|| name.clone());
        }
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
        validate_driver_target(self.signal_info(target.signal)?)?;
        let value_width = self.expr_info(value)?.value_type.width();
        if target.width != value_width {
            return Err(BuildError::WidthMismatch {
                expected: target.width,
                actual: value_width,
            });
        }
        self.record_driver_target(target)?;
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
        validate_driver_target(target_signal)?;
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
        self.record_driver_target(target)?;
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

    fn record_driver_target(&mut self, target: SignalSlice) -> Result<(), BuildError> {
        let signal_name = self.signal_info(target.signal)?.name.clone();
        insert_driver_target(&mut self.driver_ranges, target, &signal_name)
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

fn validate_driver_target(signal: &Signal) -> Result<(), BuildError> {
    if matches!(signal.direction, Direction::Output | Direction::Internal) {
        Ok(())
    } else {
        Err(BuildError::InvalidDriverTarget {
            name: signal.name.clone(),
        })
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

    #[test]
    fn rejects_inout_in_builder_and_json_artifacts() {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("InoutBuilder").unwrap();
        let error = module.signal("bus", Direction::Inout, bit).unwrap_err();
        assert!(matches!(error, BuildError::UnsupportedInout { .. }));

        let mut module = ModuleBuilder::new("InoutJson").unwrap();
        module.input("bus", bit).unwrap();
        let json = module.finish().to_json().unwrap().replace("Input", "Inout");
        let error = FrontendArtifact::from_json(&json).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::UnsupportedInout { .. })
        ));
    }

    #[test]
    fn rejects_out_of_bounds_expression_slice_from_json() {
        let byte = ValueType::bits(8).unwrap();
        let mut module = ModuleBuilder::new("InvalidSlice").unwrap();
        let input = module.input("input", byte).unwrap();
        let input = module.read(input).unwrap();
        module.expr_slice(input, 0, 4).unwrap();
        let artifact = module.finish();
        let mut json = serde_json::to_value(&artifact).unwrap();
        json["expressions"][1]["node"]["Slice"]["lsb"] = 7.into();

        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::InvalidSlice {
                lsb: 7,
                width: 4,
                signal_width: 8,
            })
        ));
    }

    #[test]
    fn revalidates_expression_slice_type_from_json() {
        let logic = ValueType::logic(8).unwrap();
        let mut module = ModuleBuilder::new("SliceType").unwrap();
        let input = module.input("input", logic).unwrap();
        let input = module.read(input).unwrap();
        module.expr_slice(input, 0, 4).unwrap();
        let artifact = module.finish();

        let mut signed = serde_json::to_value(&artifact).unwrap();
        signed["expressions"][1]["value_type"]["signed"] = true.into();
        let error = FrontendArtifact::from_json(&signed.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));

        let mut two_state = serde_json::to_value(&artifact).unwrap();
        two_state["expressions"][1]["value_type"]["four_state"] = false.into();
        let error = FrontendArtifact::from_json(&two_state.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_overlapping_register_and_assignment_drivers() {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("DriverConflict").unwrap();
        let clock = module.input("clock", bit).unwrap();
        let d = module.input("d", bit).unwrap();
        let q = module.output("q", bit).unwrap();
        let d_expr = module.read(d).unwrap();
        let q_target = module.whole(q).unwrap();
        module.assign(q_target, d_expr).unwrap();
        let error = module
            .register(q_target, d_expr, clock, Edge::Posedge, None, None)
            .unwrap_err();
        assert!(matches!(error, BuildError::OverlappingDrivers { .. }));

        let mut module = ModuleBuilder::new("DuplicateRegisterJson").unwrap();
        let clock = module.input("clock", bit).unwrap();
        let d = module.input("d", bit).unwrap();
        let q = module.output("q", bit).unwrap();
        let d_expr = module.read(d).unwrap();
        let q_target = module.whole(q).unwrap();
        module
            .register(q_target, d_expr, clock, Edge::Posedge, None, None)
            .unwrap();
        let mut json = serde_json::to_value(module.finish()).unwrap();
        let duplicate = json["registers"][0].clone();
        json["registers"].as_array_mut().unwrap().push(duplicate);
        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::OverlappingDrivers { .. })
        ));
    }

    #[test]
    fn rejects_incomplete_or_duplicate_json_port_order() {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("PortOrder").unwrap();
        module.input("a", bit).unwrap();
        module.output("b", bit).unwrap();
        let artifact = module.finish();

        let mut duplicate = serde_json::to_value(&artifact).unwrap();
        duplicate["port_order"] = serde_json::json!([0, 0]);
        let error = FrontendArtifact::from_json(&duplicate.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::DuplicatePortOrder { .. })
        ));

        let mut missing = serde_json::to_value(&artifact).unwrap();
        missing["port_order"] = serde_json::json!([0]);
        let error = FrontendArtifact::from_json(&missing.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::MissingPortOrder { .. })
        ));
    }

    #[test]
    fn revalidates_mux_types_from_json() {
        let bit = ValueType::bits(1).unwrap();
        let byte = ValueType::bits(8).unwrap();
        let mut module = ModuleBuilder::new("MuxJson").unwrap();
        let condition = module.input("condition", bit).unwrap();
        let a = module.input("a", byte).unwrap();
        let b = module.input("b", byte).unwrap();
        let condition = module.read(condition).unwrap();
        let a = module.read(a).unwrap();
        let b = module.read(b).unwrap();
        module.mux(condition, a, b).unwrap();
        let artifact = module.finish();

        let mut wide_condition = serde_json::to_value(&artifact).unwrap();
        wide_condition["expressions"][3]["node"]["Mux"]["condition"] = 1.into();
        let error = FrontendArtifact::from_json(&wide_condition.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::WidthMismatch {
                expected: 1,
                actual: 8,
            })
        ));

        let mut wrong_result = serde_json::to_value(&artifact).unwrap();
        wrong_result["expressions"][3]["value_type"]["width"] = 4.into();
        let error = FrontendArtifact::from_json(&wrong_result.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn normalizes_two_state_constant_masks_and_rejects_them_in_json() {
        let byte = ValueType::bits(8).unwrap();
        let value = Constant::new(BigUint::from(0xa5u8), BigUint::from(0xffu8), byte);
        assert_eq!(value.mask(), &BigUint::default());

        let mut module = ModuleBuilder::new("ConstantMaskJson").unwrap();
        module.constant(value);
        let mut json = serde_json::to_value(module.finish()).unwrap();
        json["expressions"][0]["node"]["Constant"]["mask"] =
            serde_json::to_value(BigUint::from(1u8)).unwrap();
        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TwoStateConstantMask)
        ));
    }

    #[test]
    fn rejects_out_of_range_constant_bits_from_json() {
        let value = Constant::four_state(0xa5u8, 0u8, 8).unwrap();
        let mut module = ModuleBuilder::new("ConstantRange").unwrap();
        module.constant(value);
        let artifact = module.finish();

        let mut payload = serde_json::to_value(&artifact).unwrap();
        payload["expressions"][0]["node"]["Constant"]["payload"] =
            serde_json::to_value(BigUint::from(0x100u16)).unwrap();
        let error = FrontendArtifact::from_json(&payload.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::ConstantOutOfRange { width: 8 })
        ));

        let mut mask = serde_json::to_value(&artifact).unwrap();
        mask["expressions"][0]["node"]["Constant"]["mask"] =
            serde_json::to_value(BigUint::from(0x100u16)).unwrap();
        let error = FrontendArtifact::from_json(&mask.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::ConstantOutOfRange { width: 8 })
        ));
    }

    #[test]
    fn rejects_signal_namespace_prefix_collisions() {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("NamespaceBuilder").unwrap();
        module.input("bus", bit).unwrap();
        let error = module.input("bus.member", bit).unwrap_err();
        assert!(matches!(error, BuildError::SignalNamespaceCollision { .. }));

        let mut reverse = ModuleBuilder::new("ReverseNamespaceBuilder").unwrap();
        reverse.input("bus.member", bit).unwrap();
        let error = reverse.input("bus", bit).unwrap_err();
        assert!(matches!(error, BuildError::SignalNamespaceCollision { .. }));

        let mut module = ModuleBuilder::new("NamespaceJson").unwrap();
        module.input("first", bit).unwrap();
        module.input("second", bit).unwrap();
        let mut json = serde_json::to_value(module.finish()).unwrap();
        json["signals"][0]["name"] = "bus".into();
        json["signals"][1]["name"] = "bus.member".into();
        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::SignalNamespaceCollision { .. })
        ));
    }

    #[test]
    fn revalidates_leaf_expression_types_from_json() {
        let byte = ValueType::bits(8).unwrap();
        let mut module = ModuleBuilder::new("LeafTypes").unwrap();
        let input = module.input("input", byte).unwrap();
        module.read(input).unwrap();
        module.constant(Constant::two_state(0xa5u8, 8).unwrap());
        let artifact = module.finish();

        let mut signal = serde_json::to_value(&artifact).unwrap();
        signal["expressions"][0]["value_type"]["width"] = 4.into();
        let error = FrontendArtifact::from_json(&signal.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));

        let mut constant = serde_json::to_value(&artifact).unwrap();
        constant["expressions"][1]["value_type"]["four_state"] = true.into();
        let error = FrontendArtifact::from_json(&constant.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn revalidates_concat_types_from_json() {
        let bit = ValueType::bits(1).unwrap();
        let logic = ValueType::logic(1).unwrap();
        let mut module = ModuleBuilder::new("ConcatTypes").unwrap();
        let a = module.input("a", bit).unwrap();
        let b = module.input("b", logic).unwrap();
        let a = module.read(a).unwrap();
        let b = module.read(b).unwrap();
        module.concat(vec![a, b]).unwrap();
        let artifact = module.finish();

        let mut wrong_type = serde_json::to_value(&artifact).unwrap();
        wrong_type["expressions"][2]["value_type"]["four_state"] = false.into();
        let error = FrontendArtifact::from_json(&wrong_type.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::TypeMismatch { .. })
        ));

        let mut empty = serde_json::to_value(&artifact).unwrap();
        empty["expressions"][2]["node"]["Concat"] = serde_json::json!([]);
        let error = FrontendArtifact::from_json(&empty.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::ZeroWidth)
        ));
    }

    #[test]
    fn rejects_input_driver_targets_in_builder_and_json() {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("InputDriver").unwrap();
        let clock = module.input("clock", bit).unwrap();
        let input = module.input("input", bit).unwrap();
        let value = module.constant(Constant::two_state(1u8, 1).unwrap());
        let target = module.whole(input).unwrap();
        let error = module.assign(target, value).unwrap_err();
        assert!(matches!(error, BuildError::InvalidDriverTarget { .. }));
        let error = module
            .register(target, value, clock, Edge::Posedge, None, None)
            .unwrap_err();
        assert!(matches!(error, BuildError::InvalidDriverTarget { .. }));

        let mut valid = ModuleBuilder::new("InputDriverJson").unwrap();
        let input = valid.input("input", bit).unwrap();
        let output = valid.output("output", bit).unwrap();
        let value = valid.read(input).unwrap();
        valid.assign(valid.whole(output).unwrap(), value).unwrap();
        let mut json = serde_json::to_value(valid.finish()).unwrap();
        json["assignments"][0]["target"]["signal"] = input.index().into();
        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::InvalidDriverTarget { .. })
        ));

        let mut valid = ModuleBuilder::new("InputRegisterJson").unwrap();
        let clock = valid.input("clock", bit).unwrap();
        let input = valid.input("input", bit).unwrap();
        let output = valid.output("output", bit).unwrap();
        let value = valid.read(input).unwrap();
        let output_target = valid.whole(output).unwrap();
        valid
            .register(output_target, value, clock, Edge::Posedge, None, None)
            .unwrap();
        let mut json = serde_json::to_value(valid.finish()).unwrap();
        json["registers"][0]["target"]["signal"] = input.index().into();
        let error = FrontendArtifact::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(
            error,
            ArtifactJsonError::InvalidArtifact(BuildError::InvalidDriverTarget { .. })
        ));
    }
}
