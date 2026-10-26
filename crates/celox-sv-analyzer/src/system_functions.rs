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
    Ln,
    Log10,
    Exp,
    Sqrt,
    Floor,
    Ceil,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Pow,
    Atan2,
    Hypot,

    Rtoi,
    Itor,
    Realtobits,
    Bitstoreal,
    Shortrealtobits,
    Bitstoshortreal,
    /// `$time` (20.3.1).
    Time,
    /// `$stime` (20.3.2).
    Stime,
    /// Real simulation time in the calling scope (20.3.3).
    Realtime,
    /// `$timeunit` (20.4.2).
    Timeunit,
    /// `$timeprecision` (20.4.2).
    Timeprecision,
    /// `$signed` (11.7).
    Signed,
    /// `$unsigned` (11.7).
    Unsigned,
    /// `$sampled` (IEEE 1800-2023 16.9.3): the value of its operand in the
    /// Preponed region of the time slot.
    Sampled,
    /// `$past` (16.9.3): the sampled value of its operand at an earlier tick
    /// of the clock.
    Past,
    /// `$rose`, `$fell`, `$stable` and `$changed` (16.9.3): the sampled value
    /// of the operand against its sampled value at the previous tick.
    Rose,
    Fell,
    Stable,
    Changed,
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
    /// `$dist_uniform` (20.14.2).
    DistUniform,
    DistNormal,
    DistExponential,
    DistPoisson,
    DistChiSquare,
    DistT,
    DistErlang,

    /// Command-line prefix query (21.6).
    TestPlusargs,
    /// Command-line value query (21.6).
    ValuePlusargs,
    /// `$urandom` (18.13.1).
    Urandom,
    /// Scope randomization of packed variables, without constraints (18.12).
    ScopeRandomize,
    /// Internal scalar enum randomization: destination followed by legal values.
    ScopeRandomizeEnum,
    /// `$urandom_range` (18.13.2).
    UrandomRange,
    /// `$fopen` (21.3.1).
    Fopen,
    /// Binary input into packed integers or a static memory (21.3.4.4).
    Fread,
    /// File position and byte input functions (21.3.4–8).
    Ftell,
    Fseek,
    Rewind,
    Fgetc,
    Ungetc,
    Feof,
    /// Integral formatted input (21.3.4.3).
    Sscanf,
    Fscanf,
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
    /// IEEE 1800-2023 20.8.2: every real math call has real formals/result.
    pub fn real_math_unary(self) -> Option<celox_design::FloatUnaryOp> {
        Some(match self {
            Self::Ln => celox_design::FloatUnaryOp::Ln,
            Self::Log10 => celox_design::FloatUnaryOp::Log10,
            Self::Exp => celox_design::FloatUnaryOp::Exp,
            Self::Sqrt => celox_design::FloatUnaryOp::Sqrt,
            Self::Floor => celox_design::FloatUnaryOp::Floor,
            Self::Ceil => celox_design::FloatUnaryOp::Ceil,
            Self::Sin => celox_design::FloatUnaryOp::Sin,
            Self::Cos => celox_design::FloatUnaryOp::Cos,
            Self::Tan => celox_design::FloatUnaryOp::Tan,
            Self::Asin => celox_design::FloatUnaryOp::Asin,
            Self::Acos => celox_design::FloatUnaryOp::Acos,
            Self::Atan => celox_design::FloatUnaryOp::Atan,
            Self::Sinh => celox_design::FloatUnaryOp::Sinh,
            Self::Cosh => celox_design::FloatUnaryOp::Cosh,
            Self::Tanh => celox_design::FloatUnaryOp::Tanh,
            Self::Asinh => celox_design::FloatUnaryOp::Asinh,
            Self::Acosh => celox_design::FloatUnaryOp::Acosh,
            Self::Atanh => celox_design::FloatUnaryOp::Atanh,
            _ => return None,
        })
    }
    pub fn real_math_binary(self) -> Option<celox_design::FloatBinaryOp> {
        Some(match self {
            Self::Pow => celox_design::FloatBinaryOp::Pow,
            Self::Atan2 => celox_design::FloatBinaryOp::Atan2,
            Self::Hypot => celox_design::FloatBinaryOp::Hypot,
            _ => return None,
        })
    }
    pub fn is_real_math(self) -> bool {
        self.real_math_unary().is_some() || self.real_math_binary().is_some()
    }
    pub fn eval_real_math(self, args: &[f64]) -> Option<f64> {
        use celox_design::{FloatPrecision, eval_float_binary, eval_float_unary};
        let bits = if let Some(op) = self.real_math_unary() {
            let [arg] = args else {
                return None;
            };
            eval_float_unary(op, FloatPrecision::F64, arg.to_bits())
        } else {
            let [left, right] = args else {
                return None;
            };
            eval_float_binary(
                self.real_math_binary()?,
                FloatPrecision::F64,
                left.to_bits(),
                right.to_bits(),
            )
        };
        Some(f64::from_bits(bits))
    }

    /// Calls that return through the timed scheduler's host boundary.
    pub fn is_host_request(self) -> bool {
        matches!(
            self,
            Self::Fopen
                | Self::Fread
                | Self::Ftell
                | Self::Fseek
                | Self::Rewind
                | Self::Fgetc
                | Self::Ungetc
                | Self::Feof
                | Self::Sscanf
                | Self::Fscanf
                | Self::TestPlusargs
                | Self::ValuePlusargs
                | Self::DistNormal
                | Self::DistExponential
                | Self::DistPoisson
                | Self::DistChiSquare
                | Self::DistT
                | Self::DistErlang
        )
    }

    /// The built-in name (`$` functions or `std::randomize`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ln => "$ln",
            Self::Log10 => "$log10",
            Self::Exp => "$exp",
            Self::Sqrt => "$sqrt",
            Self::Floor => "$floor",
            Self::Ceil => "$ceil",
            Self::Sin => "$sin",
            Self::Cos => "$cos",
            Self::Tan => "$tan",
            Self::Asin => "$asin",
            Self::Acos => "$acos",
            Self::Atan => "$atan",
            Self::Sinh => "$sinh",
            Self::Cosh => "$cosh",
            Self::Tanh => "$tanh",
            Self::Asinh => "$asinh",
            Self::Acosh => "$acosh",
            Self::Atanh => "$atanh",
            Self::Pow => "$pow",
            Self::Atan2 => "$atan2",
            Self::Hypot => "$hypot",

            Self::Rtoi => "$rtoi",
            Self::Itor => "$itor",
            Self::Realtobits => "$realtobits",
            Self::Bitstoreal => "$bitstoreal",
            Self::Shortrealtobits => "$shortrealtobits",
            Self::Bitstoshortreal => "$bitstoshortreal",
            Self::Time => "$time",
            Self::Stime => "$stime",
            Self::Realtime => "$realtime",
            Self::Timeunit => "$timeunit",
            Self::Timeprecision => "$timeprecision",
            Self::Signed => "$signed",
            Self::Unsigned => "$unsigned",
            Self::Sampled => "$sampled",
            Self::Past => "$past",
            Self::Rose => "$rose",
            Self::Fell => "$fell",
            Self::Stable => "$stable",
            Self::Changed => "$changed",
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
            Self::DistUniform => "$dist_uniform",
            Self::DistNormal => "$dist_normal",
            Self::DistExponential => "$dist_exponential",
            Self::DistPoisson => "$dist_poisson",
            Self::DistChiSquare => "$dist_chi_square",
            Self::DistT => "$dist_t",
            Self::DistErlang => "$dist_erlang",

            Self::TestPlusargs => "$test$plusargs",
            Self::ValuePlusargs => "$value$plusargs",
            Self::Urandom => "$urandom",
            Self::ScopeRandomize => "std::randomize",
            Self::ScopeRandomizeEnum => "$__celox_randomize_enum",
            Self::UrandomRange => "$urandom_range",
            Self::Fopen => "$fopen",
            Self::Fread => "$fread",
            Self::Ftell => "$ftell",
            Self::Fseek => "$fseek",
            Self::Rewind => "$rewind",
            Self::Fgetc => "$fgetc",
            Self::Ungetc => "$ungetc",
            Self::Feof => "$feof",
            Self::Sscanf => "$sscanf",
            Self::Fscanf => "$fscanf",
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
        matches!(
            self,
            Self::Random
                | Self::DistUniform
                | Self::DistNormal
                | Self::DistExponential
                | Self::DistPoisson
                | Self::DistChiSquare
                | Self::DistT
                | Self::DistErlang
                | Self::TestPlusargs
                | Self::ValuePlusargs
                | Self::Urandom
                | Self::Fopen
                | Self::Fread
                | Self::Ftell
                | Self::Fseek
                | Self::Rewind
                | Self::Fgetc
                | Self::Ungetc
                | Self::Feof
                | Self::Sscanf
                | Self::Fscanf
                | Self::UrandomRange
                | Self::ScopeRandomize
                | Self::ScopeRandomizeEnum
        )
    }

    /// Whether a call reads the state of the simulation: it has an effect,
    /// or it returns the simulation time. Such a call is not a constant
    /// expression, and a process evaluates it where it runs.
    pub fn reads_simulation_state(self) -> bool {
        self.is_effectful()
            || matches!(
                self,
                Self::Time
                    | Self::Stime
                    | Self::Realtime
                    | Self::Sampled
                    | Self::Past
                    | Self::Rose
                    | Self::Fell
                    | Self::Stable
                    | Self::Changed
            )
    }

    /// Whether the call writes argument `position` back, as `$random(seed)`
    /// updates its seed variable.
    pub fn writes_argument(self, position: usize) -> bool {
        self == Self::ScopeRandomize
            || (self == Self::ScopeRandomizeEnum && position == 0)
            || (matches!(
                self,
                Self::Random
                    | Self::DistUniform
                    | Self::DistNormal
                    | Self::DistExponential
                    | Self::DistPoisson
                    | Self::DistChiSquare
                    | Self::DistT
                    | Self::DistErlang
            ) && position == 0)
            || (self == Self::ValuePlusargs && position == 1)
            || (self == Self::Fread && position == 0)
            || (matches!(self, Self::Sscanf | Self::Fscanf) && position >= 2)
    }

    /// The width and signedness of a call that is lowered as an integral
    /// operation. `None` for `$signed` and `$unsigned`, whose type follows
    /// their operand, and for the data queries, which the analyzer folds to
    /// literals where it converts them.
    pub fn result_type(self) -> Option<(usize, bool)> {
        match self {
            Self::Ln
            | Self::Log10
            | Self::Exp
            | Self::Sqrt
            | Self::Floor
            | Self::Ceil
            | Self::Sin
            | Self::Cos
            | Self::Tan
            | Self::Asin
            | Self::Acos
            | Self::Atan
            | Self::Sinh
            | Self::Cosh
            | Self::Tanh
            | Self::Asinh
            | Self::Acosh
            | Self::Atanh
            | Self::Pow
            | Self::Atan2
            | Self::Hypot => Some((64, false)),

            Self::Timeunit
            | Self::Timeprecision
            | Self::Clog2
            | Self::Countbits
            | Self::Countones
            | Self::ScopeRandomize
            | Self::ScopeRandomizeEnum => Some((32, true)),
            Self::Onehot
            | Self::Onehot0
            | Self::Isunknown
            | Self::Isunbounded
            | Self::Rose
            | Self::Fell
            | Self::Stable
            | Self::Changed => Some((1, false)),
            Self::Random
            | Self::DistUniform
            | Self::DistNormal
            | Self::DistExponential
            | Self::DistPoisson
            | Self::DistChiSquare
            | Self::DistT
            | Self::DistErlang
            | Self::TestPlusargs
            | Self::ValuePlusargs
            | Self::Fread
            | Self::Ftell
            | Self::Fseek
            | Self::Rewind
            | Self::Fgetc
            | Self::Ungetc
            | Self::Sscanf
            | Self::Fscanf
            | Self::Feof => Some((32, true)),
            Self::Urandom | Self::UrandomRange | Self::Stime | Self::Fopen => Some((32, false)),
            Self::Rtoi => Some((32, true)),
            Self::Itor | Self::Bitstoreal | Self::Realtobits | Self::Realtime => Some((64, false)),
            Self::Shortrealtobits | Self::Bitstoshortreal => Some((32, false)),
            Self::Time => Some((64, false)),
            Self::Signed
            | Self::Unsigned
            | Self::Sampled
            | Self::Past
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
    /// `$stop` (20.2).
    Stop,
    /// `$exit` (20.2, 24.7), which ends the initial procedures of the
    /// program block that calls it; a call from outside one is ignored.
    Exit,
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
    /// Value change dump controls (21.7.1).
    Vcd(VcdTask),
    /// File output tasks (21.3).
    Fclose,
    /// Flush selected output files or all open files (21.3.6).
    Fflush,
    Fdisplay(Radix),
    Fwrite(Radix),
    /// Memory snapshot output (21.5).
    Writemem(MemoryFormat),
    /// Veryl's immediate assertions, emitted into SystemVerilog as calls of
    /// `$assert(condition, message...)`, which ends the simulation when the
    /// condition fails, and `$assert_continue`, which reports and continues.
    Assert,
    AssertContinue,
}

/// Four-state VCD control system tasks (IEEE 1800-2023 21.7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VcdTask {
    File,
    Vars,
    Flush,
    On,
    Off,
    All,
    Limit,
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
    /// Tasks executed synchronously by the timed simulation host.
    pub fn is_host_request(self) -> bool {
        matches!(
            self,
            Self::Fclose
                | Self::Fflush
                | Self::Fdisplay(_)
                | Self::Fwrite(_)
                | Self::Writemem(_)
                | Self::Vcd(_)
        )
    }

    /// The name, with its leading `$`.
    pub const fn name(self) -> &'static str {
        use Radix::{Binary, Decimal, Hexadecimal, Octal};
        match self {
            Self::Finish => "$finish",
            Self::Stop => "$stop",
            Self::Exit => "$exit",
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
            Self::Fclose => "$fclose",
            Self::Fflush => "$fflush",
            Self::Fdisplay(Decimal) => "$fdisplay",
            Self::Fdisplay(Binary) => "$fdisplayb",
            Self::Fdisplay(Octal) => "$fdisplayo",
            Self::Fdisplay(Hexadecimal) => "$fdisplayh",
            Self::Fwrite(Decimal) => "$fwrite",
            Self::Fwrite(Binary) => "$fwriteb",
            Self::Fwrite(Octal) => "$fwriteo",
            Self::Fwrite(Hexadecimal) => "$fwriteh",
            Self::Writemem(MemoryFormat::Binary) => "$writememb",
            Self::Writemem(MemoryFormat::Hexadecimal) => "$writememh",
            Self::Readmem(MemoryFormat::Binary) => "$readmemb",
            Self::Readmem(MemoryFormat::Hexadecimal) => "$readmemh",
            Self::Vcd(VcdTask::File) => "$dumpfile",
            Self::Vcd(VcdTask::Vars) => "$dumpvars",
            Self::Vcd(VcdTask::Flush) => "$dumpflush",
            Self::Vcd(VcdTask::On) => "$dumpon",
            Self::Vcd(VcdTask::Off) => "$dumpoff",
            Self::Vcd(VcdTask::All) => "$dumpall",
            Self::Vcd(VcdTask::Limit) => "$dumplimit",
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

/// An operation on `string` values (IEEE 1800-2023 6.16, 11.4).
///
/// The analyzer rewrites string operators, methods and conversions into
/// calls of these, so the frontend dispatches on them instead of on the
/// integral operators that a string handle would otherwise reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringOp {
    /// A string holding the bytes of an integral operand, most significant
    /// first, without NUL bytes: a cast to `string` (6.24.1), or a string
    /// literal or integral value assigned to a string.
    FromBits,
    /// `{a, b, ...}` (11.4.12.2). Integral operands contribute their bytes.
    Concat,
    /// `{n{s}}` (11.4.12.2), with the arguments `s` and `n`.
    Replicate,
    /// `str.compare(s)` (6.16.6), and the ordering of relational operators.
    Compare,
    /// `str.icompare(s)` (6.16.7).
    ICompare,
    /// `str.len()` (6.16.1).
    Len,
    /// `str.getc(i)` and `str[i]` (6.16.3).
    Getc,
    /// `str.putc(i, c)` and `str[i] = c` (6.16.2), as the updated string.
    Putc,
    /// `str.substr(i, j)` (6.16.8).
    Substr,
    /// `str.toupper()` (6.16.4).
    ToUpper,
    /// `str.tolower()` (6.16.5).
    ToLower,
    /// `str.atoi()` and its radix variants (6.16.9).
    Atoi(Radix),
    /// `str.itoa(i)` and its radix variants (6.16.11-14), as the new string.
    Itoa(Radix),
}

impl StringOp {
    /// Whether the result is a string rather than an integral value.
    pub fn returns_string(self) -> bool {
        matches!(
            self,
            Self::FromBits
                | Self::Concat
                | Self::Replicate
                | Self::Putc
                | Self::Substr
                | Self::ToUpper
                | Self::ToLower
                | Self::Itoa(_)
        )
    }

    /// The width and signedness of the result; a string is a 32-bit handle.
    pub fn result_type(self) -> (usize, bool) {
        match self {
            Self::Compare | Self::ICompare | Self::Len | Self::Atoi(_) => (32, true),
            Self::Getc => (8, false),
            _ => (32, false),
        }
    }

    /// The method a call names on a string value, and its number of
    /// arguments.
    pub fn method(name: &str) -> Option<(Self, usize)> {
        Some(match name {
            "len" => (Self::Len, 0),
            "getc" => (Self::Getc, 1),
            "putc" => (Self::Putc, 2),
            "toupper" => (Self::ToUpper, 0),
            "tolower" => (Self::ToLower, 0),
            "compare" => (Self::Compare, 1),
            "icompare" => (Self::ICompare, 1),
            "substr" => (Self::Substr, 2),
            "atoi" => (Self::Atoi(Radix::Decimal), 0),
            "atohex" => (Self::Atoi(Radix::Hexadecimal), 0),
            "atooct" => (Self::Atoi(Radix::Octal), 0),
            "atobin" => (Self::Atoi(Radix::Binary), 0),
            "itoa" => (Self::Itoa(Radix::Decimal), 1),
            "hextoa" => (Self::Itoa(Radix::Hexadecimal), 1),
            "octtoa" => (Self::Itoa(Radix::Octal), 1),
            "bintoa" => (Self::Itoa(Radix::Binary), 1),
            _ => return None,
        })
    }

    /// Whether the method updates the string it is called on instead of
    /// returning a value (`putc` and the `itoa` family).
    pub fn updates_receiver(self) -> bool {
        matches!(self, Self::Putc | Self::Itoa(_))
    }
}

impl fmt::Display for StringOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::FromBits => "string cast",
            Self::Concat => "string concatenation",
            Self::Replicate => "string replication",
            Self::Compare => "compare",
            Self::ICompare => "icompare",
            Self::Len => "len",
            Self::Getc => "getc",
            Self::Putc => "putc",
            Self::Substr => "substr",
            Self::ToUpper => "toupper",
            Self::ToLower => "tolower",
            Self::Atoi(Radix::Decimal) => "atoi",
            Self::Atoi(Radix::Hexadecimal) => "atohex",
            Self::Atoi(Radix::Octal) => "atooct",
            Self::Atoi(Radix::Binary) => "atobin",
            Self::Itoa(Radix::Decimal) => "itoa",
            Self::Itoa(Radix::Hexadecimal) => "hextoa",
            Self::Itoa(Radix::Octal) => "octtoa",
            Self::Itoa(Radix::Binary) => "bintoa",
        };
        f.write_str(name)
    }
}

