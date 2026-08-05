//! Source-independent testbench bytecode contracts.
//!
//! Frontends translate source-language operators into this vocabulary before
//! execution. No Veryl AST, parser ID, simulator backend, or runtime handle is
//! part of the bytecode representation.

use num_bigint::BigUint;
use std::path::PathBuf;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentParameterValue {
    Bits { words: Vec<u64>, width: u32 },
    String(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentConnection {
    pub port: String,
    pub group: Option<String>,
    pub member: Option<String>,
    pub input: bool,
    pub has_output: bool,
    pub is_clock: bool,
    pub is_reset: bool,
    pub width: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestbenchComponent {
    pub instance: String,
    pub component: String,
    pub params: Vec<(String, ComponentParameterValue)>,
    pub connections: Vec<ComponentConnection>,
    pub is_var_form: bool,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentLibrary {
    pub export: String,
    pub type_name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentConnectionBinding<Event, Signal, Expression> {
    pub port: String,
    pub input: Option<Expression>,
    /// Direct mask source for a variable/select input connection. Runtime
    /// hosts evaluate `input` for the value and use this target to preserve
    /// the corresponding four-state mask.
    pub input_target: Option<TestbenchTarget<Signal, Expression>>,
    pub output: Option<TestbenchTarget<Signal, Expression>>,
    pub output_rtl_driven: bool,
    pub event: Option<Event>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentBinding<Event, Signal, Expression> {
    pub instance: String,
    pub connections: Vec<ComponentConnectionBinding<Event, Signal, Expression>>,
}

/// Source-independent testbench control program.
///
/// Frontends instantiate this with semantic state/event identities and
/// unbound expressions. Runtime binding instantiates the same contract with
/// backend event/signal handles and executable expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestbenchStatement<Event, Signal, Expression, Argument, Target = Signal> {
    ClockNext {
        clock_event: Event,
        count: ClockCount<Expression>,
    },
    ResetAssert {
        reset_signal: Signal,
        reset_event: Option<Event>,
        clock_event: Event,
        duration: ClockCount<Expression>,
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
        dst: Target,
        expr: Expression,
    },
    RandomSeed {
        handle: String,
        value: Expression,
    },
    RandomGet {
        handle: String,
        width: u32,
        signed: bool,
        ret: Option<Target>,
    },
    RandomGetRange {
        handle: String,
        min: Expression,
        max: Expression,
        width: u32,
        signed: bool,
        ret: Option<Target>,
    },
    RandomGetSeed {
        handle: String,
        ret: Option<Target>,
    },
    ComponentMethod {
        instance: String,
        method: String,
        args: Vec<Argument>,
        ret: Option<Target>,
        ret_width: Option<u32>,
        ret_signed: bool,
        ret_strict: bool,
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
pub struct TestbenchSelection<Expression> {
    pub offset: Expression,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestbenchTarget<Signal, Expression> {
    pub signal: Signal,
    pub selection: Option<TestbenchSelection<Expression>>,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticArgument<A> {
    pub expr: ExprBytecode<StateLocation<A>>,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
}

pub type SemanticStatement<A> = TestbenchStatement<
    A,
    SemanticSignal<A>,
    ExprBytecode<StateLocation<A>>,
    SemanticArgument<A>,
    TestbenchTarget<SemanticSignal<A>, ExprBytecode<StateLocation<A>>>,
>;
pub type SemanticComponentBinding<A> =
    ComponentBinding<A, SemanticSignal<A>, ExprBytecode<StateLocation<A>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestbenchProgram<A> {
    statements: Vec<SemanticStatement<A>>,
    random_seed: Option<u64>,
    components: Vec<TestbenchComponent>,
    component_libraries: Vec<ComponentLibrary>,
    component_file_base: Option<PathBuf>,
    component_bindings: Vec<SemanticComponentBinding<A>>,
}

impl<A> Default for TestbenchProgram<A> {
    fn default() -> Self {
        Self {
            statements: Vec::new(),
            random_seed: None,
            components: Vec::new(),
            component_libraries: Vec::new(),
            component_file_base: None,
            component_bindings: Vec::new(),
        }
    }
}

impl<A> TestbenchProgram<A> {
    pub fn new(statements: Vec<SemanticStatement<A>>) -> Self {
        Self {
            statements,
            random_seed: None,
            components: Vec::new(),
            component_libraries: Vec::new(),
            component_file_base: None,
            component_bindings: Vec::new(),
        }
    }

    pub fn with_random_seed(mut self, random_seed: u64) -> Self {
        self.random_seed = Some(random_seed);
        self
    }

    pub fn with_random_seed_option(mut self, random_seed: Option<u64>) -> Self {
        self.random_seed = random_seed;
        self
    }

    pub fn statements(&self) -> &[SemanticStatement<A>] {
        &self.statements
    }

    pub fn into_statements(self) -> Vec<SemanticStatement<A>> {
        self.statements
    }

    pub fn random_seed(&self) -> u64 {
        self.random_seed.unwrap_or_default()
    }

    pub fn configured_random_seed(&self) -> Option<u64> {
        self.random_seed
    }

    pub fn with_components(mut self, components: Vec<TestbenchComponent>) -> Self {
        self.components = components;
        self
    }

    pub fn with_component_runtime(
        mut self,
        libraries: Vec<ComponentLibrary>,
        file_base: Option<PathBuf>,
        bindings: Vec<SemanticComponentBinding<A>>,
    ) -> Self {
        self.component_libraries = libraries;
        self.component_file_base = file_base;
        self.component_bindings = bindings;
        self
    }

    pub fn components(&self) -> &[TestbenchComponent] {
        &self.components
    }

    pub fn component_libraries(&self) -> &[ComponentLibrary] {
        &self.component_libraries
    }

    pub fn component_file_base(&self) -> Option<&std::path::Path> {
        self.component_file_base.as_deref()
    }

    pub fn component_bindings(&self) -> &[SemanticComponentBinding<A>] {
        &self.component_bindings
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
        stride_bits: usize,
        base_bit_offset: usize,
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
pub type ExecutableStatement<Event, Signal> = TestbenchStatement<
    Event,
    Signal,
    CompiledExpr,
    ExecutableArgument,
    TestbenchTarget<Signal, CompiledExpr>,
>;
pub type ExecutableComponentBinding<Event, Signal> = ComponentBinding<Event, Signal, CompiledExpr>;

pub struct ExecutableTestbench<Event, Signal> {
    statements: Vec<ExecutableStatement<Event, Signal>>,
    random_seed: Option<u64>,
    components: Vec<TestbenchComponent>,
    component_libraries: Vec<ComponentLibrary>,
    component_file_base: Option<PathBuf>,
    component_bindings: Vec<ExecutableComponentBinding<Event, Signal>>,
}

impl<Event, Signal> ExecutableTestbench<Event, Signal> {
    pub fn new(statements: Vec<ExecutableStatement<Event, Signal>>, random_seed: u64) -> Self {
        Self::new_with_random_seed(statements, Some(random_seed))
    }

    pub fn new_with_random_seed(
        statements: Vec<ExecutableStatement<Event, Signal>>,
        random_seed: Option<u64>,
    ) -> Self {
        Self {
            statements,
            random_seed,
            components: Vec::new(),
            component_libraries: Vec::new(),
            component_file_base: None,
            component_bindings: Vec::new(),
        }
    }

    pub fn with_component_runtime(
        mut self,
        components: Vec<TestbenchComponent>,
        libraries: Vec<ComponentLibrary>,
        file_base: Option<PathBuf>,
        bindings: Vec<ExecutableComponentBinding<Event, Signal>>,
    ) -> Self {
        self.components = components;
        self.component_libraries = libraries;
        self.component_file_base = file_base;
        self.component_bindings = bindings;
        self
    }

    pub fn statements(&self) -> &[ExecutableStatement<Event, Signal>] {
        &self.statements
    }

    pub fn into_statements(self) -> Vec<ExecutableStatement<Event, Signal>> {
        self.statements
    }

    pub fn random_seed(&self) -> u64 {
        self.random_seed.unwrap_or_default()
    }

    pub fn configured_random_seed(&self) -> Option<u64> {
        self.random_seed
    }

    pub fn components(&self) -> &[TestbenchComponent] {
        &self.components
    }

    pub fn component_libraries(&self) -> &[ComponentLibrary] {
        &self.component_libraries
    }

    pub fn component_file_base(&self) -> Option<&std::path::Path> {
        self.component_file_base.as_deref()
    }

    pub fn component_bindings(&self) -> &[ExecutableComponentBinding<Event, Signal>] {
        &self.component_bindings
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
                    stride_bits,
                    base_bit_offset,
                    element_width,
                } => ExprOpcode::LoadIndexed {
                    location: bind_location(location, &mut resolve)?,
                    stride_bits,
                    base_bit_offset,
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
