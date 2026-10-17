//! The system tasks and functions Celox knows (those IEEE 1800-2023 defines,
//! mostly in clauses 20 and 21, and the optional ones of its Annex D), and
//! where the SystemVerilog frontend supports each one.
//!
//! The analyzer checks each call against this catalog where it converts the
//! call, with the context it converts it in ([`CallSite`]), so a name the
//! frontend cannot lower is reported by name instead of being dropped or
//! reported as a generic expression error. Names that are not in the catalog
//! are rejected as unknown.
//!
//! A supported call is identified by [`SystemFunction`] or [`SystemTask`]
//! from then on: the IR and the frontend dispatch on these, never on names.

use std::fmt;

use crate::AnalyzerError;

/// A system function the frontend supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemFunction {
    /// `$timeunit` (20.4.2).
    Timeunit,
    /// `$timeprecision` (20.4.2).
    Timeprecision,
    /// `$signed` (11.7).
    Signed,
    /// `$unsigned` (11.7).
    Unsigned,
    /// `$bits` (20.6.2).
    Bits,
    /// `$isunbounded` (20.6.3).
    Isunbounded,
    /// `$dimensions` (20.7).
    Dimensions,
    /// `$unpacked_dimensions` (20.7).
    UnpackedDimensions,
    /// `$size` (20.7).
    Size,
    /// `$left`, `$right`, `$low`, `$high`, `$increment` (20.7).
    ArrayQuery(ArrayQuery),
    /// `$clog2` (20.8.1).
    Clog2,
    /// `$countbits` (20.9).
    Countbits,
    /// `$countones` (20.9).
    Countones,
    /// `$onehot` (20.9).
    Onehot,
    /// `$onehot0` (20.9).
    Onehot0,
    /// `$isunknown` (20.9).
    Isunknown,
    /// `$random` (20.14.1), with the algorithm of Annex N.
    Random,
    /// `$urandom` (18.13.1).
    Urandom,
    /// `$urandom_range` (18.13.2).
    UrandomRange,
}

/// An array query function that selects one bound of a dimension (20.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayQuery {
    Left,
    Right,
    Low,
    High,
    Increment,
}

impl SystemFunction {
    /// The name, with its leading `$`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Timeunit => "$timeunit",
            Self::Timeprecision => "$timeprecision",
            Self::Signed => "$signed",
            Self::Unsigned => "$unsigned",
            Self::Bits => "$bits",
            Self::Isunbounded => "$isunbounded",
            Self::Dimensions => "$dimensions",
            Self::UnpackedDimensions => "$unpacked_dimensions",
            Self::Size => "$size",
            Self::ArrayQuery(ArrayQuery::Left) => "$left",
            Self::ArrayQuery(ArrayQuery::Right) => "$right",
            Self::ArrayQuery(ArrayQuery::Low) => "$low",
            Self::ArrayQuery(ArrayQuery::High) => "$high",
            Self::ArrayQuery(ArrayQuery::Increment) => "$increment",
            Self::Clog2 => "$clog2",
            Self::Countbits => "$countbits",
            Self::Countones => "$countones",
            Self::Onehot => "$onehot",
            Self::Onehot0 => "$onehot0",
            Self::Isunknown => "$isunknown",
            Self::Random => "$random",
            Self::Urandom => "$urandom",
            Self::UrandomRange => "$urandom_range",
        }
    }

    /// The supported function named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        match lookup(name)?.support {
            Support::Function(function) => Some(function),
            Support::Task(..) | Support::Unsupported => None,
        }
    }

    /// The queries of a data object's or type's shape (20.6.2, 20.7), whose
    /// operand is not evaluated.
    pub fn is_data_query(self) -> bool {
        matches!(
            self,
            Self::Bits
                | Self::Size
                | Self::Dimensions
                | Self::UnpackedDimensions
                | Self::ArrayQuery(_)
        )
    }

    /// Whether each call has an effect, so that copies of one call made by
    /// the analyzer must not run it twice.
    pub fn is_effectful(self) -> bool {
        matches!(self, Self::Random | Self::Urandom | Self::UrandomRange)
    }

    /// Whether the call writes argument `position` back, as `$random(seed)`
    /// updates its seed variable.
    pub fn writes_argument(self, position: usize) -> bool {
        self == Self::Random && position == 0
    }

    /// The width and signedness of a call that is lowered as an integral
    /// operation. `None` for `$signed` and `$unsigned`, whose type follows
    /// their operand, and for the data queries, which the analyzer folds to
    /// literals where it converts them.
    pub fn result_type(self) -> Option<(usize, bool)> {
        match self {
            Self::Timeunit
            | Self::Timeprecision
            | Self::Clog2
            | Self::Countbits
            | Self::Countones => Some((32, true)),
            Self::Onehot | Self::Onehot0 | Self::Isunknown | Self::Isunbounded => Some((1, false)),
            Self::Random => Some((32, true)),
            Self::Urandom | Self::UrandomRange => Some((32, false)),
            Self::Signed
            | Self::Unsigned
            | Self::Bits
            | Self::Dimensions
            | Self::UnpackedDimensions
            | Self::Size
            | Self::ArrayQuery(_) => None,
        }
    }
}

