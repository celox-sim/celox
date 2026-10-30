//! Source-independent testbench bytecode contracts.
//!
//! Frontends translate source-language operators into this vocabulary before
//! execution. No Veryl AST, parser ID, simulator backend, or runtime handle is
//! part of the bytecode representation.

mod scheduler;
pub use scheduler::*;

use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod expression;
#[cfg(all(test, target_arch = "x86_64"))]
mod expression_tests;
mod format;
pub mod lowering;
mod value;
pub use expression::CompiledExpr;
pub use format::{
    DisplayFormatArg, FieldSpec, MAX_FIELD_WIDTH, format_display_arg, format_sized_display_arg,
    format_sv_display_arg, format_veryl_display_arg, pad_veryl_field,
};
pub use value::TestbenchValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateLocation<A> {
    pub address: A,
    pub byte_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssertMessage<Argument> {
    Formatted {
        template: String,
        args: Vec<Argument>,
    },
    DynamicArgs(Vec<Argument>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockCount<Expression> {
    Static(u64),
    Dynamic(Expression),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopBound<Expression> {
    Static(usize),
    Dynamic {
        expr: Expression,
        width: usize,
        signed: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentParameterValue {
    Bits { words: Vec<u64>, width: u32 },
    String(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchComponent {
    pub instance: String,
    pub component: String,
    pub params: Vec<(String, ComponentParameterValue)>,
    pub connections: Vec<ComponentConnection>,
    pub is_var_form: bool,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentLibrary {
    pub export: String,
    pub type_name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentBinding<Event, Signal, Expression> {
    pub instance: String,
    pub connections: Vec<ComponentConnectionBinding<Event, Signal, Expression>>,
}

/// Source-independent testbench control program.
///
/// Frontends instantiate this with semantic state/event identities and
/// unbound expressions. Runtime binding instantiates the same contract with
/// backend event/signal handles and executable expressions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestbenchStatement<Event, Signal, Expression, Argument, Target = Signal> {
    ClockNext {
        clock_event: Event,
        count: ClockCount<Expression>,
        /// Relative clock period for concurrent scheduling, clamped to at least two.
        period: u64,
    },
    ResetAssert {
        reset_signal: Signal,
        reset_event: Option<Event>,
        clock_event: Event,
        duration: ClockCount<Expression>,
        period: u64,
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
    /// Apply preloaded memory data at this point in the testbench program.
    WriteMemory {
        signal: Signal,
        writes: Vec<celox_design::InitialStateWriteRun>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SemanticSignal<A> {
    pub address: A,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchSelection<Expression> {
    pub offset: Expression,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchTarget<Signal, Expression> {
    pub signal: Signal,
    pub selection: Option<TestbenchSelection<Expression>>,
    pub width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticArgument<A> {
    pub expr: ExprBytecode<StateLocation<A>>,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
    #[serde(default)]
    pub float_precision: Option<celox_design::FloatPrecision>,
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

/// A `$tb::clock_gen` instance: a clock that runs from time 0, rising at
/// multiples of `period` and falling `ceil(period / 2)` after each rise,
/// whether or not a process waits on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchClock<Event> {
    pub event: Event,
    /// At least two.
    pub period: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelRequest<A> {
    Statement(usize),
    /// Settle the DUT before the TB reads `reads`, the storage of DUT
    /// signals its next expression loads.
    Settle {
        reads: Vec<A>,
    },
    Written {
        address: A,
        width: usize,
    },
    Error(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchKernel<A> {
    pub entry: usize,
    pub resume: A,
    pub status: A,
    pub request: A,
    pub requests: Vec<KernelRequest<A>>,
    /// The runtime event site of each `$display` statement, by statement
    /// site.
    #[serde(default)]
    pub display_sites: Vec<(usize, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchProgram<A> {
    kernels: Vec<TestbenchKernel<A>>,
    scheduler: Option<TestbenchScheduler<A>>,
    statements: Vec<SemanticStatement<A>>,
    process_lengths: Vec<usize>,
    clocks: Vec<TestbenchClock<A>>,
    private_variables: Vec<A>,
    random_seed: Option<u64>,
    components: Vec<TestbenchComponent>,
    component_libraries: Vec<ComponentLibrary>,
    component_file_base: Option<PathBuf>,
    component_bindings: Vec<SemanticComponentBinding<A>>,
}

impl<A> Default for TestbenchProgram<A> {
    fn default() -> Self {
        Self {
            kernels: Vec::new(),
            scheduler: None,
            statements: Vec::new(),
            process_lengths: Vec::new(),
            clocks: Vec::new(),
            private_variables: Vec::new(),
            random_seed: None,
            components: Vec::new(),
            component_libraries: Vec::new(),
            component_file_base: None,
            component_bindings: Vec::new(),
        }
    }
}

impl<A> TestbenchProgram<A> {
    pub fn scheduler(&self) -> Option<&TestbenchScheduler<A>> {
        self.scheduler.as_ref()
    }
    pub fn set_scheduler(&mut self, scheduler: TestbenchScheduler<A>) {
        self.scheduler = Some(scheduler);
    }
    pub fn kernels(&self) -> &[TestbenchKernel<A>] {
        &self.kernels
    }
    pub fn set_kernels(&mut self, kernels: Vec<TestbenchKernel<A>>) {
        self.kernels = kernels;
    }

    pub fn new(statements: Vec<SemanticStatement<A>>) -> Self {
        Self {
            kernels: Vec::new(),
            scheduler: None,
            process_lengths: vec![statements.len()],
            clocks: Vec::new(),
            private_variables: Vec::new(),
            statements,
            random_seed: None,
            components: Vec::new(),
            component_libraries: Vec::new(),
            component_file_base: None,
            component_bindings: Vec::new(),
        }
    }

    /// Build independently scheduled initial processes in declaration order.
    pub fn from_processes(processes: Vec<Vec<SemanticStatement<A>>>) -> Self {
        let lengths = processes.iter().map(Vec::len).collect();
        let mut program = Self::new(processes.into_iter().flatten().collect());
        program.process_lengths = lengths;
        program
    }

    /// The clock generators of the testbench.
    pub fn with_clocks(mut self, clocks: Vec<TestbenchClock<A>>) -> Self {
        self.clocks = clocks;
        self
    }

    pub fn clocks(&self) -> &[TestbenchClock<A>] {
        &self.clocks
    }

    /// Temporaries whose values belong to each independently suspended process.
    pub fn with_private_variables(mut self, variables: Vec<A>) -> Self {
        self.private_variables = variables;
        self
    }

    pub fn private_variables(&self) -> &[A] {
        &self.private_variables
    }

    pub fn processes(&self) -> impl Iterator<Item = &[SemanticStatement<A>]> {
        self.process_lengths.iter().scan(0, |offset, &length| {
            let start = *offset;
            *offset += length;
            Some(&self.statements[start..*offset])
        })
    }
    pub fn process_lengths(&self) -> &[usize] {
        &self.process_lengths
    }

    pub fn with_random_seed(mut self, random_seed: u64) -> Self {
        self.random_seed = Some(random_seed);
        self
    }

    pub fn with_random_seed_option(mut self, random_seed: Option<u64>) -> Self {
        self.random_seed = random_seed;
        self
    }

    pub fn statements_mut(&mut self) -> &mut [SemanticStatement<A>] {
        &mut self.statements
    }

    pub fn component_bindings_mut(&mut self) -> &mut [SemanticComponentBinding<A>] {
        &mut self.component_bindings
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

    /// Remap filesystem paths retained for runtime libraries and diagnostics.
    /// Literal strings supplied by the design are left untouched.
    /// A failed mapping can leave some paths remapped; discard that program.
    pub fn try_map_paths<E>(
        &mut self,
        map: &mut impl FnMut(&std::path::Path) -> Result<PathBuf, E>,
    ) -> Result<(), E> {
        fn location<E>(
            source: &mut Option<SourceLocation>,
            map: &mut impl FnMut(&std::path::Path) -> Result<PathBuf, E>,
        ) -> Result<(), E> {
            if let Some(source) = source
                && !source.file.is_empty()
            {
                source.file = map(std::path::Path::new(&source.file))?
                    .to_string_lossy()
                    .into_owned();
            }
            Ok(())
        }
        fn statements<A, E>(
            block: &mut [SemanticStatement<A>],
            map: &mut impl FnMut(&std::path::Path) -> Result<PathBuf, E>,
        ) -> Result<(), E> {
            for statement in block {
                match statement {
                    TestbenchStatement::Assert {
                        location: source, ..
                    } => location(source, map)?,
                    TestbenchStatement::If {
                        then_block,
                        else_block,
                        ..
                    } => {
                        statements(then_block, map)?;
                        statements(else_block, map)?;
                    }
                    TestbenchStatement::For { body, .. } => statements(body, map)?,
                    _ => {}
                }
            }
            Ok(())
        }
        for library in &mut self.component_libraries {
            library.path = map(&library.path)?;
        }
        if let Some(base) = &mut self.component_file_base {
            *base = map(base)?;
        }
        for component in &mut self.components {
            location(&mut component.source, map)?;
        }
        statements(&mut self.statements, map)
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    FloatBinary {
        op: celox_design::FloatBinaryOp,
        precision: celox_design::FloatPrecision,
    },
    FloatUnary {
        op: celox_design::FloatUnaryOp,
        precision: celox_design::FloatPrecision,
    },
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
    ConstString(Vec<u8>),
    /// Pop the bit offset of a string element within the variable at
    /// `location`, which holds `elements` heap handles, and push its contents.
    /// An offset past the last element reads `""`.
    LoadString {
        location: L,
        elements: usize,
    },
    /// Pop a string and store its heap handle at `location`.
    StoreString {
        location: L,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "L: Serialize", deserialize = "L: Deserialize<'de>"))]
pub struct ExprBytecode<L = usize> {
    ops: Vec<ExprOpcode<L>>,
    #[serde(default)]
    kernel: Option<ExpressionKernel<L>>,
}

impl<L> ExprBytecode<L> {
    pub fn new(ops: Vec<ExprOpcode<L>>) -> Self {
        Self { ops, kernel: None }
    }

    pub fn kernel(&self) -> Option<&ExpressionKernel<L>> {
        self.kernel.as_ref()
    }
    pub fn set_kernel(&mut self, kernel: ExpressionKernel<L>) {
        self.kernel = Some(kernel);
    }

    /// Fold literal casts without executing a testbench expression.
    pub fn literal_u64(&self) -> Option<u64> {
        let [ExprOpcode::ConstU64(value), rest @ ..] = self.ops.as_slice() else {
            return None;
        };
        let mut value = *value;
        for op in rest {
            let ExprOpcode::Resize {
                source_width,
                target_width,
                signed,
            } = op
            else {
                return None;
            };
            if *source_width > 64 || *target_width > 64 {
                return None;
            }
            let mask = |width: usize| {
                if width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << width) - 1
                }
            };
            if *source_width == 0 {
                value = if value & 1 != 0 {
                    mask(*target_width)
                } else {
                    0
                };
            } else {
                value &= mask(*source_width);
                if *signed && *target_width > *source_width && value >> (source_width - 1) != 0 {
                    value |= !mask(*source_width);
                }
                value &= mask(*target_width);
            }
        }
        Some(value)
    }
    pub fn ops(&self) -> &[ExprOpcode<L>] {
        &self.ops
    }

    /// Whether evaluating the expression writes state, as a function call
    /// that assigns a variable does.
    pub fn writes_state(&self) -> bool {
        self.ops.iter().any(|op| {
            matches!(
                op,
                ExprOpcode::StoreU64 { .. } | ExprOpcode::StoreString { .. }
            )
        })
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// A native expression entry point and its ordinary state result slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpressionKernel<L> {
    pub constant: Option<u64>,
    pub entry: usize,
    pub result: L,
    pub width: usize,
    pub is_string: bool,
    pub writes: Vec<(L, usize)>,
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
    pub float_precision: Option<celox_design::FloatPrecision>,
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
    kernels: Vec<TestbenchKernel<Signal>>,
    scheduler: Option<TestbenchScheduler<Signal>>,
    statements: Vec<ExecutableStatement<Event, Signal>>,
    process_lengths: Vec<usize>,
    clocks: Vec<TestbenchClock<Event>>,
    private_signals: Vec<Signal>,
    random_seed: Option<u64>,
    components: Vec<TestbenchComponent>,
    component_libraries: Vec<ComponentLibrary>,
    component_file_base: Option<PathBuf>,
    component_bindings: Vec<ExecutableComponentBinding<Event, Signal>>,
}

impl<Event, Signal> ExecutableTestbench<Event, Signal> {
    pub fn scheduler(&self) -> Option<&TestbenchScheduler<Signal>> {
        self.scheduler.as_ref()
    }
    pub fn with_scheduler(mut self, scheduler: Option<TestbenchScheduler<Signal>>) -> Self {
        self.scheduler = scheduler;
        self
    }
    pub fn kernels(&self) -> &[TestbenchKernel<Signal>] {
        &self.kernels
    }
    pub fn with_kernels(mut self, kernels: Vec<TestbenchKernel<Signal>>) -> Self {
        self.kernels = kernels;
        self
    }

    pub fn new(statements: Vec<ExecutableStatement<Event, Signal>>, random_seed: u64) -> Self {
        Self::new_with_random_seed(statements, Some(random_seed))
    }

    pub fn new_with_random_seed(
        statements: Vec<ExecutableStatement<Event, Signal>>,
        random_seed: Option<u64>,
    ) -> Self {
        Self {
            kernels: Vec::new(),
            scheduler: None,
            process_lengths: vec![statements.len()],
            clocks: Vec::new(),
            private_signals: Vec::new(),
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

    /// Preserve process boundaries after binding a semantic program.
    pub fn from_processes(
        processes: Vec<Vec<ExecutableStatement<Event, Signal>>>,
        random_seed: Option<u64>,
    ) -> Self {
        let lengths = processes.iter().map(Vec::len).collect();
        let mut program =
            Self::new_with_random_seed(processes.into_iter().flatten().collect(), random_seed);
        program.process_lengths = lengths;
        program
    }

    /// The clock generators of the testbench.
    pub fn with_clocks(mut self, clocks: Vec<TestbenchClock<Event>>) -> Self {
        self.clocks = clocks;
        self
    }

    pub fn clocks(&self) -> &[TestbenchClock<Event>] {
        &self.clocks
    }

    pub fn with_private_signals(mut self, signals: Vec<Signal>) -> Self {
        self.private_signals = signals;
        self
    }

    pub fn private_signals(&self) -> &[Signal] {
        &self.private_signals
    }

    pub fn processes(&self) -> impl Iterator<Item = &[ExecutableStatement<Event, Signal>]> {
        self.process_lengths.iter().scan(0, |offset, &length| {
            let start = *offset;
            *offset += length;
            Some(&self.statements[start..*offset])
        })
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

impl<L> ExprBytecode<L> {
    pub fn try_map_locations<M, E>(
        self,
        mut map: impl FnMut(L) -> Result<M, E>,
    ) -> Result<ExprBytecode<M>, E> {
        let mut ops = Vec::with_capacity(self.ops.len());
        for op in self.ops {
            let bound = match op {
                ExprOpcode::ConstString(value) => ExprOpcode::ConstString(value),
                ExprOpcode::LoadString { location, elements } => ExprOpcode::LoadString {
                    location: map(location)?,
                    elements,
                },
                ExprOpcode::StoreString { location } => ExprOpcode::StoreString {
                    location: map(location)?,
                },
                ExprOpcode::ConstU64(value) => ExprOpcode::ConstU64(value),
                ExprOpcode::ConstWide(value) => ExprOpcode::ConstWide(value),
                ExprOpcode::LoadU64 {
                    location,
                    byte_size,
                    mask,
                } => ExprOpcode::LoadU64 {
                    location: map(location)?,
                    byte_size,
                    mask,
                },
                ExprOpcode::LoadWide {
                    location,
                    byte_size,
                    width,
                } => ExprOpcode::LoadWide {
                    location: map(location)?,
                    byte_size,
                    width,
                },
                ExprOpcode::BinOp(op) => ExprOpcode::BinOp(op),
                ExprOpcode::FloatBinary { op, precision } => {
                    ExprOpcode::FloatBinary { op, precision }
                }
                ExprOpcode::FloatUnary { op, precision } => {
                    ExprOpcode::FloatUnary { op, precision }
                }
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
                    location: map(location)?,
                    stride_bits,
                    base_bit_offset,
                    element_width,
                },
                ExprOpcode::LoadBitSelect {
                    location,
                    base_byte_size,
                    select_width,
                } => ExprOpcode::LoadBitSelect {
                    location: map(location)?,
                    base_byte_size,
                    select_width,
                },
                ExprOpcode::StoreU64 {
                    location,
                    byte_size,
                } => ExprOpcode::StoreU64 {
                    location: map(location)?,
                    byte_size,
                },
            };
            ops.push(bound);
        }
        let kernel = self
            .kernel
            .map(|kernel| -> Result<_, E> {
                Ok(ExpressionKernel {
                    constant: kernel.constant,
                    entry: kernel.entry,
                    result: map(kernel.result)?,
                    width: kernel.width,
                    is_string: kernel.is_string,
                    writes: kernel
                        .writes
                        .into_iter()
                        .map(|(location, size)| Ok((map(location)?, size)))
                        .collect::<Result<_, E>>()?,
                })
            })
            .transpose()?;
        Ok(ExprBytecode { ops, kernel })
    }
}

impl<A> ExprBytecode<StateLocation<A>> {
    pub fn bind_with(
        self,
        mut resolve: impl FnMut(&A) -> Option<usize>,
    ) -> Result<ExprBytecode, BindError<A>> {
        self.try_map_locations(|location| bind_location(location, &mut resolve))
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
                period: 2,
            }],
            else_block: vec![SemanticStatement::Finish],
        };

        assert!(matches!(statement, SemanticStatement::If { .. }));
    }
}

mod visit;