/// Where a property lives in an object and how it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectField {
    /// Byte offset of the value.
    pub offset: u32,
    pub width: u32,
    pub signed: bool,
    /// What a handle property refers to.
    pub handle: Option<HandleProperty>,
}

/// What a handle property refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleProperty {
    String,
    Object,
}

impl ObjectField {
    /// Bytes of the value.
    pub fn bytes(self) -> u32 {
        self.width.div_ceil(8)
    }
}

/// An operation on class objects (IEEE 1800-2023 clause 8). The analyzer
/// lays out each class and writes the layout into the operation, so the
/// frontend needs no class table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectOp {
    /// A new object of `class` with `size` zeroed bytes, whose handle
    /// properties are at the given offsets.
    New {
        class: u32,
        size: u32,
        handles: Vec<(u32, HandleProperty)>,
    },
    /// Read a property of the object in the argument.
    Load(ObjectField),
    /// Write the second argument into a property of the object in the first.
    Store(ObjectField),
    /// The run-time class of the object in the argument, 0 for `null`.
    ClassOf,
    /// Advance the object's independent RNG, returning 64 random bits.
    Random,
    /// Draw a domain index without replacement for the property at `offset`.
    RandomCyclic {
        offset: u32,
        count: u16,
    },
    /// Seed the object's RNG from the second argument.
    Seed,
    /// Validate a receiver; a nonzero class also rejects unsupported dynamic
    /// dispatch through a base handle before randomizing its properties.
    Check(u32),
    /// Query or set the random mode of a property at its byte offset.
    ModeGet(u32, bool),
    ModeSet(u32),
    ModePush,
    ModePop,
    ModeSelect(u32, bool),
    ModeSelected(u32),
    /// A new object of the class of the object in the argument, with a copy
    /// of its properties (IEEE 1800-2023 8.12).
    Copy,
}