impl fmt::Display for SystemFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A system task the frontend supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemTask {
    /// `$finish` (20.2).
    Finish,
    /// `$printtimescale` (20.4.1); the analyzer rewrites it into a
    /// [`SystemTask::Display`] of the message.
    Printtimescale,
    /// `$fatal` (20.10).
    Fatal,
    /// `$error`, `$warning`, `$info` (20.10).
    Severity(Severity),
    /// `$display` and its `b`, `o` and `h` variants (21.2.1).
    Display(Radix),
    /// `$write` and its `b`, `o` and `h` variants (21.2.1).
    Write(Radix),
    /// `$readmemb` and `$readmemh` (21.4).
    Readmem(MemoryFormat),
    /// Veryl's immediate assertions, emitted into SystemVerilog as calls of
    /// `$assert(condition, message...)`, which ends the simulation when the
    /// condition fails, and `$assert_continue`, which reports and continues.
    Assert,
    AssertContinue,
}

/// The severity of `$error`, `$warning` and `$info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// The number format of a memory file read by `$readmemb` or `$readmemh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryFormat {
    Binary,
    Hexadecimal,
}

impl MemoryFormat {
    /// The base of the numbers in the file.
    pub const fn base(self) -> u32 {
        match self {
            Self::Binary => 2,
            Self::Hexadecimal => 16,
        }
    }
}

/// The default radix of a display task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Radix {
    Binary,
    Octal,
    Decimal,
    Hexadecimal,
}

impl Radix {
    /// The format letter of the radix, as in `%h`.
    pub const fn letter(self) -> char {
        match self {
            Self::Binary => 'b',
            Self::Octal => 'o',
            Self::Decimal => 'd',
            Self::Hexadecimal => 'h',
        }
    }
}

impl SystemTask {
    /// The name, with its leading `$`.
    pub const fn name(self) -> &'static str {
        use Radix::{Binary, Decimal, Hexadecimal, Octal};
        match self {
            Self::Finish => "$finish",
            Self::Printtimescale => "$printtimescale",
            Self::Fatal => "$fatal",
            Self::Severity(Severity::Error) => "$error",
            Self::Severity(Severity::Warning) => "$warning",
            Self::Severity(Severity::Info) => "$info",
            Self::Display(Decimal) => "$display",
            Self::Display(Binary) => "$displayb",
            Self::Display(Octal) => "$displayo",
            Self::Display(Hexadecimal) => "$displayh",
            Self::Write(Decimal) => "$write",
            Self::Write(Binary) => "$writeb",
            Self::Write(Octal) => "$writeo",
            Self::Write(Hexadecimal) => "$writeh",
            Self::Readmem(MemoryFormat::Binary) => "$readmemb",
            Self::Readmem(MemoryFormat::Hexadecimal) => "$readmemh",
            Self::Assert => "$assert",
            Self::AssertContinue => "$assert_continue",
        }
    }

    /// The supported task named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        match lookup(name)?.support {
            Support::Task(task, _) => Some(task),
            Support::Function(_) | Support::Unsupported => None,
        }
    }
}

impl fmt::Display for SystemTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The subroutine a call expression calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A user function or a DPI import, by its resolved name.
    Subroutine(String),
    System(SystemFunction),
}

