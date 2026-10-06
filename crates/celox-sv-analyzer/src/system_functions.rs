//! The system tasks and functions Celox knows (IEEE 1800-2023 clauses 20 and
//! 21), and where the SystemVerilog frontend supports each one.
//!
//! The analyzer checks each call against this catalog where it converts the
//! call, with the context it converts it in ([`CallSite`]), so a name the
//! frontend cannot lower is reported by name instead of being dropped or
//! reported as a generic expression error. Names that are not in the catalog
//! are rejected as unknown.

use crate::AnalyzerError;

/// Whether IEEE 1800-2023 defines the name as a task, a function, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemTfKind {
    Task,
    Function,
    /// Both forms exist, such as `$cast` (IEEE 1800-2023 6.24.2).
    TaskOrFunction,
}

/// Where a call may appear as a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementUse {
    Unsupported,
    /// In `always` processes and subroutines, but not in `initial` blocks.
    Procedural,
    /// Also in `initial` blocks.
    ProceduralAndInitial,
}

/// One system task or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemTf {
    pub name: &'static str,
    pub kind: SystemTfKind,
    /// Support as a statement. A function called as a statement is evaluated
    /// and its value discarded.
    pub statement: StatementUse,
    /// Support as an expression operand, including constant expressions.
    pub expression: bool,
    /// The number of arguments Celox accepts, when the call is supported
    /// somewhere: at least `min_args`, and at most `max_args` if bounded.
    pub min_args: usize,
    pub max_args: Option<usize>,
}

impl SystemTf {
    /// Whether the call is supported in any position.
    pub fn is_supported(&self) -> bool {
        self.statement != StatementUse::Unsupported || self.expression
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
        statement: StatementUse::Unsupported,
        expression: false,
        min_args: 0,
        max_args: None,
    }
}

/// A task supported as a statement.
const fn task(
    name: &'static str,
    statement: StatementUse,
    min_args: usize,
    max_args: Option<usize>,
) -> SystemTf {
    SystemTf {
        name,
        kind: Task,
        statement,
        expression: false,
        min_args,
        max_args,
    }
}

/// A function supported in expressions, and as a statement whose value is
/// discarded.
const fn function(name: &'static str, min_args: usize, max_args: usize) -> SystemTf {
    SystemTf {
        name,
        kind: Function,
        statement: ProceduralAndInitial,
        expression: true,
        min_args,
        max_args: Some(max_args),
    }
}