impl ObjectOp {
    /// The width and signedness of the result.
    pub fn result_type(&self) -> (usize, bool) {
        match self {
            Self::Random | Self::RandomCyclic { .. } => (64, false),
            Self::Seed
            | Self::Check(_)
            | Self::ModeSet(_)
            | Self::ModePush
            | Self::ModePop
            | Self::ModeSelect(..)
            | Self::ModeSelected(_) => (32, false),
            Self::ModeGet(..) => (32, true),
            Self::Load(field) => (field.width as usize, field.signed),
            Self::New { .. } | Self::ClassOf | Self::Store(_) | Self::Copy => (32, false),
        }
    }
}

impl fmt::Display for ObjectOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::New { .. } => "new",
            Self::Load(_) => "property read",
            Self::Store(_) => "property write",
            Self::ClassOf => "class query",
            Self::Random => "object random bits",
            Self::RandomCyclic { .. } => "object random cyclic index",
            Self::Seed => "object random seed",
            Self::Check(_) => "object receiver check",
            Self::ModeGet(..) => "random mode query",
            Self::ModeSet(_) => "random mode update",
            Self::ModePush => "save random modes",
            Self::ModePop => "restore random modes",
            Self::ModeSelect(..) => "select random property",
            Self::ModeSelected(_) => "selected random property query",
            Self::Copy => "shallow copy",
        })
    }
}