impl Callee {
    /// The name of the user subroutine, if this is not a system function.
    pub fn subroutine(&self) -> Option<&str> {
        match self {
            Self::Subroutine(name) => Some(name),
            Self::System(_) => None,
        }
    }

    /// The system function, if this is one.
    pub fn system(&self) -> Option<SystemFunction> {
        match self {
            Self::Subroutine(_) => None,
            Self::System(function) => Some(*function),
        }
    }

    /// The width and signedness of a call of a system function that is
    /// lowered as an integral operation; see [`SystemFunction::result_type`].
    pub fn result_type(&self) -> Option<(usize, bool)> {
        self.system()?.result_type()
    }

    /// Rename a user subroutine with `rename`.
    pub fn map_subroutine(self, rename: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Subroutine(name) => Self::Subroutine(rename(name)),
            Self::System(function) => Self::System(function),
        }
    }

    /// Whether each call has an effect, so that copies of one call made by
    /// the analyzer must not run it twice: user subroutines may have side
    /// effects, as do the random number functions.
    pub fn is_effectful(&self) -> bool {
        self.system().is_none_or(SystemFunction::is_effectful)
    }
}

impl fmt::Display for Callee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subroutine(name) => f.write_str(name),
            Self::System(function) => f.write_str(function.name()),
        }
    }
}

/// Whether IEEE 1800-2023 defines the name as a task, a function, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemTfKind {
    Task,
    Function,
    /// Both forms exist, such as `$cast` (IEEE 1800-2023 6.24.2).
    TaskOrFunction,
}

/// Where a supported task may appear as a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementUse {
    /// In `always` processes and subroutines, but not in `initial` blocks.
    Procedural,
    /// Also in `initial` blocks.
    ProceduralAndInitial,
}

/// How the frontend supports a catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Unsupported,
    /// A function, in expressions and as a statement whose value is
    /// discarded, in any procedural body.
    Function(SystemFunction),
    /// A task, as a statement where [`StatementUse`] allows.
    Task(SystemTask, StatementUse),
}

/// One system task or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemTf {
    pub name: &'static str,
    pub kind: SystemTfKind,
    pub support: Support,
    /// The number of arguments Celox accepts, when the call is supported:
    /// at least `min_args`, and at most `max_args` if bounded.
    pub min_args: usize,
    pub max_args: Option<usize>,
}

impl SystemTf {
    /// Whether the call is supported in any position.
    pub fn is_supported(&self) -> bool {
        self.support != Support::Unsupported
    }

    /// "system task" or "system function", for diagnostics about a call in
    /// statement (`statement == true`) or expression position.
    pub fn noun(&self, statement: bool) -> &'static str {
        match (self.kind, statement) {
            (SystemTfKind::Task, _) | (SystemTfKind::TaskOrFunction, true) => "system task",
            (SystemTfKind::Function, _) | (SystemTfKind::TaskOrFunction, false) => {
                "system function"
            }
        }
    }
}

use StatementUse::{Procedural, ProceduralAndInitial};
use SystemTfKind::{Function, Task, TaskOrFunction};

/// A system task or function the frontend does not lower.
const fn unsupported(name: &'static str, kind: SystemTfKind) -> SystemTf {
    SystemTf {
        name,
        kind,
        support: Support::Unsupported,
        min_args: 0,
        max_args: None,
    }
}

/// A supported task.
const fn task(
    task: SystemTask,
    statement: StatementUse,
    min_args: usize,
    max_args: Option<usize>,
) -> SystemTf {
    SystemTf {
        name: task.name(),
        kind: Task,
        support: Support::Task(task, statement),
        min_args,
        max_args,
    }
}

/// A supported function.
const fn function(function: SystemFunction, min_args: usize, max_args: Option<usize>) -> SystemTf {
    SystemTf {
        name: function.name(),
        kind: Function,
        support: Support::Function(function),
        min_args,
        max_args,
    }
}

