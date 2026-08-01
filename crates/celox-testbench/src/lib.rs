//! Source-independent testbench bytecode contracts.
//!
//! Frontends translate source-language operators into this vocabulary before
//! execution. No Veryl AST, parser ID, simulator backend, or runtime handle is
//! part of the bytecode representation.

use num_bigint::BigUint;

mod format;
mod vm;
pub use format::{DisplayFormatArg, format_display_arg};
pub use vm::{CompiledExpr, TestbenchValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateLocation<A> {
    pub address: A,
    pub byte_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TestbenchOperator {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    BitXnor,
    BitNand,
    BitNor,
    LogicShiftL,
    LogicShiftR,
    ArithShiftL,
    ArithShiftR,
    Eq,
    EqWildcard,
    Ne,
    NeWildcard,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    LogicAnd,
    LogicOr,
    LogicNot,
    BitNot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssertMessage<Argument> {
    Formatted {
        template: String,
        args: Vec<Argument>,
    },
    DynamicArgs(Vec<Argument>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockCount<Expression> {
    Static(u64),
    Dynamic(Expression),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopBound<Expression> {
    Static(usize),
    Dynamic {
        expr: Expression,
        width: usize,
        signed: bool,
    },
}

/// Source-independent testbench control program.
///
/// Frontends instantiate this with semantic state/event identities and
/// unbound expressions. Runtime binding instantiates the same contract with
/// backend event/signal handles and executable expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestbenchStatement<Event, Signal, Expression, Argument> {
    ClockNext {
        clock_event: Event,
        count: ClockCount<Expression>,
    },
    ResetAssert {
        reset_signal: Signal,
        clock_event: Event,
        duration: u64,
        assert_value: u8,
        deassert_value: u8,
    },
    Assert {
        expr: Expression,
        site_id: u32,
        continue_on_fail: bool,
        message: Option<AssertMessage<Argument>>,
        location: Option<SourceLocation>,
    },
    Display {
        message: Option<AssertMessage<Argument>>,
        newline: bool,
    },
    If {
        expr: Expression,
        then_block: Vec<Self>,
        else_block: Vec<Self>,
    },
    For {
        loop_var: Option<(Signal, usize, bool)>,
        start: LoopBound<Expression>,
        end: LoopBound<Expression>,
        inclusive: bool,
        step: usize,
        step_op: Option<TestbenchOperator>,
        reverse: bool,
        body: Vec<Self>,
    },
    Assign {
        dst: Signal,
        expr: Expression,
    },
    Break,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SemanticSignal<A> {
    pub address: A,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticArgument<A> {
    pub expr: ExprBytecode<StateLocation<A>>,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
}

pub type SemanticStatement<A> =
    TestbenchStatement<A, SemanticSignal<A>, ExprBytecode<StateLocation<A>>, SemanticArgument<A>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestbenchProgram<A> {
    statements: Vec<SemanticStatement<A>>,
}

impl<A> Default for TestbenchProgram<A> {
    fn default() -> Self {
        Self {
            statements: Vec::new(),
        }
    }
}

impl<A> TestbenchProgram<A> {
    pub fn new(statements: Vec<SemanticStatement<A>>) -> Self {
        Self { statements }
    }

    pub fn statements(&self) -> &[SemanticStatement<A>] {
        &self.statements
    }

    pub fn into_statements(self) -> Vec<SemanticStatement<A>> {
        self.statements
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprOpcode<L = usize> {
    ConstU64(u64),
    ConstWide(BigUint),
    LoadU64 {
        location: L,
        byte_size: usize,
        mask: u64,
    },
    LoadWide {
        location: L,
        byte_size: usize,
        width: usize,
    },
    BinOp(TestbenchOperator),
    TypedBinOp {
        op: TestbenchOperator,
        lhs_width: usize,
        rhs_width: usize,
        result_width: usize,
        lhs_signed: bool,
        rhs_signed: bool,
    },
    TypedUnary {
        op: TestbenchOperator,
        operand_width: usize,
        result_width: usize,
    },
    Resize {
        source_width: usize,
        target_width: usize,
        signed: bool,
    },
    ConcatPart {
        part_width: usize,
        result_width: usize,
    },
    Ternary {
        then_len: usize,
        else_len: usize,
    },
    LoadIndexed {
        location: L,
        stride_bytes: usize,
        element_byte_size: usize,
        element_width: usize,
    },
    LoadBitSelect {
        location: L,
        base_byte_size: usize,
        select_width: usize,
    },
    StoreU64 {
        location: L,
        byte_size: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExprBytecode<L = usize> {
    ops: Vec<ExprOpcode<L>>,
}

impl<L> ExprBytecode<L> {
    pub fn new(ops: Vec<ExprOpcode<L>>) -> Self {
        Self { ops }
    }

    pub fn ops(&self) -> &[ExprOpcode<L>] {
        &self.ops
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindError<A> {
    pub address: A,
}

pub struct ExecutableArgument {
    pub expr: CompiledExpr,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
}

pub type ExecutableAssertMessage = AssertMessage<ExecutableArgument>;
pub type ExecutableClockCount = ClockCount<CompiledExpr>;
pub type ExecutableLoopBound = LoopBound<CompiledExpr>;
pub type ExecutableStatement<Event, Signal> =
    TestbenchStatement<Event, Signal, CompiledExpr, ExecutableArgument>;

pub struct ExecutableTestbench<Event, Signal> {
    statements: Vec<ExecutableStatement<Event, Signal>>,
}

impl<Event, Signal> ExecutableTestbench<Event, Signal> {
    pub fn new(statements: Vec<ExecutableStatement<Event, Signal>>) -> Self {
        Self { statements }
    }

    pub fn statements(&self) -> &[ExecutableStatement<Event, Signal>] {
        &self.statements
    }

    pub fn into_statements(self) -> Vec<ExecutableStatement<Event, Signal>> {
        self.statements
    }
}

impl<A> ExprBytecode<StateLocation<A>> {
    pub fn bind_with(
        self,
        mut resolve: impl FnMut(&A) -> Option<usize>,
    ) -> Result<ExprBytecode, BindError<A>> {
        let mut ops = Vec::with_capacity(self.ops.len());
        for op in self.ops {
            let bound = match op {
                ExprOpcode::ConstU64(value) => ExprOpcode::ConstU64(value),
                ExprOpcode::ConstWide(value) => ExprOpcode::ConstWide(value),
                ExprOpcode::LoadU64 {
                    location,
                    byte_size,
                    mask,
                } => ExprOpcode::LoadU64 {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                    mask,
                },
                ExprOpcode::LoadWide {
                    location,
                    byte_size,
                    width,
                } => ExprOpcode::LoadWide {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                    width,
                },
                ExprOpcode::BinOp(op) => ExprOpcode::BinOp(op),
                ExprOpcode::TypedBinOp {
                    op,
                    lhs_width,
                    rhs_width,
                    result_width,
                    lhs_signed,
                    rhs_signed,
                } => ExprOpcode::TypedBinOp {
                    op,
                    lhs_width,
                    rhs_width,
                    result_width,
                    lhs_signed,
                    rhs_signed,
                },
                ExprOpcode::TypedUnary {
                    op,
                    operand_width,
                    result_width,
                } => ExprOpcode::TypedUnary {
                    op,
                    operand_width,
                    result_width,
                },
                ExprOpcode::Resize {
                    source_width,
                    target_width,
                    signed,
                } => ExprOpcode::Resize {
                    source_width,
                    target_width,
                    signed,
                },
                ExprOpcode::ConcatPart {
                    part_width,
                    result_width,
                } => ExprOpcode::ConcatPart {
                    part_width,
                    result_width,
                },
                ExprOpcode::Ternary { then_len, else_len } => {
                    ExprOpcode::Ternary { then_len, else_len }
                }
                ExprOpcode::LoadIndexed {
                    location,
                    stride_bytes,
                    element_byte_size,
                    element_width,
                } => ExprOpcode::LoadIndexed {
                    location: bind_location(location, &mut resolve)?,
                    stride_bytes,
                    element_byte_size,
                    element_width,
                },
                ExprOpcode::LoadBitSelect {
                    location,
                    base_byte_size,
                    select_width,
                } => ExprOpcode::LoadBitSelect {
                    location: bind_location(location, &mut resolve)?,
                    base_byte_size,
                    select_width,
                },
                ExprOpcode::StoreU64 {
                    location,
                    byte_size,
                } => ExprOpcode::StoreU64 {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                },
            };
            ops.push(bound);
        }
        Ok(ExprBytecode::new(ops))
    }
}

fn bind_location<A>(
    location: StateLocation<A>,
    resolve: &mut impl FnMut(&A) -> Option<usize>,
) -> Result<usize, BindError<A>> {
    let Some(base) = resolve(&location.address) else {
        return Err(BindError {
            address: location.address,
        });
    };
    Ok(base + location.byte_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_uses_source_independent_operators() {
        let code: ExprBytecode = ExprBytecode::new(vec![
            ExprOpcode::ConstU64(1),
            ExprOpcode::ConstU64(2),
            ExprOpcode::TypedBinOp {
                op: TestbenchOperator::Add,
                lhs_width: 8,
                rhs_width: 8,
                result_width: 8,
                lhs_signed: false,
                rhs_signed: false,
            },
        ]);
        assert_eq!(code.ops().len(), 3);
        assert!(!code.is_empty());
    }

    #[test]
    fn semantic_state_locations_bind_after_layout() {
        let code = ExprBytecode::new(vec![ExprOpcode::LoadU64 {
            location: StateLocation {
                address: 7u32,
                byte_offset: 3,
            },
            byte_size: 2,
            mask: 0xffff,
        }]);

        let bound = code
            .bind_with(|address| (*address == 7).then_some(100))
            .unwrap();
        assert_eq!(
            bound.ops(),
            &[ExprOpcode::LoadU64 {
                location: 103,
                byte_size: 2,
                mask: 0xffff,
            }]
        );
    }

    #[test]
    fn binding_reports_an_unmapped_semantic_address() {
        let code = ExprBytecode::new(vec![ExprOpcode::StoreU64 {
            location: StateLocation {
                address: 9u32,
                byte_offset: 0,
            },
            byte_size: 1,
        }]);

        assert_eq!(code.bind_with(|_| None), Err(BindError { address: 9 }));
    }

    #[test]
    fn control_program_is_independent_of_runtime_handles() {
        type SemanticStatement = TestbenchStatement<u32, u32, ExprBytecode<StateLocation<u32>>, ()>;

        let statement = SemanticStatement::If {
            expr: ExprBytecode::new(vec![ExprOpcode::LoadU64 {
                location: StateLocation {
                    address: 1,
                    byte_offset: 0,
                },
                byte_size: 1,
                mask: 1,
            }]),
            then_block: vec![SemanticStatement::ClockNext {
                clock_event: 2,
                count: ClockCount::Static(1),
            }],
            else_block: vec![SemanticStatement::Finish],
        };

        assert!(matches!(statement, SemanticStatement::If { .. }));
    }
}