/// The subroutine a call expression calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Callee {
    /// Check the first argument at runtime before evaluating/returning the second.
    /// Used for tagged union member access (IEEE 1800-2023 11.9).
    Checked {
        width: usize,
        signed: bool,
    },
    /// A user function or a DPI import, by its resolved name.
    Subroutine(String),
    System(SystemFunction),
    /// A string operation, see [`StringOp`].
    String(StringOp),
    /// A class object operation, see [`ObjectOp`].
    /// Boxed to keep call expressions small.
    Object(Box<ObjectOp>),
}

impl Callee {
    /// The name of the user subroutine, if this is not a system function.
    pub fn subroutine(&self) -> Option<&str> {
        match self {
            Self::Subroutine(name) => Some(name),
            Self::System(_) | Self::String(_) | Self::Object(_) | Self::Checked { .. } => None,
        }
    }

    /// The system function, if this is one.
    pub fn system(&self) -> Option<SystemFunction> {
        match self {
            Self::Subroutine(_) | Self::String(_) | Self::Object(_) | Self::Checked { .. } => None,
            Self::System(function) => Some(*function),
        }
    }

    /// The width and signedness of a call of a system function that is
    /// lowered as an integral operation; see [`SystemFunction::result_type`].
    pub fn result_type(&self) -> Option<(usize, bool)> {
        match self {
            Self::Checked { width, signed } => Some((*width, *signed)),
            Self::String(op) => Some(op.result_type()),
            Self::Object(op) => Some(op.result_type()),
            _ => self.system()?.result_type(),
        }
    }