/// The catalog: IEEE 1800-2023 clauses 20 and 21 in order, then the names
/// other clauses define, then Annex D.
pub const SYSTEM_TFS: &[SystemTf] = &[
    // 20.2 Simulation control system tasks
    task(SystemTask::Finish, ProceduralAndInitial, 0, Some(1)),
    unsupported("$stop", Task),
    unsupported("$exit", Task),
    // 20.3 Simulation time system functions
    unsupported("$time", Function),
    unsupported("$stime", Function),
    unsupported("$realtime", Function),
    // 20.4 Timescale system tasks and system functions
    task(SystemTask::Printtimescale, ProceduralAndInitial, 0, Some(1)),
    unsupported("$timeformat", Task),
    function(SystemFunction::Timeunit, 0, Some(1)),
    function(SystemFunction::Timeprecision, 0, Some(1)),
    // 20.5 Conversion functions
    unsupported("$rtoi", Function),
    unsupported("$itor", Function),
    unsupported("$realtobits", Function),
    unsupported("$bitstoreal", Function),
    unsupported("$shortrealtobits", Function),
    unsupported("$bitstoshortreal", Function),
    // 11.7 Signed expressions
    function(SystemFunction::Signed, 1, Some(1)),
    function(SystemFunction::Unsigned, 1, Some(1)),
    // 6.24.2 Dynamic casting
    unsupported("$cast", TaskOrFunction),
    // 20.6 Data query functions
    unsupported("$typename", Function),
    function(SystemFunction::Bits, 1, Some(1)),
    function(SystemFunction::Isunbounded, 1, Some(1)),
    // 20.7 Array query functions
    function(SystemFunction::Dimensions, 1, Some(1)),
    function(SystemFunction::UnpackedDimensions, 1, Some(1)),
    function(SystemFunction::ArrayQuery(ArrayQuery::Left), 1, Some(2)),
    function(SystemFunction::ArrayQuery(ArrayQuery::Right), 1, Some(2)),
    function(SystemFunction::ArrayQuery(ArrayQuery::Low), 1, Some(2)),
    function(SystemFunction::ArrayQuery(ArrayQuery::High), 1, Some(2)),
    function(
        SystemFunction::ArrayQuery(ArrayQuery::Increment),
        1,
        Some(2),
    ),
    function(SystemFunction::Size, 1, Some(2)),
    // 20.8 Math functions
    function(SystemFunction::Clog2, 1, Some(1)),
    unsupported("$ln", Function),
    unsupported("$log10", Function),
    unsupported("$exp", Function),
    unsupported("$sqrt", Function),
    unsupported("$pow", Function),
    unsupported("$floor", Function),
    unsupported("$ceil", Function),
    unsupported("$sin", Function),
    unsupported("$cos", Function),
    unsupported("$tan", Function),
    unsupported("$asin", Function),
    unsupported("$acos", Function),
    unsupported("$atan", Function),
    unsupported("$atan2", Function),
    unsupported("$hypot", Function),
    unsupported("$sinh", Function),
    unsupported("$cosh", Function),
    unsupported("$tanh", Function),
    unsupported("$asinh", Function),
    unsupported("$acosh", Function),
    unsupported("$atanh", Function),
    // 20.9 Bit vector system functions
    function(SystemFunction::Countbits, 2, None),
    function(SystemFunction::Countones, 1, Some(1)),
    function(SystemFunction::Onehot, 1, Some(1)),
    function(SystemFunction::Onehot0, 1, Some(1)),
    function(SystemFunction::Isunknown, 1, Some(1)),
    // 20.10 Severity system tasks (and 20.10.1 elaboration severity system
    // tasks)
    task(SystemTask::Fatal, ProceduralAndInitial, 0, None),
    task(
        SystemTask::Severity(Severity::Error),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Severity(Severity::Warning),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Severity(Severity::Info),
        ProceduralAndInitial,
        0,
        None,
    ),
    // 20.11 Assertion control system tasks
    unsupported("$asserton", Task),
    unsupported("$assertoff", Task),
    unsupported("$assertkill", Task),
    unsupported("$assertpasson", Task),
    unsupported("$assertpassoff", Task),
    unsupported("$assertfailon", Task),
    unsupported("$assertfailoff", Task),
    unsupported("$assertnonvacuouson", Task),
    unsupported("$assertvacuousoff", Task),
    unsupported("$assertcontrol", Task),
    // 20.12 Sampled value system functions
    unsupported("$sampled", Function),
    unsupported("$rose", Function),
    unsupported("$fell", Function),
    unsupported("$stable", Function),
    unsupported("$changed", Function),
    unsupported("$past", Function),
    unsupported("$past_gclk", Function),
    unsupported("$rose_gclk", Function),
    unsupported("$fell_gclk", Function),
    unsupported("$stable_gclk", Function),
    unsupported("$changed_gclk", Function),
    unsupported("$future_gclk", Function),
    unsupported("$rising_gclk", Function),
    unsupported("$falling_gclk", Function),
    unsupported("$steady_gclk", Function),
    unsupported("$changing_gclk", Function),
    // 20.13 Coverage system functions
    unsupported("$coverage_control", Function),
    unsupported("$coverage_get_max", Function),
    unsupported("$coverage_get", Function),
    unsupported("$coverage_merge", Function),
    unsupported("$coverage_save", Function),
    unsupported("$get_coverage", Function),
    unsupported("$set_coverage_db_name", Task),
    unsupported("$load_coverage_db", Task),
    // 20.14 Probabilistic distribution functions
    function(SystemFunction::Random, 0, Some(1)),
    unsupported("$dist_uniform", Function),
    unsupported("$dist_normal", Function),
    unsupported("$dist_exponential", Function),
    unsupported("$dist_poisson", Function),
    unsupported("$dist_chi_square", Function),
    unsupported("$dist_t", Function),
    unsupported("$dist_erlang", Function),
    // 20.15 Stochastic analysis tasks and functions
    unsupported("$q_initialize", Task),
    unsupported("$q_add", Task),
    unsupported("$q_remove", Task),
    unsupported("$q_full", Function),
    unsupported("$q_exam", Task),
    // 20.16 Programmable logic array modeling system tasks
    unsupported("$async$and$array", Task),
    unsupported("$async$nand$array", Task),
    unsupported("$async$or$array", Task),
    unsupported("$async$nor$array", Task),
    unsupported("$async$and$plane", Task),
    unsupported("$async$nand$plane", Task),
    unsupported("$async$or$plane", Task),
    unsupported("$async$nor$plane", Task),
    unsupported("$sync$and$array", Task),
    unsupported("$sync$nand$array", Task),
    unsupported("$sync$or$array", Task),
    unsupported("$sync$nor$array", Task),
    unsupported("$sync$and$plane", Task),
    unsupported("$sync$nand$plane", Task),
    unsupported("$sync$or$plane", Task),
    unsupported("$sync$nor$plane", Task),
    // 20.17 Miscellaneous tasks and functions
    unsupported("$system", TaskOrFunction),
    unsupported("$stacktrace", TaskOrFunction),
    // 21.2 Display system tasks
    task(
        SystemTask::Display(Radix::Decimal),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Display(Radix::Binary),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Display(Radix::Octal),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Display(Radix::Hexadecimal),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Write(Radix::Decimal),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Write(Radix::Binary),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Write(Radix::Octal),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Write(Radix::Hexadecimal),
        ProceduralAndInitial,
        0,
        None,
    ),
    unsupported("$strobe", Task),
    unsupported("$strobeb", Task),
    unsupported("$strobeo", Task),
    unsupported("$strobeh", Task),
    unsupported("$monitor", Task),
    unsupported("$monitorb", Task),
    unsupported("$monitoro", Task),
    unsupported("$monitorh", Task),
    unsupported("$monitoron", Task),
    unsupported("$monitoroff", Task),
    // 21.3 File input/output system tasks and system functions
    unsupported("$fopen", Function),
    unsupported("$fclose", Task),
    unsupported("$fdisplay", Task),
    unsupported("$fdisplayb", Task),
    unsupported("$fdisplayo", Task),
    unsupported("$fdisplayh", Task),
    unsupported("$fwrite", Task),
    unsupported("$fwriteb", Task),
    unsupported("$fwriteo", Task),
    unsupported("$fwriteh", Task),
    unsupported("$fstrobe", Task),
    unsupported("$fstrobeb", Task),
    unsupported("$fstrobeo", Task),
    unsupported("$fstrobeh", Task),
    unsupported("$fmonitor", Task),
    unsupported("$fmonitorb", Task),
    unsupported("$fmonitoro", Task),
    unsupported("$fmonitorh", Task),
    unsupported("$swrite", Task),
    unsupported("$swriteb", Task),
    unsupported("$swriteo", Task),
    unsupported("$swriteh", Task),
    unsupported("$sformat", Task),
    unsupported("$sformatf", Function),
    unsupported("$fgetc", Function),
    unsupported("$ungetc", Function),
    unsupported("$fgets", Function),
    unsupported("$fscanf", Function),
    unsupported("$sscanf", Function),
    unsupported("$fread", Function),
    unsupported("$ftell", Function),
    unsupported("$fseek", Function),
    unsupported("$rewind", Function),
    unsupported("$fflush", Task),
    unsupported("$ferror", Function),
    unsupported("$feof", Function),
    // 21.4 Loading memory array data from a file
    task(
        SystemTask::Readmem(MemoryFormat::Binary),
        ProceduralAndInitial,
        2,
        Some(4),
    ),
    task(
        SystemTask::Readmem(MemoryFormat::Hexadecimal),
        ProceduralAndInitial,
        2,
        Some(4),
    ),
    // 21.5 Writing memory array data to a file
    unsupported("$writememb", Task),
    unsupported("$writememh", Task),
    // 21.6 Command line input
    unsupported("$test$plusargs", Function),
    unsupported("$value$plusargs", Function),
    // 21.7 Value change dump (VCD) files
    unsupported("$dumpfile", Task),
    unsupported("$dumpvars", Task),
    unsupported("$dumpoff", Task),
    unsupported("$dumpon", Task),
    unsupported("$dumpall", Task),
    unsupported("$dumplimit", Task),
    unsupported("$dumpflush", Task),
    unsupported("$dumpports", Task),
    unsupported("$dumpportsoff", Task),
    unsupported("$dumpportson", Task),
    unsupported("$dumpportsall", Task),
    unsupported("$dumpportslimit", Task),
    unsupported("$dumpportsflush", Task),
    // 14.14 Global clocking
    unsupported("$global_clock", Function),
    // 16.14.7 Inferred clocking and disable functions
    unsupported("$inferred_clock", Function),
    unsupported("$inferred_disable", Function),
    // 18.13 Random number system functions and methods
    function(SystemFunction::Urandom, 0, Some(1)),
    function(SystemFunction::UrandomRange, 1, Some(2)),
    // 32.9 Loading timing data from an SDF file
    unsupported("$sdf_annotate", Task),
    // Annex D Optional system tasks and system functions, which the standard
    // describes for information only
    unsupported("$countdrivers", Function),
    unsupported("$getpattern", Function),
    unsupported("$input", Task),
    unsupported("$key", Task),
    unsupported("$nokey", Task),
    unsupported("$list", Task),
    unsupported("$log", Task),
    unsupported("$nolog", Task),
    unsupported("$reset", Task),
    unsupported("$reset_count", Function),
    unsupported("$reset_value", Function),
    unsupported("$save", Task),
    unsupported("$restart", Task),
    unsupported("$incsave", Task),
    unsupported("$scale", Function),
    unsupported("$scope", Task),
    unsupported("$showscopes", Task),
    unsupported("$showvars", Task),
    unsupported("$sreadmemb", Task),
    unsupported("$sreadmemh", Task),
    // Veryl's immediate assertions, emitted into SystemVerilog as calls of
    // these names: `$assert(condition, message...)` ends the simulation when
    // the condition fails and `$assert_continue` reports and continues.
    task(SystemTask::Assert, Procedural, 1, None),
    task(SystemTask::AssertContinue, Procedural, 1, None),
];