/// The catalog, in the order of IEEE 1800-2023 clauses 20 and 21.
pub const SYSTEM_TFS: &[SystemTf] = &[
    // 20.2 Simulation control system tasks
    task("$finish", Procedural, 0, Some(1)),
    task("$stop", Procedural, 0, Some(1)),
    unsupported("$exit", Task),
    // 20.3 Simulation time system functions
    unsupported("$time", Function),
    unsupported("$stime", Function),
    unsupported("$realtime", Function),
    // 20.4 Timescale system tasks and system functions
    unsupported("$printtimescale", Task),
    unsupported("$timeformat", Task),
    unsupported("$timeunit", Function),
    unsupported("$timeprecision", Function),
    // 20.5 Conversion functions
    unsupported("$rtoi", Function),
    unsupported("$itor", Function),
    unsupported("$realtobits", Function),
    unsupported("$bitstoreal", Function),
    unsupported("$shortrealtobits", Function),
    unsupported("$bitstoshortreal", Function),
    // 11.7 Signed expressions
    function("$signed", 1, 1),
    function("$unsigned", 1, 1),
    // 6.24.2 Dynamic casting
    unsupported("$cast", TaskOrFunction),
    // 20.6 Data query functions
    unsupported("$typename", Function),
    function("$bits", 1, 1),
    unsupported("$isunbounded", Function),
    // 20.7 Array query functions
    unsupported("$dimensions", Function),
    unsupported("$unpacked_dimensions", Function),
    unsupported("$left", Function),
    unsupported("$right", Function),
    unsupported("$low", Function),
    unsupported("$high", Function),
    unsupported("$increment", Function),
    function("$size", 1, 2),
    // 20.8 Math functions
    function("$clog2", 1, 1),
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
    unsupported("$countbits", Function),
    function("$countones", 1, 1),
    function("$onehot", 1, 1),
    function("$onehot0", 1, 1),
    function("$isunknown", 1, 1),
    // 20.10 Severity tasks (and 20.11 elaboration system tasks)
    task("$fatal", Procedural, 0, None),
    task("$error", Procedural, 0, None),
    task("$warning", Procedural, 0, None),
    task("$info", Procedural, 0, None),
    // 20.12 Assertion control system tasks
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
    // 20.13 Sampled value system functions
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
    // 20.14 Coverage control functions
    unsupported("$coverage_control", Function),
    unsupported("$coverage_get_max", Function),
    unsupported("$coverage_get", Function),
    unsupported("$coverage_merge", Function),
    unsupported("$coverage_save", Function),
    unsupported("$get_coverage", Function),
    unsupported("$set_coverage_db_name", Task),
    unsupported("$load_coverage_db", Task),
    // 20.15 Probabilistic distribution functions
    unsupported("$random", Function),
    unsupported("$urandom", Function),
    unsupported("$urandom_range", Function),
    unsupported("$dist_uniform", Function),
    unsupported("$dist_normal", Function),
    unsupported("$dist_exponential", Function),
    unsupported("$dist_poisson", Function),
    unsupported("$dist_chi_square", Function),
    unsupported("$dist_t", Function),
    unsupported("$dist_erlang", Function),
    // 20.16 Stochastic analysis tasks and functions
    unsupported("$q_initialize", Task),
    unsupported("$q_add", Task),
    unsupported("$q_remove", Task),
    unsupported("$q_full", Function),
    unsupported("$q_exam", Task),
    // 20.17 Programmable logic array modeling system tasks
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
    // 20.18 Miscellaneous tasks and functions
    unsupported("$system", TaskOrFunction),
    // 21.2 Display system tasks
    task("$display", Procedural, 0, None),
    task("$displayb", Procedural, 0, None),
    task("$displayo", Procedural, 0, None),
    task("$displayh", Procedural, 0, None),
    task("$write", Procedural, 0, None),
    task("$writeb", Procedural, 0, None),
    task("$writeo", Procedural, 0, None),
    task("$writeh", Procedural, 0, None),
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
    task("$readmemb", ProceduralAndInitial, 2, Some(4)),
    task("$readmemh", ProceduralAndInitial, 2, Some(4)),
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
    // Veryl's immediate assertions, emitted into SystemVerilog as calls of
    // these names: `$assert(condition, message...)` ends the simulation when
    // the condition fails and `$assert_continue` reports and continues.
    task("$assert", Procedural, 1, None),
    task("$assert_continue", Procedural, 1, None),
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

/// Check a call of `name` with `args` at `site` and return its entry.
///
/// `args` is `None` for a call that binds its arguments by name.
pub fn check_call(
    name: &str,
    args: Option<&[Arg]>,
    site: CallSite,
) -> Result<&'static SystemTf, AnalyzerError> {
    let tf = lookup(name).ok_or_else(|| AnalyzerError::UnknownSystemTf {
        name: name.to_string(),
    })?;
    let statement = matches!(site, CallSite::Statement(_));
    let noun = tf.noun(statement);
    match site {
        CallSite::Statement(body) => match tf.statement {
            StatementUse::Unsupported => {
                return Err(AnalyzerError::Unsupported(format!("{noun} `{name}`")));
            }
            StatementUse::Procedural if body == Body::Initial => {
                return Err(AnalyzerError::Unsupported(format!(
                    "{noun} `{name}` inside an initial block"
                )));
            }
            StatementUse::Procedural | StatementUse::ProceduralAndInitial => {}
        },
        CallSite::Expression if tf.kind == Task => {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name: name.to_string(),
                detail: "a system task has no value".to_string(),
            });
        }
        CallSite::Expression if !tf.expression => {
            return Err(AnalyzerError::Unsupported(format!(
                "{noun} `{name}` in an expression"
            )));
        }
        CallSite::Expression => {}
    }
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
    Ok(tf)
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
    fn functions_are_supported_in_expressions_and_statements_together() {
        // A function called as a statement is evaluated as an expression, so
        // it is supported as a statement exactly when it is in expressions.
        for tf in SYSTEM_TFS.iter().filter(|tf| tf.kind == Function) {
            assert_eq!(
                tf.expression,
                tf.statement != StatementUse::Unsupported,
                "{}",
                tf.name
            );
        }
        // Tasks have no value.
        for tf in SYSTEM_TFS.iter().filter(|tf| tf.kind == Task) {
            assert!(!tf.expression, "{}", tf.name);
        }
    }
}