    /// Rename a user subroutine with `rename`.
    pub fn map_subroutine(self, rename: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Checked { .. } => self,
            Self::Subroutine(name) => Self::Subroutine(rename(name)),
            Self::System(function) => Self::System(function),
            Self::String(op) => Self::String(op),
            Self::Object(op) => Self::Object(op),
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
            Self::Checked { .. } => f.write_str("tagged union access"),
            Self::Subroutine(name) => f.write_str(name),
            Self::System(function) => f.write_str(function.name()),
            Self::String(op) => op.fmt(f),
            Self::Object(op) => op.fmt(f),
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
    task(SystemTask::Stop, ProceduralAndInitial, 0, Some(1)),
    task(SystemTask::Exit, ProceduralAndInitial, 0, Some(0)),
    // 20.3 Simulation time system functions
    function(SystemFunction::Time, 0, Some(0)),
    function(SystemFunction::Stime, 0, Some(0)),
    function(SystemFunction::Realtime, 0, Some(0)),
    // 20.4 Timescale system tasks and system functions
    task(SystemTask::Printtimescale, ProceduralAndInitial, 0, Some(1)),
    unsupported("$timeformat", Task),
    function(SystemFunction::Timeunit, 0, Some(1)),
    function(SystemFunction::Timeprecision, 0, Some(1)),
    // 20.5 Conversion functions
    function(SystemFunction::Rtoi, 1, Some(1)),
    function(SystemFunction::Itor, 1, Some(1)),
    function(SystemFunction::Realtobits, 1, Some(1)),
    function(SystemFunction::Bitstoreal, 1, Some(1)),
    function(SystemFunction::Shortrealtobits, 1, Some(1)),
    function(SystemFunction::Bitstoshortreal, 1, Some(1)),
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
    function(SystemFunction::Ln, 1, Some(1)),
    function(SystemFunction::Log10, 1, Some(1)),
    function(SystemFunction::Exp, 1, Some(1)),
    function(SystemFunction::Sqrt, 1, Some(1)),
    function(SystemFunction::Pow, 2, Some(2)),
    function(SystemFunction::Floor, 1, Some(1)),
    function(SystemFunction::Ceil, 1, Some(1)),
    function(SystemFunction::Sin, 1, Some(1)),
    function(SystemFunction::Cos, 1, Some(1)),
    function(SystemFunction::Tan, 1, Some(1)),
    function(SystemFunction::Asin, 1, Some(1)),
    function(SystemFunction::Acos, 1, Some(1)),
    function(SystemFunction::Atan, 1, Some(1)),
    function(SystemFunction::Atan2, 2, Some(2)),
    function(SystemFunction::Hypot, 2, Some(2)),
    function(SystemFunction::Sinh, 1, Some(1)),
    function(SystemFunction::Cosh, 1, Some(1)),
    function(SystemFunction::Tanh, 1, Some(1)),
    function(SystemFunction::Asinh, 1, Some(1)),
    function(SystemFunction::Acosh, 1, Some(1)),
    function(SystemFunction::Atanh, 1, Some(1)),
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
    function(SystemFunction::Sampled, 1, Some(1)),
    function(SystemFunction::Rose, 1, Some(1)),
    function(SystemFunction::Fell, 1, Some(1)),
    function(SystemFunction::Stable, 1, Some(1)),
    function(SystemFunction::Changed, 1, Some(1)),
    function(SystemFunction::Past, 1, Some(3)),
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
    function(SystemFunction::DistUniform, 3, Some(3)),
    function(SystemFunction::DistNormal, 3, Some(3)),
    function(SystemFunction::DistExponential, 2, Some(2)),
    function(SystemFunction::DistPoisson, 2, Some(2)),
    function(SystemFunction::DistChiSquare, 2, Some(2)),
    function(SystemFunction::DistT, 2, Some(2)),
    function(SystemFunction::DistErlang, 3, Some(3)),
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
    function(SystemFunction::Fopen, 1, Some(2)),
    task(SystemTask::Fclose, ProceduralAndInitial, 1, Some(1)),
    task(
        SystemTask::Fdisplay(Radix::Decimal),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fdisplay(Radix::Binary),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fdisplay(Radix::Octal),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fdisplay(Radix::Hexadecimal),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fwrite(Radix::Decimal),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fwrite(Radix::Binary),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fwrite(Radix::Octal),
        ProceduralAndInitial,
        1,
        None,
    ),
    task(
        SystemTask::Fwrite(Radix::Hexadecimal),
        ProceduralAndInitial,
        1,
        None,
    ),
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
    function(SystemFunction::Fgetc, 1, Some(1)),
    function(SystemFunction::Ungetc, 2, Some(2)),
    unsupported("$fgets", Function),
    function(SystemFunction::Fscanf, 2, None),
    function(SystemFunction::Sscanf, 2, None),
    function(SystemFunction::Fread, 2, Some(4)),
    function(SystemFunction::Ftell, 1, Some(1)),
    function(SystemFunction::Fseek, 3, Some(3)),
    function(SystemFunction::Rewind, 1, Some(1)),
    task(SystemTask::Fflush, ProceduralAndInitial, 0, Some(1)),
    unsupported("$ferror", Function),
    function(SystemFunction::Feof, 1, Some(1)),
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
    task(
        SystemTask::Writemem(MemoryFormat::Binary),
        ProceduralAndInitial,
        2,
        Some(4),
    ),
    task(
        SystemTask::Writemem(MemoryFormat::Hexadecimal),
        ProceduralAndInitial,
        2,
        Some(4),
    ),
    // 21.6 Command line input
    function(SystemFunction::TestPlusargs, 1, Some(1)),
    function(SystemFunction::ValuePlusargs, 2, Some(2)),
    // 21.7 Value change dump (VCD) files
    task(
        SystemTask::Vcd(VcdTask::File),
        ProceduralAndInitial,
        0,
        Some(1),
    ),
    task(
        SystemTask::Vcd(VcdTask::Vars),
        ProceduralAndInitial,
        0,
        None,
    ),
    task(
        SystemTask::Vcd(VcdTask::Off),
        ProceduralAndInitial,
        0,
        Some(0),
    ),
    task(
        SystemTask::Vcd(VcdTask::On),
        ProceduralAndInitial,
        0,
        Some(0),
    ),
    task(
        SystemTask::Vcd(VcdTask::All),
        ProceduralAndInitial,
        0,
        Some(0),
    ),
    task(
        SystemTask::Vcd(VcdTask::Limit),
        ProceduralAndInitial,
        1,
        Some(1),
    ),
    task(
        SystemTask::Vcd(VcdTask::Flush),
        ProceduralAndInitial,
        0,
        Some(0),
    ),
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
    /// A final procedure, restricted to function statements (9.2.3).
    Final,
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
    // Functions require values, except the optional addressing operands of
    // `$fread` (21.3.4.4) and the number of ticks of `$past` (16.9.3). Tasks
    // such as `$display(a,,b)` may omit operands.
    if tf.kind != Task
        && let Some(position) = args.iter().enumerate().position(|(position, arg)| {
            *arg == Arg::Omitted
                && !(checked == SystemCall::Function(SystemFunction::Fread) && position >= 2)
                && !(checked == SystemCall::Function(SystemFunction::Past) && position == 1)
        })
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
    fn fread_allows_only_optional_address_operands_to_be_omitted() {
        use Arg::{Given, Omitted};
        for args in [
            vec![Given, Given, Omitted, Given],
            vec![Given, Given, Given, Omitted],
        ] {
            assert_eq!(
                check_function_call("$fread", Some(&args)).unwrap(),
                SystemFunction::Fread
            );
        }
        for args in [vec![Omitted, Given], vec![Given, Omitted]] {
            assert!(check_function_call("$fread", Some(&args)).is_err());
        }
        assert!(check_function_call("$fseek", Some(&[Given, Omitted, Given])).is_err());
    }

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