/// The catalog entry for `name`, which includes the leading `$`.
pub fn lookup(name: &str) -> Option<&'static SystemTf> {
    SYSTEM_TFS.iter().find(|tf| tf.name == name)
}

/// The kind of procedural body a call statement is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// `always`, `always_comb`, or `always_ff`.
    Always,
    Initial,
    /// A function or task.
    Subroutine,
}

/// Where a call is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSite {
    Statement(Body),
    /// An operand of an expression, including a constant expression and the
    /// operand of `void'(...)`.
    Expression,
}

/// One argument of a call as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// An expression or a data type.
    Given,
    /// An omitted positional argument, as in `$display(a,,b)`.
    Omitted,
}

/// A supported system task or function, as [`check_call`] identifies a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemCall {
    Function(SystemFunction),
    Task(SystemTask),
}

impl SystemCall {
    /// The function, for a call in expression position.
    pub fn function(self) -> Option<SystemFunction> {
        match self {
            Self::Function(function) => Some(function),
            Self::Task(_) => None,
        }
    }
}

/// Check a call of `name` with `args` at `site` and identify it.
///
/// `args` is `None` for a call that binds its arguments by name.
pub fn check_call(
    name: &str,
    args: Option<&[Arg]>,
    site: CallSite,
) -> Result<SystemCall, AnalyzerError> {
    let tf = lookup(name).ok_or_else(|| AnalyzerError::UnknownSystemTf {
        name: name.to_string(),
    })?;
    let noun = tf.noun(matches!(site, CallSite::Statement(_)));
    let checked = match (site, tf.support) {
        (CallSite::Expression, _) if tf.kind == Task => {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name: name.to_string(),
                detail: "a system task has no value".to_string(),
            });
        }
        (CallSite::Statement(_), Support::Unsupported) => {
            return Err(AnalyzerError::Unsupported(format!("{noun} `{name}`")));
        }
        (CallSite::Expression, Support::Unsupported | Support::Task(..)) => {
            return Err(AnalyzerError::Unsupported(format!(
                "{noun} `{name}` in an expression"
            )));
        }
        (CallSite::Statement(Body::Initial), Support::Task(_, Procedural)) => {
            return Err(AnalyzerError::Unsupported(format!(
                "{noun} `{name}` inside an initial block"
            )));
        }
        (CallSite::Statement(_), Support::Task(task, _)) => SystemCall::Task(task),
        (_, Support::Function(function)) => SystemCall::Function(function),
    };
    let Some(args) = args else {
        return Err(AnalyzerError::Unsupported(format!(
            "named arguments of `{name}`"
        )));
    };
    let count = args.len();
    if count < tf.min_args || tf.max_args.is_some_and(|max| count > max) {
        return Err(AnalyzerError::InvalidSystemTfCall {
            name: name.to_string(),
            detail: format!("expected {}, found {count}", expected_arguments(tf)),
        });
    }
    // A function takes a value for each argument; only tasks such as
    // `$display(a,,b)` may leave one out.
    if tf.kind != Task
        && let Some(position) = args.iter().position(|arg| *arg == Arg::Omitted)
    {
        return Err(AnalyzerError::InvalidSystemTfCall {
            name: name.to_string(),
            detail: format!("argument {} is omitted", position + 1),
        });
    }
    Ok(checked)
}

/// Check a call of `name` with `args` in expression position and identify
/// the function.
pub fn check_function_call(
    name: &str,
    args: Option<&[Arg]>,
) -> Result<SystemFunction, AnalyzerError> {
    match check_call(name, args, CallSite::Expression)? {
        SystemCall::Function(function) => Ok(function),
        SystemCall::Task(_) => unreachable!("a task is rejected in expression position"),
    }
}

fn expected_arguments(tf: &SystemTf) -> String {
    let plural = |count: usize| if count == 1 { "argument" } else { "arguments" };
    match tf.max_args {
        Some(max) if max == tf.min_args => format!("{max} {}", plural(max)),
        Some(max) => format!("{} to {max} arguments", tf.min_args),
        None => format!("at least {} {}", tf.min_args, plural(tf.min_args)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_well_formed() {
        let mut names = fxhash::FxHashSet::default();
        for tf in SYSTEM_TFS {
            assert!(names.insert(tf.name), "duplicate entry {}", tf.name);
            assert!(tf.name.starts_with('$') && tf.name.len() > 1, "{}", tf.name);
            if let Some(max) = tf.max_args {
                assert!(tf.min_args <= max, "{}", tf.name);
            }
        }
    }

    #[test]
    fn supported_entries_round_trip_through_their_names() {
        for tf in SYSTEM_TFS {
            match tf.support {
                Support::Function(function) => {
                    assert_eq!(tf.kind, Function, "{}", tf.name);
                    assert_eq!(SystemFunction::from_name(tf.name), Some(function));
                }
                Support::Task(task, _) => {
                    assert_eq!(tf.kind, Task, "{}", tf.name);
                    assert_eq!(SystemTask::from_name(tf.name), Some(task));
                }
                Support::Unsupported => {}
            }
        }
    }
}
