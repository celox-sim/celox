//! Source-language-independent design identities and semantic vocabulary.

mod float;
pub use float::{eval_float_binary, eval_float_unary};

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub mod edges;
pub mod schedule;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DomainKind {
    ClockPosedge,
    ClockNegedge,
    ResetAsyncHigh,
    ResetAsyncLow,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriggerIdWithKind {
    pub kind: DomainKind,
    pub id: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortTypeKind {
    Clock,
    ResetAsyncHigh,
    ResetAsyncLow,
    ResetSyncHigh,
    ResetSyncLow,
    Logic,
    Bit,
    Other,
}

/// Source-independent metadata for one elaborated design variable.
///
/// Source IDs, source paths, and declaration syntax belong to the frontend and
/// deliberately are not part of this type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariableMetadata {
    pub width: usize,
    pub is_4state: bool,
    pub kind: DomainKind,
    pub type_kind: PortTypeKind,
    /// Per-dimension sizes for array ports (for example, `[4]` for `logic<32>[4]`).
    /// Empty means scalar.
    pub array_dims: Vec<usize>,
    /// What each element holds. Handle elements are 32-bit two-state slots.
    #[serde(default)]
    pub value_kind: ValueKind,
}

/// Bit width of a handle element in state memory.
pub const HANDLE_WIDTH: usize = 32;

/// What a state element holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValueKind {
    /// Packed bits.
    #[default]
    Bits,
    /// A handle of a string in the simulator's heap.
    StrRef,
    /// A handle of a class object in the simulator's heap, `0` for `null`.
    ObjRef,
}

impl ValueKind {
    pub fn is_handle(self) -> bool {
        self != Self::Bits
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerSet<A> {
    pub clock: A,
    pub resets: Vec<A>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeEventKind {
    Display,
    Write,
    /// A message of a severity task, or of a Veryl assertion
    /// (IEEE 1800-2023 20.10). [`Severity::Fatal`] ends the simulation with
    /// an error.
    Report(Severity),
    /// `$finish` executed by the design (IEEE 1800-2023 20.2).
    Finish,
    /// `$stop` executed by the design (IEEE 1800-2023 20.2).
    Stop,
}

impl RuntimeEventKind {
    /// A report that ends the simulation with an error.
    pub fn is_fatal(self) -> bool {
        self == Self::Report(Severity::Fatal)
    }
}

/// The severity of a report (IEEE 1800-2023 20.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// `$info`: a message of no specific severity.
    Info,
    /// `$warning`.
    Warning,
    /// `$error`, and the default action of a failed `assert` or `assume`.
    Error,
    /// `$fatal`.
    Fatal,
}

/// Where a report is written: the file and line of `__FILE__` and
/// `__LINE__` at the task call (IEEE 1800-2023 20.10).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReportLocation {
    pub file: String,
    pub line: usize,
}

/// The assertion whose action block issues a report.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssertionSite {
    /// The statement label, which names the assertion within its scope
    /// (IEEE 1800-2023 16.3).
    pub label: Option<String>,
    pub directive: AssertionDirective,
    pub deferral: AssertionDeferral,
    pub outcome: AssertionOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssertionDirective {
    Assert,
    Assume,
    Cover,
}

/// When an assertion reports (IEEE 1800-2023 16.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssertionDeferral {
    /// A simple immediate assertion: at once.
    None,
    /// `#0`: in the Reactive region.
    Observed,
    /// `final`: in the Postponed region.
    Final,
}

/// How the host defers the events of a site (IEEE 1800-2023 16.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeferredSite {
    /// A flush point of a process with deferred assertions (16.4.2): the
    /// actions its queue holds are discarded. The events of this site are
    /// not shown.
    FlushPoint,
    /// The action of a deferred assertion: its event waits in the queue of
    /// the flush-point site `queue` of the same instance, numbered like the
    /// sites of its module, until `region` of the time slot.
    Action {
        queue: u32,
        region: AssertionDeferral,
    },
}

/// Which action block of an assertion issues a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssertionOutcome {
    /// The pass action, or the statement of a `cover`.
    Pass,
    /// The fail action.
    Fail,
}

/// How a runtime event site sizes its formatted arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplaySizing {
    /// The formatting of Veryl's simulator: radices at the argument's width,
    /// decimals at their minimum, and C-style field widths that never
    /// truncate.
    #[default]
    Veryl,
    /// Automatic sizing and field widths of IEEE 1800-2023 21.2.1.2.
    Ieee,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeEventSite {
    pub kind: RuntimeEventKind,
    pub template: Option<String>,
    #[serde(default)]
    pub sizing: DisplaySizing,
    /// Fully elaborated module-instance scope that emitted this event.
    pub scope: Option<String>,
    #[serde(default)]
    pub location: Option<ReportLocation>,
    /// The assertion whose action issues this report.
    #[serde(default)]
    pub assertion: Option<AssertionSite>,
    /// How the host defers the events of this site, for the processes of
    /// deferred assertions.
    #[serde(default)]
    pub deferred: Option<DeferredSite>,
    pub arg_widths: Vec<usize>,
    pub arg_signed: Vec<bool>,
    pub arg_is_string: Vec<bool>,
    #[serde(default)]
    pub arg_float_precision: Vec<Option<FloatPrecision>>,
    /// Arguments that are heap handles of strings, formatted as their
    /// contents. Missing entries are not handles.
    #[serde(default)]
    pub arg_is_handle: Vec<bool>,
}

/// A function in the host's C ABI that the design calls, such as a
/// SystemVerilog DPI-C import. `ExternCall` instructions index the design's
/// table of these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternFunction {
    /// The C symbol the function is linked by.
    pub name: String,
    /// The prototype every call of `name` must share (IEEE 1800-2023
    /// 35.5.4).
    pub signature: ExternSignature,
}

/// The prototype of an [`ExternFunction`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternSignature {
    /// Whether the function is declared `pure`.
    pub pure: bool,
    /// The result, or `None` for a `void` function.
    pub result: Option<ExternType>,
    pub arguments: Vec<ExternType>,
}

/// A type an [`ExternFunction`] passes or returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternType {
    /// `bit`, passed as `svBit`.
    Bit,
    /// `logic`, passed as `svLogic`.
    Logic,
    /// `byte`, `shortint`, `int` or `longint`, passed as the C integer of
    /// that width.
    Integer { width: usize, signed: bool },
}

impl fmt::Display for ExternType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, signed) = match *self {
            ExternType::Bit => return f.write_str("bit"),
            ExternType::Logic => return f.write_str("logic"),
            ExternType::Integer { width: 8, signed } => ("byte", signed),
            ExternType::Integer { width: 16, signed } => ("shortint", signed),
            ExternType::Integer { width: 32, signed } => ("int", signed),
            ExternType::Integer { width: 64, signed } => ("longint", signed),
            ExternType::Integer { width, signed } => {
                return write!(f, "{}{width}", if signed { "i" } else { "u" });
            }
        };
        f.write_str(name)?;
        if !signed {
            f.write_str(" unsigned")?;
        }
        Ok(())
    }
}

impl fmt::Display for ExternFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let signature = &self.signature;
        if signature.pure {
            f.write_str("pure ")?;
        }
        f.write_str("function ")?;
        match signature.result {
            Some(result) => write!(f, "{result}")?,
            None => f.write_str("void")?,
        }
        write!(f, " {}(", self.name)?;
        for (index, argument) in signature.arguments.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "input {argument}")?;
        }
        f.write_str(")")
    }
}

/// Runtime activation recipe for one combinational event site.
///
/// Expression trees used to emit the event have already been lowered into
/// SIR.  The runtime only retains the persistent-state ranges needed to detect
/// whether the corresponding combinational process must be observed again.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCombObserver<A> {
    pub site_id: u32,
    pub activation_group: u32,
    pub sensitivity: Vec<VarAtomBase<A>>,
    /// Occurrence counters of the sensitivity: a counter that moved since
    /// the last check activates the observer even when the settled value
    /// equals the last one.
    #[serde(default = "Vec::new")]
    pub occurrence_counters: Vec<A>,
    pub written_inputs: Vec<A>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialStateWriteRun {
    pub bit_offset: usize,
    pub bit_width: usize,
    pub value_bytes: Vec<u8>,
    pub mask_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InitialStateData {
    Packed {
        value: BigUint,
        mask: BigUint,
        written_mask: BigUint,
    },
    Writes(Vec<InitialStateWriteRun>),
}

impl InitialStateData {
    /// The written bits below `width` of `Packed` data as write runs, so a
    /// target such as an unpacked array is written run by run instead of
    /// being read and written back whole. A target without a mask plane
    /// (`four_state` false) receives unknown bits as 0, as a two-state write
    /// of the packed value does.
    pub fn packed_write_runs(
        value: &BigUint,
        mask: &BigUint,
        written_mask: &BigUint,
        width: usize,
        four_state: bool,
    ) -> Vec<InitialStateWriteRun> {
        let (value, mask, written) = (
            value.to_u64_digits(),
            mask.to_u64_digits(),
            written_mask.to_u64_digits(),
        );
        let bit = |digits: &[u64], index: usize| {
            digits
                .get(index / 64)
                .is_some_and(|digit| digit >> (index % 64) & 1 == 1)
        };
        let mut runs = Vec::new();
        let mut index = 0;
        while index < width {
            if written
                .get(index / 64)
                .is_none_or(|&digit| digit >> (index % 64) == 0)
            {
                // No written bit in the rest of this digit.
                index = (index / 64 + 1) * 64;
                continue;
            }
            if !bit(&written, index) {
                index += 1;
                continue;
            }
            let start = index;
            while index < width && bit(&written, index) {
                index += 1;
            }
            let bit_width = index - start;
            let mut value_bytes = vec![0u8; bit_width.div_ceil(8)];
            let mut mask_bytes = vec![0u8; bit_width.div_ceil(8)];
            for offset in 0..bit_width {
                let unknown = bit(&mask, start + offset);
                if bit(&value, start + offset) && (four_state || !unknown) {
                    value_bytes[offset / 8] |= 1 << (offset % 8);
                }
                if four_state && unknown {
                    mask_bytes[offset / 8] |= 1 << (offset % 8);
                }
            }
            runs.push(InitialStateWriteRun {
                bit_offset: start,
                bit_width,
                value_bytes,
                mask_bytes,
            });
        }
        runs
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialStateValue<A> {
    pub address: A,
    pub data: InitialStateData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeErrorInfo<A> {
    pub message: String,
    pub signals: Vec<A>,
}

/// How a process kernel returned control to the runtime, as stored in its
/// [`ProcessSlots::status`] slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessStatus {
    /// Resume after the number of time units stored in [`ProcessSlots::delay`].
    /// A zero delay resumes at the same time after the nonblocking
    /// assignments of that time are published.
    Delay,
    /// The process ran to the end of its body.
    Done,
    /// The process requested the end of the simulation.
    Finish,
    /// Like [`Self::Delay`], but a zero delay resumes in the inactive region
    /// of the same time, before nonblocking assignments are published (IEEE
    /// 1800-2023 4.4.2.3).
    InactiveDelay,
    /// The process suspended at an event or level wait. The delay slot holds
    /// the index of its [`WaitSite`]. The runtime resumes the process when a
    /// counter of that site moves; the kernel then checks the wait itself.
    Wait,
    /// The process was resumed at a wait whose condition does not hold. It
    /// ran no statement and waits at the same site.
    Pending,
    /// The process staged one nonblocking assignment. The delay slot holds
    /// the index of its [`NbaSite`]. The runtime queues the staged value and
    /// resumes the process at once.
    Nba,
    /// The process returned so the runtime can charge its time-slot budget.
    /// It resumes at once.
    Yield,
    /// The process suspended the simulation (`$stop`, IEEE 1800-2023 20.2).
    /// It resumes at the point stored in [`ProcessSlots::resume`] when the
    /// simulation continues.
    Stop,
    /// A synchronous host operation. The delay slot holds its family and
    /// local site index; the same process resumes after the host responds.
    HostRequest,
    /// A program kernel called `$exit` (IEEE 1800-2023 24.7): it and every
    /// other kernel of its program instance ([`ProcessSlots::program`]) end.
    Exit,
}

impl ProcessStatus {
    /// Encoding in the status slot. Zero is never written, so a kernel that
    /// returned without reporting a status is detected.
    pub const fn code(self) -> u8 {
        match self {
            Self::Delay => 1,
            Self::Done => 2,
            Self::Finish => 3,
            Self::InactiveDelay => 4,
            Self::Wait => 5,
            Self::Pending => 6,
            Self::Nba => 7,
            Self::Yield => 8,
            Self::Stop => 9,
            Self::HostRequest => 10,
            Self::Exit => 11,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Delay),
            2 => Some(Self::Done),
            3 => Some(Self::Finish),
            4 => Some(Self::InactiveDelay),
            5 => Some(Self::Wait),
            6 => Some(Self::Pending),
            7 => Some(Self::Nba),
            8 => Some(Self::Yield),
            9 => Some(Self::Stop),
            10 => Some(Self::HostRequest),
            11 => Some(Self::Exit),
            _ => None,
        }
    }
}

/// Family of a synchronous host operation performed by a process kernel.
/// Each family numbers its request sites independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostRequestKind {
    Plusargs,
    FileIo,
    Vcd,
    Scan,
    Distribution,
}

impl HostRequestKind {
    /// Encode a request in the process's existing 64-bit delay slot.
    pub const fn encode(self, site: u32) -> u64 {
        let family = match self {
            Self::Plusargs => 1u64,
            Self::FileIo => 2,
            Self::Vcd => 3,
            Self::Scan => 4,
            Self::Distribution => 5,
        };
        (family << 32) | site as u64
    }

    /// Decode a request, rejecting unknown families and reserved bits.
    pub const fn decode(request: u64) -> Option<(Self, u32)> {
        let family = match request >> 32 {
            1 => Self::Plusargs,
            2 => Self::FileIo,
            3 => Self::Vcd,
            4 => Self::Scan,
            5 => Self::Distribution,
            _ => return None,
        };
        Some((family, request as u32))
    }
}

/// A state slot exchanged with a synchronous host operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRequestValue<A> {
    pub id: A,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
    /// The payload representation, independent of display formatting.
    #[serde(default)]
    pub value_kind: ValueKind,
    #[serde(default)]
    pub float_precision: Option<FloatPrecision>,
}

impl<A: Copy> HostRequestValue<A> {
    pub fn map<B>(&self, map: &mut impl FnMut(A) -> B) -> HostRequestValue<B> {
        HostRequestValue {
            value_kind: self.value_kind,
            id: map(self.id),
            width: self.width,
            signed: self.signed,
            is_string: self.is_string,
            float_precision: self.float_precision,
        }
    }
}

/// One synchronous host request of a process. Sites are numbered within
/// each process and family in their declaration order. Argument slots are
/// evaluated by the kernel before it suspends; result slots are written by
/// the host before the kernel resumes. `operation` and `strings` are the
/// operation family's stable, backend-independent descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRequestSite<A> {
    pub process: usize,
    pub kind: HostRequestKind,
    pub operation: u32,
    pub args: Vec<HostRequestValue<A>>,
    pub results: Vec<HostRequestValue<A>>,
    pub strings: Vec<String>,
}

impl<A: Copy> HostRequestSite<A> {
    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> HostRequestSite<B> {
        HostRequestSite {
            process: self.process,
            kind: self.kind,
            operation: self.operation,
            args: self.args.iter().map(|value| value.map(&mut map)).collect(),
            results: self
                .results
                .iter()
                .map(|value| value.map(&mut map))
                .collect(),
            strings: self.strings.clone(),
        }
    }

    pub fn slots(&self) -> impl Iterator<Item = A> + '_ {
        self.args.iter().chain(&self.results).map(|value| value.id)
    }
}

/// The source procedure a process kernel was lowered from, for diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessOrigin {
    /// The construct, such as `initial` or `always`.
    pub construct: String,
    /// `path:line:column` of the construct, when known.
    pub location: Option<String>,
    /// Fully elaborated module-instance scope that owns the process.
    pub scope: Option<String>,
}

impl std::fmt::Display for ProcessOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`", self.construct)?;
        if let Some(location) = &self.location {
            write!(f, " at {location}")?;
        }
        if let Some(scope) = &self.scope {
            write!(f, " in `{scope}`")?;
        }
        Ok(())
    }
}

/// When a procedural kernel is invoked by the runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessKind {
    /// Static declarations run before ordinary HDL procedures.
    Initializer,
    #[default]
    Regular,
    /// A final procedure runs only when simulation ends (IEEE 1800-2023 9.2.3).
    Final,
}

/// State through which one process kernel exchanges control with the runtime.
///
/// A process kernel is a resumable function. On entry it reads `resume` to
/// find where it stopped; before returning it stores the next resume point,
/// a [`ProcessStatus`] code in `status` and, for a delay, the number of time
/// units in `delay`. All three are ordinary two-state state objects, so
/// checkpoints capture a suspended process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessSlots<A> {
    /// The phase in which the runtime invokes this kernel.
    pub kind: ProcessKind,
    /// This kernel requires Simulation's process scheduler rather than Simulator.
    pub requires_timed_simulation: bool,
    /// Every delay the kernel suspends for is a constant of at least one
    /// time unit.
    pub const_delays: bool,
    /// For an `initial` procedure of a program instance (IEEE 1800-2023
    /// 24.3), the status slot of that instance's first such kernel, which
    /// every kernel of the instance names: the kernel runs in the reactive
    /// region set (24.3.1), and `$exit` and the end of the instance's
    /// kernels act on them together (24.7). `None` for design code.
    pub program: Option<A>,
    pub resume: A,
    pub status: A,
    pub delay: A,
}

impl<A> ProcessSlots<A> {
    pub fn map<B>(self, mut map: impl FnMut(A) -> B) -> ProcessSlots<B> {
        ProcessSlots {
            kind: self.kind,
            requires_timed_simulation: self.requires_timed_simulation,
            const_delays: self.const_delays,
            program: self.program.map(&mut map),
            resume: map(self.resume),
            status: map(self.status),
            delay: map(self.delay),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &A> {
        [&self.resume, &self.status, &self.delay].into_iter()
    }
}

/// Width of [`ProcessSlots::status`].
pub const PROCESS_STATUS_WIDTH: usize = 8;
/// Width of [`ProcessSlots::delay`].
pub const PROCESS_DELAY_WIDTH: usize = 64;

/// Width of every occurrence counter of an [`EventWatch`]. Counters wrap;
/// only their inequality is meaningful.
pub const EVENT_COUNTER_WIDTH: usize = 32;

/// The occurrences an event counter counts on its watched signal (IEEE
/// 1800-2023 9.4.2, Table 9-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CounterKind {
    /// Any change of the value, including the unknown bits.
    Changes,
    /// A positive edge of the least significant bit.
    Rises,
    /// A negative edge of the least significant bit.
    Falls,
}

impl CounterKind {
    pub const ALL: [Self; 3] = [Self::Changes, Self::Rises, Self::Falls];

    /// The counter kind that wakes an event domain of `kind`.
    pub const fn for_domain(kind: DomainKind) -> Self {
        match edges::polarity(kind) {
            Some(true) => Self::Rises,
            Some(false) => Self::Falls,
            None => Self::Changes,
        }
    }
}

/// A counted signal: a signal that an event control or flip-flop domain
/// watches and that one unit may store twice without the runtime checking
/// in between. Its occurrences are counted in two-state counter objects.
///
/// Generated code bumps the counters around each such store (see
/// `celox_frontend_core::watch`) and the host and the scheduler bump them on
/// their writes. Every other watched signal is checked by value. The
/// counters are ordinary state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventWatch<A> {
    pub signal: A,
    pub width: usize,
    pub four_state: bool,
    pub changes: Option<A>,
    pub rises: Option<A>,
    pub falls: Option<A>,
    /// Canonical event domains that the counter of the given kind triggers
    /// instead of edge detection by value.
    pub domains: Vec<(A, CounterKind)>,
}

impl<A: Copy> EventWatch<A> {
    pub fn counter(&self, kind: CounterKind) -> Option<A> {
        match kind {
            CounterKind::Changes => self.changes,
            CounterKind::Rises => self.rises,
            CounterKind::Falls => self.falls,
        }
    }

    /// The present counters and their kinds.
    pub fn counters(&self) -> impl Iterator<Item = (CounterKind, A)> + '_ {
        CounterKind::ALL
            .into_iter()
            .filter_map(|kind| Some((kind, self.counter(kind)?)))
    }

    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> EventWatch<B> {
        EventWatch {
            signal: map(self.signal),
            width: self.width,
            four_state: self.four_state,
            changes: self.changes.map(&mut map),
            rises: self.rises.map(&mut map),
            falls: self.falls.map(&mut map),
            domains: self
                .domains
                .iter()
                .map(|&(event, kind)| (map(event), kind))
                .collect(),
        }
    }
}

/// Counter objects a frontend declares for a signal that one of its units
/// may store twice between two runtime checks. Assembly turns them into an
/// [`EventWatch`] only if something watches the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterSlots<A> {
    pub signal: A,
    pub changes: A,
    pub rises: A,
    pub falls: A,
}

impl<A: Copy> CounterSlots<A> {
    pub fn counter(&self, kind: CounterKind) -> A {
        match kind {
            CounterKind::Changes => self.changes,
            CounterKind::Rises => self.rises,
            CounterKind::Falls => self.falls,
        }
    }

    pub fn map<B>(self, mut map: impl FnMut(A) -> B) -> CounterSlots<B> {
        CounterSlots {
            signal: map(self.signal),
            changes: map(self.changes),
            rises: map(self.rises),
            falls: map(self.falls),
        }
    }
}

/// An event or level wait of a process kernel. A kernel suspended with
/// [`ProcessStatus::Wait`] stores the index of the site among its process's
/// sites in its delay slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSite<A> {
    /// Index of the process in [`RuntimeSchema::processes`].
    pub process: usize,
    /// Occurrences that may satisfy the wait. The runtime resumes the
    /// process at the first one after the wait began; a site whose kernel
    /// checks a condition at its resume point decides itself whether it
    /// continues.
    pub terms: Vec<(A, CounterKind)>,
}

impl<A: Copy> WaitSite<A> {
    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> WaitSite<B> {
        WaitSite {
            process: self.process,
            terms: self
                .terms
                .iter()
                .map(|&(signal, kind)| (map(signal), kind))
                .collect(),
        }
    }
}

/// A nonblocking assignment of a process kernel. A kernel that returns
/// [`ProcessStatus::Nba`] stores the index of the site among its process's
/// sites in its delay slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NbaSite<A> {
    /// Index of the process in [`RuntimeSchema::processes`].
    pub process: usize,
    /// The assigned object and the static part of the assigned bit range.
    pub target: A,
    pub lsb: usize,
    pub width: usize,
    /// Staging object of `width` bits holding the assigned value, with the
    /// target's state representation.
    pub value: A,
    /// For an assignment whose position is computed at run time, the
    /// staging objects of the position.
    pub position: Option<NbaPosition<A>>,
    /// One-bit flag of a site whose destination has no other writer in the
    /// design (no other nonblocking site, no store of any unit, not a
    /// top-level port) and whose update order nothing observes (no wait,
    /// counter, event domain or combinational observer reads it, directly
    /// or through combinational logic): publication places flagged updates
    /// after the queued ones. Without the flag the assignment goes through
    /// the ordered queue.
    ///
    /// The kernel stages the value and sets the flag without returning;
    /// publication commits flagged sites and clears the flags. A kernel
    /// reaching the site again while the flag is set returns
    /// [`ProcessStatus::Nba`] first, so the runtime queues the staged value.
    pub pending: Option<A>,
}

impl<A: Copy> NbaSite<A> {
    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> NbaSite<B> {
        NbaSite {
            process: self.process,
            target: map(self.target),
            lsb: self.lsb,
            width: self.width,
            value: map(self.value),
            position: self.position.map(|position| position.map(&mut map)),
            pending: self.pending.map(&mut map),
        }
    }
}

/// The run-time position of a nonblocking assignment: two-state staging
/// objects holding a bit offset added to the site's `lsb` and the number of
/// bits written there, the low bits of the value and at most the site's
/// `width`. Zero bits write nothing, as for a position outside the target
/// or one with an unknown bit (IEEE 1800-2023 7.4.6, 11.5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NbaPosition<A> {
    pub offset: A,
    pub width: A,
}

impl<A: Copy> NbaPosition<A> {
    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> NbaPosition<B> {
        NbaPosition {
            offset: map(self.offset),
            width: map(self.width),
        }
    }
}

/// Controls which combinational stores dead store elimination preserves.
///
/// The policy is part of the runtime contract: host-side handles resolve
/// only to state the policy guarantees to keep, so a design that happens to
/// read a signal today cannot make a testbench depend on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DeadStorePolicy {
    /// Keep all stores (no dead store elimination). Default for user-facing builds.
    #[default]
    Off,
    /// Eliminate stores except those explicitly marked live via `live_signal()`
    /// and those loaded by execution units.
    PreserveListedSignals,
    /// Eliminate stores except those to top-module ports and those loaded by EUs.
    PreserveTopPorts,
    /// Eliminate stores except those to ports of *all* instances and those loaded by EUs.
    PreserveAllPorts,
}

/// One simulation tick, expressed as a power of ten seconds.
/// Valid SystemVerilog/VCD scales range from 1 fs (-15) to 100 s (2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub struct TimePrecision(i32);

impl TimePrecision {
    pub fn exponent(self) -> i32 {
        self.0
    }

    /// The magnitude and physical unit used by a VCD `$timescale` header.
    pub fn vcd_timescale(self) -> (u8, &'static str) {
        let magnitude = 10u8.pow(self.0.rem_euclid(3) as u32);
        let unit = ["s", "ms", "us", "ns", "ps", "fs"][(-self.0.div_euclid(3)) as usize];
        (magnitude, unit)
    }
}

impl Default for TimePrecision {
    /// Scale-free frontends use the same 1ps tick as the SV implementation default.
    fn default() -> Self {
        Self(-12)
    }
}

impl TryFrom<i32> for TimePrecision {
    type Error = &'static str;
    fn try_from(exponent: i32) -> Result<Self, Self::Error> {
        if (-15..=2).contains(&exponent) {
            Ok(Self(exponent))
        } else {
            Err("time precision must be between 1fs and 100s")
        }
    }
}

impl From<TimePrecision> for i32 {
    fn from(precision: TimePrecision) -> Self {
        precision.0
    }
}

/// Timed-only preparation for runtime static declarations. Ordinary folded
/// initial writes remain in the plain simulator's image; a timed simulation
/// resets their targets, executes declarations, then replays these writes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeclarationStartup<A> {
    pub reset: Vec<InitialStateValue<A>>,
    pub ordinary: Vec<InitialStateValue<A>>,
    pub snapshots: Vec<(VarAtomBase<A>, A)>,
    pub counter_snapshots: Vec<(A, A)>,
    pub probes: Vec<(VarAtomBase<A>, A)>,
}

impl<A: Copy> DeclarationStartup<A> {
    pub fn map<B>(&self, mut map: impl FnMut(A) -> B) -> DeclarationStartup<B> {
        let mut values = |values: &[InitialStateValue<A>]| {
            values
                .iter()
                .map(|value| InitialStateValue {
                    address: map(value.address),
                    data: value.data.clone(),
                })
                .collect()
        };
        let reset = values(&self.reset);
        let ordinary = values(&self.ordinary);
        DeclarationStartup {
            reset,
            ordinary,
            snapshots: self
                .snapshots
                .iter()
                .map(|(source, saved)| {
                    (
                        VarAtomBase::new(map(source.id), source.access.lsb, source.access.msb),
                        map(*saved),
                    )
                })
                .collect(),
            counter_snapshots: self
                .counter_snapshots
                .iter()
                .map(|(counter, saved)| (map(*counter), map(*saved)))
                .collect(),
            probes: self
                .probes
                .iter()
                .map(|(source, counter)| {
                    (
                        VarAtomBase::new(map(source.id), source.access.lsb, source.access.msb),
                        map(*counter),
                    )
                })
                .collect(),
        }
    }
}

/// Source-independent runtime diagnostics and observable event descriptions.
#[derive(Clone, Debug)]
pub struct RuntimeSchema<A> {
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<A>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    /// Extern functions called by `ExternCall`, by index.
    pub extern_functions: Vec<ExternFunction>,
    pub comb_observers: Vec<RuntimeCombObserver<A>>,
    /// Persistent state read directly by host-side testbench execution. These
    /// are optimization roots even when no SIR instruction loads them.
    pub testbench_read_roots: HashSet<A>,
    /// Bit ranges written by RTL execution units. External component outputs
    /// may not overlap these ranges because that would create multiple drivers.
    pub rtl_writes: HashSet<VarAtomBase<A>>,
    /// State objects written by combinational logic. Their values are
    /// derived from the other state, so state files recompute rather than
    /// compare them.
    pub comb_writes: HashSet<A>,
    /// Stateful combinational process writes, excluded from derived-value
    /// classification when the optimizer builds `comb_writes`.
    pub persistent_comb_state: HashSet<A>,
    pub declaration_startup: Option<DeclarationStartup<A>>,
    /// Dead store elimination policy the program was compiled with. Together
    /// with `live_signals` and the design it decides which combinationally
    /// written state the host may observe.
    pub dead_store_policy: DeadStorePolicy,
    /// State explicitly declared observable for dead store elimination.
    pub live_signals: HashSet<A>,
    /// Control slots of the process kernels, in kernel order.
    pub processes: Vec<ProcessSlots<A>>,
    /// Source procedures of the process kernels, in kernel order.
    pub process_origins: Vec<ProcessOrigin>,
    /// Counted signals and their occurrence counters.
    pub watches: Vec<EventWatch<A>>,
    /// Bit ranges whose changes bump an occurrence counter: the sensitivity
    /// of change-gated combinational processes and comb observers. Design
    /// code counts its own writes; the runtime counts the writes it makes,
    /// such as the nonblocking updates it publishes.
    pub occurrence_probes: Vec<(VarAtomBase<A>, A)>,
    /// Wait sites of the process kernels. A process's sites appear in the
    /// order of its local site indices.
    pub wait_sites: Vec<WaitSite<A>>,
    /// Nonblocking assignment sites of the process kernels, ordered like
    /// `wait_sites`.
    pub nba_sites: Vec<NbaSite<A>>,
    /// Synchronous host operations of the process kernels.
    pub host_request_sites: Vec<HostRequestSite<A>>,
    /// Canonical event domains whose evaluation commits the results of
    /// their blocking assignments, in the active region; their apply units
    /// commit only the nonblocking results.
    pub active_commits: Vec<A>,
    /// Objects through which design code reads the simulation time, at
    /// most one per module instance. Before each round a timed scheduler
    /// stores the current time into each, as [`TIME_SLOT_WIDTH`] bits.
    pub time_slots: Vec<A>,
    /// The state a compiled scheduler exchanges with the runtime, and the
    /// scheduler itself once one is generated. Present when the design has
    /// process kernels.
    pub scheduler: Option<SchedulerSchema<A>>,
    /// Generated units that run an edge of an event with every derived
    /// clock domain it reaches, for the events that have them.
    pub derived_edges: Vec<DerivedEdgeUnits<A>>,
}

/// The process units that run the edges of one event together with its
/// derived clock domains, the signal having already moved. Each reads
/// [`SchedulerSlots::dirty`] to know whether combinational logic must
/// settle before the domains evaluate.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DerivedEdgeUnits<A> {
    /// The canonical event.
    pub event: A,
    /// Indices among the process units of the rising and the falling edge.
    pub rising: usize,
    pub falling: usize,
}

/// Width of [`SchedulerSlots::limit`], [`SchedulerSlots::now`],
/// [`SchedulerSlots::exit_arg`], [`SchedulerSlots::count`],
/// [`SchedulerSlots::current`], [`SchedulerSlots::levels`],
/// [`SchedulerSlots::ready`], [`SchedulerSlots::clock_value`],
/// [`SchedulerSlots::clock_armed`] and every
/// [`SchedulerSlots::wake`] object, and of each element of
/// [`SchedulerSlots::clock_next`] and [`SchedulerSlots::clock_period`].
pub const SCHEDULER_TIME_WIDTH: usize = 64;
/// The most host periodic clocks a compiled scheduler runs: one element of
/// [`SchedulerSlots::clock_next`] and [`SchedulerSlots::clock_period`], and
/// one bit of the clock masks, per entry of [`SchedulerSchema::clocks`].
pub const SCHEDULER_CLOCKS: usize = 64;
/// Width of [`SchedulerSlots::exit`], [`SchedulerSlots::finish`],
/// [`SchedulerSlots::cont`], [`SchedulerSlots::dirty`] and every [`SchedulerSlots::waiting`] and
/// [`SchedulerSlots::baseline`] object.
pub const SCHEDULER_FLAG_WIDTH: usize = 8;

/// Hidden state objects through which a compiled scheduler and the runtime
/// exchange a run of time slots. Between runs they hold no state: the
/// runtime's scheduling state is the source of truth, so state files leave
/// them out.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchedulerSlots<A> {
    /// The last time the run may reach.
    pub limit: A,
    /// The time of the last slot the run reached.
    pub now: A,
    /// Why the run returned, a [`SchedulerExit`] code.
    pub exit: A,
    /// The process an exit concerns.
    pub exit_arg: A,
    /// Set when a `$finish` record was emitted in the unit just run.
    pub finish: A,
    /// Where a shared combinational settle continues.
    pub cont: A,
    /// Set when state combinational logic reads may have changed since its
    /// last settle.
    pub dirty: A,
    /// The number of time slots the run ran.
    pub count: A,
    /// The kernel running when the run returned, `u64::MAX` for none.
    pub current: A,
    /// The levels of the event signals at the start of the last slot, one
    /// bit per event in event order.
    pub levels: A,
    /// The kernels ready to run in the current slot, one bit per kernel in
    /// kernel order.
    pub ready: A,
    /// The time each process kernel resumes at, in kernel order, valid
    /// where [`Self::waiting`] is set.
    pub wake: Vec<A>,
    /// Whether each process kernel waits for its wake time or event.
    pub waiting: Vec<A>,
    /// The level a kernel waiting for an event last saw its signal at.
    pub baseline: Vec<A>,
    /// The time of each host clock's next edge, one element per entry of
    /// [`SchedulerSchema::clocks`], valid where [`Self::clock_armed`] is set.
    pub clock_next: A,
    /// The period of each host clock, one element per clock.
    pub clock_period: A,
    /// The level each host clock's next edge sets, one bit per clock.
    pub clock_value: A,
    /// Whether each host clock has a next edge, one bit per clock.
    pub clock_armed: A,
}

impl<A> SchedulerSlots<A> {
    pub fn map<B>(self, mut map: impl FnMut(A) -> B) -> SchedulerSlots<B> {
        SchedulerSlots {
            limit: map(self.limit),
            now: map(self.now),
            exit: map(self.exit),
            exit_arg: map(self.exit_arg),
            finish: map(self.finish),
            cont: map(self.cont),
            dirty: map(self.dirty),
            count: map(self.count),
            current: map(self.current),
            levels: map(self.levels),
            ready: map(self.ready),
            wake: self.wake.into_iter().map(&mut map).collect(),
            waiting: self.waiting.into_iter().map(&mut map).collect(),
            baseline: self.baseline.into_iter().map(&mut map).collect(),
            clock_next: map(self.clock_next),
            clock_period: map(self.clock_period),
            clock_value: map(self.clock_value),
            clock_armed: map(self.clock_armed),
        }
    }

    /// Every slot.
    pub fn iter(&self) -> impl Iterator<Item = &A> {
        [
            &self.limit,
            &self.now,
            &self.exit,
            &self.exit_arg,
            &self.finish,
            &self.cont,
            &self.dirty,
            &self.count,
            &self.current,
            &self.levels,
            &self.ready,
            &self.clock_next,
            &self.clock_period,
            &self.clock_value,
            &self.clock_armed,
        ]
        .into_iter()
        .chain(&self.wake)
        .chain(&self.waiting)
        .chain(&self.baseline)
    }
}

/// Why a run of the compiled scheduler returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum SchedulerExit {
    /// The next time slot is after the limit.
    Limit = 0,
    /// A process or a `$finish` record finished the simulation.
    Finish = 1,
    /// The delay of process [`SchedulerSlots::exit_arg`] overflows the
    /// simulation time.
    DelayOverflow = 2,
    /// A process suspended in a way the scheduler does not compile.
    Internal = 3,
    /// The next edge of host clock [`SchedulerSlots::exit_arg`] overflows
    /// the simulation time.
    ClockOverflow = 4,
}

impl SchedulerExit {
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Limit),
            1 => Some(Self::Finish),
            2 => Some(Self::DelayOverflow),
            3 => Some(Self::Internal),
            4 => Some(Self::ClockOverflow),
            _ => None,
        }
    }
}

/// A design's compiled scheduler: the process unit that runs time slots of
/// the kernels it compiles, and its slots.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchedulerSchema<A> {
    pub slots: SchedulerSlots<A>,
    /// The index of the scheduler unit among the process units, after the
    /// kernels; `None` when no scheduler was generated.
    pub unit: Option<usize>,
    /// Per process kernel, why the scheduler does not run it, or `None` when
    /// it does.
    pub fallback: Vec<Option<String>>,
    /// The event signals the scheduler's kernels write or its host clocks
    /// may drive, in the order of the bits of [`SchedulerSlots::levels`].
    pub events: Vec<A>,
    /// The event signals a host periodic clock may drive inside the
    /// scheduler's slots, in the order of its clock slots.
    pub clocks: Vec<A>,
    /// Per process kernel, the event signals it writes, as indices into
    /// [`Self::events`]; empty for a kernel the scheduler does not run.
    pub kernel_events: Vec<Vec<usize>>,
}

impl<A> SchedulerSchema<A> {
    pub fn map<B>(self, map: impl FnMut(A) -> B) -> SchedulerSchema<B> {
        let mut map = map;
        SchedulerSchema {
            slots: self.slots.map(&mut map),
            unit: self.unit,
            fallback: self.fallback,
            events: self.events.into_iter().map(&mut map).collect(),
            clocks: self.clocks.into_iter().map(map).collect(),
            kernel_events: self.kernel_events,
        }
    }
}

/// Width of every [`RuntimeSchema::time_slots`] object.
pub const TIME_SLOT_WIDTH: usize = 64;

impl<A> Default for RuntimeSchema<A> {
    fn default() -> Self {
        Self {
            runtime_errors: HashMap::default(),
            runtime_event_sites: Vec::new(),
            extern_functions: Vec::new(),
            comb_observers: Vec::new(),
            testbench_read_roots: HashSet::default(),
            rtl_writes: HashSet::default(),
            comb_writes: HashSet::default(),
            persistent_comb_state: HashSet::default(),
            declaration_startup: None,
            dead_store_policy: DeadStorePolicy::Off,
            live_signals: HashSet::default(),
            processes: Vec::new(),
            process_origins: Vec::new(),
            watches: Vec::new(),
            occurrence_probes: Vec::new(),
            wait_sites: Vec::new(),
            nba_sites: Vec::new(),
            host_request_sites: Vec::new(),
            active_commits: Vec::new(),
            time_slots: Vec::new(),
            scheduler: None,
            derived_edges: Vec::new(),
        }
    }
}

/// Source-independent event-domain topology after elaboration.
///
/// Addresses are already flattened. Source paths and frontend IDs used only
/// for diagnostics or lookup deliberately live outside this structure.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + Eq + std::hash::Hash + Ord",
    deserialize = "A: Deserialize<'de> + Eq + std::hash::Hash + Ord"
))]
pub struct EventTopology<A> {
    /// Alias event address to the canonical event-domain address.
    pub aliases: HashMap<A, A>,
    /// Canonical event domains in evaluation order.
    pub ordered_events: Vec<A>,
    /// Canonical clocks whose value may be changed by another event domain.
    pub cascaded_events: BTreeSet<A>,
    /// Canonical asynchronous/synchronous reset to its canonical clock.
    pub reset_clocks: HashMap<A, A>,
    /// Canonical event to the state-derived event domains an occurrence of
    /// it can trigger after its domain commits, in
    /// [`Self::ordered_events`] order: domains whose clock is computed from
    /// state written by the event's domain, or by a domain it triggers in
    /// turn, such as a register bit or a divider. Clocks listed in
    /// [`Self::combinational_derived`] for the same event are excluded.
    /// Events that trigger nothing are absent.
    #[serde(default)]
    pub derived_events: BTreeMap<A, Vec<A>>,
    /// Canonical event to the clocks computed combinationally from its own
    /// signal, such as `clk & enable` or `~clk`, in
    /// [`Self::ordered_events`] order. Their edges coincide with a change of
    /// the event's signal: they are judged from the state before that edge
    /// and their domains evaluate together with the event's own. A change the
    /// commit then causes is a further edge in the cascade rounds.
    #[serde(default)]
    pub combinational_derived: BTreeMap<A, Vec<CombDerivedClock<A>>>,
    /// Canonical events whose [`Self::combinational_derived`] domains can
    /// run one after another, those first and the event's own last, with the
    /// result of evaluating them together: no domain reads or writes state
    /// an earlier one writes, and none has side effects.
    #[serde(default)]
    pub independent_derived: BTreeSet<A>,
    /// Canonical events whose signal combinational logic reads. Moving such
    /// a signal needs a settle before its domains evaluate.
    #[serde(default)]
    pub combinationally_read: BTreeSet<A>,
    /// Canonical events whose own domain reads no level of their signal: its
    /// flip-flops do not sample the signal as data. With
    /// [`Self::combinationally_read`] it says whether a run of the domain
    /// observes the signal's level. Absent means it may.
    #[serde(default)]
    pub level_unread_by_domain: BTreeSet<A>,
}

/// A clock computed combinationally from an event's signal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombDerivedClock<A> {
    /// The canonical derived clock.
    pub clock: A,
    /// The clock's value as a function of the event's signal and of state
    /// variables, when its combinational cone is small enough. It lets an
    /// event-based tick judge the edge without settling the design.
    pub gate: Option<Vec<GateOp<A>>>,
}

/// One step of a postfix program computing a derived clock from its source
/// event's signal and from state variables. Values are at most 64 bits wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOp<A> {
    /// Push the level of the source event's signal, 0 or 1.
    Source,
    /// Push `width` bits of a state variable starting at bit `lsb`.
    Load {
        address: A,
        lsb: u32,
        width: u32,
    },
    Const(u64),
    /// Bitwise complement within `width` bits.
    Not {
        width: u32,
    },
    LogicNot,
    ReduceAnd {
        width: u32,
    },
    ReduceOr,
    ReduceXor,
    And,
    Or,
    Xor,
    Eq,
    Ne,
    LogicAnd,
    LogicOr,
    /// Keep `width` bits starting at bit `lsb`.
    Slice {
        lsb: u32,
        width: u32,
    },
    /// Pop the else value, the then value and the condition.
    Mux,
}

impl<A> GateOp<A> {
    pub fn map_addr<B>(self, f: impl FnOnce(A) -> B) -> GateOp<B> {
        match self {
            GateOp::Source => GateOp::Source,
            GateOp::Load {
                address,
                lsb,
                width,
            } => GateOp::Load {
                address: f(address),
                lsb,
                width,
            },
            GateOp::Const(value) => GateOp::Const(value),
            GateOp::Not { width } => GateOp::Not { width },
            GateOp::LogicNot => GateOp::LogicNot,
            GateOp::ReduceAnd { width } => GateOp::ReduceAnd { width },
            GateOp::ReduceOr => GateOp::ReduceOr,
            GateOp::ReduceXor => GateOp::ReduceXor,
            GateOp::And => GateOp::And,
            GateOp::Or => GateOp::Or,
            GateOp::Xor => GateOp::Xor,
            GateOp::Eq => GateOp::Eq,
            GateOp::Ne => GateOp::Ne,
            GateOp::LogicAnd => GateOp::LogicAnd,
            GateOp::LogicOr => GateOp::LogicOr,
            GateOp::Slice { lsb, width } => GateOp::Slice { lsb, width },
            GateOp::Mux => GateOp::Mux,
        }
    }
}

impl<A> Default for EventTopology<A> {
    fn default() -> Self {
        Self {
            aliases: HashMap::default(),
            ordered_events: Vec::new(),
            cascaded_events: BTreeSet::new(),
            reset_clocks: HashMap::default(),
            derived_events: BTreeMap::new(),
            combinational_derived: BTreeMap::new(),
            independent_derived: BTreeSet::new(),
            combinationally_read: BTreeSet::new(),
            level_unread_by_domain: BTreeSet::new(),
        }
    }
}

impl<A: Copy + Eq + std::hash::Hash> EventTopology<A> {
    pub fn canonical(&self, address: A) -> A {
        self.aliases.get(&address).copied().unwrap_or(address)
    }

    pub fn len(&self) -> usize {
        self.ordered_events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ordered_events.is_empty()
    }
}

/// Time unit and rounding precision of one source scope, in powers of ten seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeScale {
    pub unit: TimePrecision,
    pub precision: TimePrecision,
}

/// Backend-neutral semantic design data after hierarchy flattening.
///
/// This is intentionally not a frontend lookup table: every state object is
/// keyed by its flattened semantic address, and no source-language AST or path
/// type is retained.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + Eq + std::hash::Hash + Ord",
    deserialize = "A: Deserialize<'de> + Eq + std::hash::Hash + Ord"
))]
pub struct ElaboratedDesign<A> {
    /// Physical duration of one global simulation tick.
    #[serde(default)]
    pub time_precision: TimePrecision,
    /// Local time units and precisions retained for reflected instance queries.
    pub instance_time_scales: HashMap<InstanceId, TimeScale>,
    pub state_objects: HashMap<A, VariableMetadata>,
    pub events: EventTopology<A>,
    pub initial_state: Vec<InitialStateValue<A>>,
}

impl<A> Default for ElaboratedDesign<A> {
    fn default() -> Self {
        Self {
            time_precision: TimePrecision::default(),
            instance_time_scales: HashMap::default(),
            state_objects: HashMap::default(),
            events: EventTopology::default(),
            initial_state: Vec::new(),
        }
    }
}

impl<A: Copy + Eq + std::hash::Hash> ElaboratedDesign<A> {
    /// Effective kinds of canonical event domains. Infer only unambiguous
    /// one-bit alias edges by polarity; declaration metadata and signal types stay intact.
    /// Opposite edges on one net require separate dispatch domains and retain
    /// their existing canonical kind here.
    pub fn event_kinds(&self) -> HashMap<A, DomainKind> {
        let mut inferred: HashMap<A, Option<DomainKind>> = HashMap::default();
        for (address, metadata) in &self.state_objects {
            if metadata.width != 1
                || !matches!(
                    metadata.kind,
                    DomainKind::ClockPosedge
                        | DomainKind::ClockNegedge
                        | DomainKind::ResetAsyncHigh
                        | DomainKind::ResetAsyncLow
                )
            {
                continue;
            }
            let canonical = self.events.canonical(*address);
            let kind = inferred.entry(canonical).or_insert(Some(metadata.kind));
            if let Some(previous) = *kind {
                if edges::polarity(previous) != edges::polarity(metadata.kind) {
                    *kind = None;
                } else if previous != metadata.kind {
                    // A clock and asynchronous reset can share one edge. Use
                    // the clock spelling for the mixed role, independent of
                    // state-object iteration order.
                    *kind = Some(if edges::polarity(previous) == Some(true) {
                        DomainKind::ClockPosedge
                    } else {
                        DomainKind::ClockNegedge
                    });
                }
            }
        }
        self.events
            .ordered_events
            .iter()
            .filter_map(|address| {
                let metadata = self.state_objects.get(address)?;
                let kind = if metadata.kind == DomainKind::Other && metadata.width == 1 {
                    inferred
                        .get(address)
                        .copied()
                        .flatten()
                        .unwrap_or(metadata.kind)
                } else {
                    metadata.kind
                };
                Some((*address, kind))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatPrecision {
    F32,
    F64,
}
impl FloatPrecision {
    pub fn width(self) -> usize {
        match self {
            Self::F32 => 32,
            Self::F64 => 64,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Atan2,
    Hypot,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatUnaryOp {
    Neg,
    ToBool,
    FromSigned { width: usize },
    FromUnsigned { width: usize },
    ToSigned { width: usize, round: bool },
    ToUnsigned { width: usize, round: bool },
    Convert { to: FloatPrecision },
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    DivU,
    DivS,
    RemU,
    RemS,
    And,
    Or,
    Xor,
    Shl, // Logical Shift Left (<<)
    Shr, // Logical Shift Right (>>)
    Sar, // Arithmetic Shift Right (>>>)
    Eq,
    Ne,
    EqCase,
    NeCase,
    LtU,
    LtS, // Less Than (Unsigned / Signed)
    LeU,
    LeS, // Less Equal
    GtU,
    GtS, // Greater Than
    GeU,
    GeS, // Greater Equal
    LogicAnd,
    LogicOr,
    EqWildcard,
    NeWildcard,
    Float {
        op: FloatBinaryOp,
        precision: FloatPrecision,
    },
}

impl BinaryOp {
    /// Whether the operation is commutative (a op b == b op a).
    pub fn is_commutative(&self) -> bool {
        matches!(
            self,
            BinaryOp::Add
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
        )
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            BinaryOp::Float { op, precision } => return write!(f, "Float{precision:?}{op:?}"),
            BinaryOp::Add => "Add",
            BinaryOp::Sub => "Sub",
            BinaryOp::Mul => "Mul",
            BinaryOp::DivU => "DivU",
            BinaryOp::DivS => "DivS",
            BinaryOp::RemU => "RemU",
            BinaryOp::RemS => "RemS",
            BinaryOp::And => "And",
            BinaryOp::Or => "Or",
            BinaryOp::Xor => "Xor",
            BinaryOp::Shl => "Shl",
            BinaryOp::Shr => "Shr",
            BinaryOp::Sar => "Sar",
            BinaryOp::Eq => "Eq",
            BinaryOp::Ne => "Ne",
            BinaryOp::EqCase => "EqCase",
            BinaryOp::NeCase => "NeCase",
            BinaryOp::LtU => "LtU",
            BinaryOp::LtS => "LtS",
            BinaryOp::LeU => "LeU",
            BinaryOp::LeS => "LeS",
            BinaryOp::GtU => "GtU",
            BinaryOp::GtS => "GtS",
            BinaryOp::GeU => "GeU",
            BinaryOp::GeS => "GeS",
            BinaryOp::LogicAnd => "LogicAnd",
            BinaryOp::LogicOr => "LogicOr",
            BinaryOp::EqWildcard => "EqWildcard",
            BinaryOp::NeWildcard => "NeWildcard",
        };
        write!(f, "{}", op_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Ident,
    /// Convert a four-state value to two-state form. Unknown bits become zero:
    /// `(value, mask) -> (value & !mask, 0)`.
    ToTwoState,
    Minus,
    BitNot,
    LogicNot,
    And,
    Or,
    Xor,
    PopCount,
    CountLeadingZeros,
    CountTrailingZeros,
    Float {
        op: FloatUnaryOp,
        precision: FloatPrecision,
    },
}

impl UnaryOp {
    /// Return the canonical result width for an operand of `operand_width` bits.
    ///
    /// Bit-count operations return a value in `0..=operand_width`, which needs
    /// `ceil(log2(operand_width + 1))` bits.  Computing that as the bit length
    /// of `operand_width` avoids overflowing when the operand width is
    /// `usize::MAX`.
    pub fn result_width(self, operand_width: usize) -> usize {
        match self {
            UnaryOp::Float { op, precision } => match op {
                FloatUnaryOp::ToBool => 1,
                FloatUnaryOp::ToSigned { width, .. } | FloatUnaryOp::ToUnsigned { width, .. } => {
                    width
                }
                FloatUnaryOp::Convert { to } => to.width(),
                _ => precision.width(),
            },
            UnaryOp::LogicNot | UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => 1,
            UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot => {
                operand_width
            }
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                usize::BITS as usize - operand_width.leading_zeros() as usize
            }
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            UnaryOp::Float { op, precision } => return write!(f, "Float{precision:?}{op:?}"),
            UnaryOp::Ident => "Ident",
            UnaryOp::ToTwoState => "ToTwoState",
            UnaryOp::Minus => "Minus",
            UnaryOp::BitNot => "BitNot",
            UnaryOp::LogicNot => "LogicNot",
            UnaryOp::And => "And",
            UnaryOp::Or => "Or",
            UnaryOp::Xor => "Xor",
            UnaryOp::PopCount => "PopCount",
            UnaryOp::CountLeadingZeros => "CountLeadingZeros",
            UnaryOp::CountTrailingZeros => "CountTrailingZeros",
        };
        write!(f, "{}", op_str)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct BitAccess {
    pub lsb: usize,
    pub msb: usize,
}
impl BitAccess {
    pub fn new(lsb: usize, msb: usize) -> Self {
        debug_assert!(lsb <= msb, "lsb must be less than or equal to msb");
        Self { lsb, msb }
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        !(self.msb < other.lsb || other.msb < self.lsb)
    }

    /// Calculates the atomic bit ranges for a given access range and a set of boundaries.
    pub fn calculate_atoms(&self, bounds: &BTreeSet<usize>) -> Vec<Self> {
        use std::ops::Bound::*;
        let mut atoms = Vec::new();
        let mut current_lsb = self.lsb;

        // Iterate through the boundaries that are within the access range
        // Excluded(lsb) to Included(msb) handles lsb == msb case naturally (returns empty iterator)
        for &bound in bounds.range((Excluded(self.lsb), Included(self.msb))) {
            atoms.push(Self::new(current_lsb, bound - 1));
            current_lsb = bound;
        }

        // Add the last atom
        if current_lsb <= self.msb {
            atoms.push(Self::new(current_lsb, self.msb));
        }

        atoms
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct VarAtomBase<A> {
    pub id: A,
    pub access: BitAccess,
}
impl<A> VarAtomBase<A> {
    pub fn new(id: A, lsb: usize, msb: usize) -> Self {
        Self {
            id,
            access: BitAccess { lsb, msb },
        }
    }
}
impl fmt::Display for BitAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.lsb == self.msb {
            write!(f, "[{}]", self.lsb)
        } else {
            write!(f, "[{}:{}]", self.msb, self.lsb)
        }
    }
}

impl<A> fmt::Display for VarAtomBase<A>
where
    A: fmt::Display + std::hash::Hash + Eq,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.id, self.access)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleId(pub usize);

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mod{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceId(pub usize);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inst{}", self.0)
    }
}

/// Dense source-independent identity of one flattened state object.
///
/// Frontends assign this identity during design projection. Source variable
/// IDs must not cross into SIR optimization, layout, or backend code.
#[derive(
    Debug, Clone, Copy, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct StateObjectId(pub u32);

impl StateObjectId {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for StateObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "state{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbsoluteAddrBase<V> {
    pub instance_id: InstanceId,
    pub var_id: V,
}

impl<V: fmt::Display> fmt::Display for AbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AbsoluteAddr({}, {})", self.instance_id, self.var_id)
    }
}

pub const STABLE_REGION: u32 = 0;
pub const WORKING_REGION: u32 = 1;
pub const SPARSE_WORKING_REGION: u32 = 2;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedVarAddrBase<V> {
    pub region: u32,
    pub var_id: V,
}

impl<V: fmt::Display> fmt::Display for RegionedVarAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedVarAddr(region={}, {})",
            self.region, self.var_id
        )
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedAbsoluteAddrBase<V> {
    pub region: u32,
    pub instance_id: InstanceId,
    pub var_id: V,
}

pub type StateAddr = AbsoluteAddrBase<StateObjectId>;
pub type RegionedStateAddr = RegionedAbsoluteAddrBase<StateObjectId>;

impl<V: Copy> RegionedAbsoluteAddrBase<V> {
    pub fn from_absolute_addr(region: u32, addr: AbsoluteAddrBase<V>) -> Self {
        Self {
            region,
            instance_id: addr.instance_id,
            var_id: addr.var_id,
        }
    }

    pub fn absolute_addr(&self) -> AbsoluteAddrBase<V> {
        AbsoluteAddrBase {
            instance_id: self.instance_id,
            var_id: self.var_id,
        }
    }
}

impl<V: fmt::Display> fmt::Display for RegionedAbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedAbsoluteAddr(region={}, {}, {})",
            self.region, self.instance_id, self.var_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applying the runs to a target equals the packed merge: written bits
    /// take the value, unwritten bits keep the target's.
    #[test]
    fn packed_write_runs_match_the_packed_merge() {
        let width = 203;
        let all: BigUint = (BigUint::from(1u8) << width) - 1u8;
        let pattern = |seed: u64| {
            (0..width).fold(BigUint::default(), |acc, bit| {
                let set = (seed.wrapping_mul(bit as u64 + 1) >> 7).is_multiple_of(3);
                if set {
                    acc | (BigUint::from(1u8) << bit)
                } else {
                    acc
                }
            })
        };
        let (value, mask, current) = (pattern(0x9e37), pattern(0x51ed), pattern(0x2545));
        let written_masks = [
            BigUint::default(),
            all.clone(),
            pattern(0x7f4a) | (BigUint::from(1u8) << (width + 3)),
            (BigUint::from(1u8) << 64) - 1u8,
        ];
        for written in &written_masks {
            for four_state in [false, true] {
                let runs =
                    InitialStateData::packed_write_runs(&value, &mask, written, width, four_state);
                let (mut got_value, mut got_mask) = (current.clone(), BigUint::default());
                for run in &runs {
                    let run_mask = ((BigUint::from(1u8) << run.bit_width) - 1u8) << run.bit_offset;
                    let keep = &all ^ &run_mask;
                    got_value = (got_value & &keep)
                        | (BigUint::from_bytes_le(&run.value_bytes) << run.bit_offset);
                    got_mask = (got_mask & &keep)
                        | (BigUint::from_bytes_le(&run.mask_bytes) << run.bit_offset);
                }
                let written = written & &all;
                let keep = &all ^ &written;
                let (want_value, want_mask) = if four_state {
                    ((&current & &keep) | (&value & &written), &mask & &written)
                } else {
                    let known = &all ^ (&mask & &all);
                    (
                        (&current & &keep) | (&value & &written & known),
                        BigUint::default(),
                    )
                };
                assert_eq!(
                    (got_value, got_mask),
                    (want_value, want_mask),
                    "{four_state}"
                );
                assert!(
                    runs.windows(2).all(|pair| {
                        pair[0].bit_offset + pair[0].bit_width < pair[1].bit_offset
                    })
                );
            }
        }
    }

    #[test]
    fn host_request_encoding_keeps_families_and_full_site_indices_distinct() {
        for kind in [
            HostRequestKind::Plusargs,
            HostRequestKind::FileIo,
            HostRequestKind::Vcd,
            HostRequestKind::Scan,
            HostRequestKind::Distribution,
        ] {
            for site in [0, 1, u32::MAX] {
                assert_eq!(
                    HostRequestKind::decode(kind.encode(site)),
                    Some((kind, site))
                );
            }
        }
        for invalid in [0, u32::MAX as u64, 6u64 << 32, 1u64 << 63] {
            assert_eq!(HostRequestKind::decode(invalid), None);
        }
        assert_eq!(
            ProcessStatus::from_code(10),
            Some(ProcessStatus::HostRequest)
        );
        assert_eq!(ProcessStatus::from_code(11), Some(ProcessStatus::Exit));
    }

    #[test]
    fn host_request_mapping_relocates_operands_and_results() {
        let value = |id| HostRequestValue {
            value_kind: ValueKind::StrRef,
            id,
            width: 17,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        let site = HostRequestSite {
            process: 2,
            kind: HostRequestKind::Plusargs,
            operation: 1,
            args: vec![value(3u32)],
            results: vec![value(5)],
            strings: vec!["COUNT=%d".into()],
        };
        let relocated = site.map(|id| id + 10);
        assert_eq!(relocated.slots().collect::<Vec<_>>(), vec![13, 15]);
        assert_eq!(relocated.process, 2);
        assert_eq!(relocated.args[0].width, 17);
        assert!(relocated.results[0].signed);
        assert_eq!(relocated.args[0].value_kind, ValueKind::StrRef);
        assert_eq!(relocated.strings, site.strings);
    }

    #[test]
    fn bit_access_splits_only_at_internal_boundaries() {
        let access = BitAccess::new(4, 11);
        let bounds = [0, 4, 7, 12, 20].into_iter().collect();

        assert_eq!(
            access.calculate_atoms(&bounds),
            vec![BitAccess::new(4, 6), BitAccess::new(7, 11)]
        );
    }

    #[test]
    fn design_ids_and_addresses_have_stable_display() {
        let address = AbsoluteAddrBase {
            instance_id: InstanceId(42),
            var_id: 7,
        };

        assert_eq!(ModuleId(3).to_string(), "mod3");
        assert_eq!(InstanceId(42).to_string(), "inst42");
        assert_eq!(StateObjectId(7).to_string(), "state7");
        assert_eq!(address.to_string(), "AbsoluteAddr(inst42, 7)");
    }

    #[test]
    fn regioned_address_round_trips_semantic_identity() {
        let address = AbsoluteAddrBase {
            instance_id: InstanceId(2),
            var_id: 9,
        };
        let regioned = RegionedAbsoluteAddrBase::from_absolute_addr(WORKING_REGION, address);

        assert_eq!(regioned.absolute_addr(), address);
        assert_eq!(regioned.region, WORKING_REGION);
    }

    #[test]
    fn semantic_operator_contracts_are_source_independent() {
        assert!(BinaryOp::Add.is_commutative());
        assert!(!BinaryOp::Sub.is_commutative());
        assert_eq!(UnaryOp::LogicNot.result_width(128), 1);
        assert_eq!(UnaryOp::PopCount.result_width(128), 8);
    }

    #[test]
    fn initial_state_and_runtime_error_schemas_accept_design_owned_ids() {
        let initial = InitialStateValue {
            address: AbsoluteAddrBase {
                instance_id: InstanceId(1),
                var_id: 7u32,
            },
            data: InitialStateData::Writes(vec![InitialStateWriteRun {
                bit_offset: 3,
                bit_width: 5,
                value_bytes: vec![0x15],
                mask_bytes: vec![0],
            }]),
        };
        let error = RuntimeErrorInfo {
            message: "failed".to_string(),
            signals: vec![initial.address],
        };
        let mut runtime = RuntimeSchema::default();
        runtime.runtime_errors.insert(1, error.clone());
        runtime.runtime_event_sites.push(RuntimeEventSite {
            kind: RuntimeEventKind::Report(Severity::Fatal),
            template: Some("failed".to_string()),
            sizing: DisplaySizing::Veryl,
            scope: None,
            location: None,
            assertion: None,
            deferred: None,
            arg_widths: Vec::new(),
            arg_signed: Vec::new(),
            arg_is_string: Vec::new(),
            arg_float_precision: Vec::new(),
            arg_is_handle: Vec::new(),
        });
        runtime.comb_observers.push(RuntimeCombObserver {
            site_id: 0,
            activation_group: 0,
            sensitivity: vec![VarAtomBase {
                id: initial.address,
                access: BitAccess { lsb: 3, msb: 7 },
            }],
            occurrence_counters: Vec::new(),
            written_inputs: vec![initial.address],
        });
        runtime.testbench_read_roots.insert(initial.address);

        assert_eq!(error.signals, vec![initial.address]);
        assert!(matches!(initial.data, InitialStateData::Writes(_)));
        assert_eq!(runtime.runtime_errors[&1], error);
        assert_eq!(runtime.runtime_event_sites.len(), 1);
        assert_eq!(runtime.comb_observers[0].sensitivity[0].id, initial.address);
        assert!(runtime.testbench_read_roots.contains(&initial.address));
    }

    #[test]
    fn variable_metadata_preserves_elaborated_shape_and_domain() {
        let metadata = VariableMetadata {
            width: 32,
            is_4state: true,
            kind: DomainKind::Other,
            type_kind: PortTypeKind::Logic,
            array_dims: vec![4],
            value_kind: Default::default(),
        };

        assert_eq!(metadata.width, 32);
        assert_eq!(metadata.array_dims, vec![4]);
    }

    #[test]
    fn elaborated_design_uses_flat_addresses_and_canonical_event_topology() {
        let mut design = ElaboratedDesign::<u32>::default();
        design.state_objects.insert(
            10,
            VariableMetadata {
                width: 1,
                is_4state: false,
                kind: DomainKind::ClockPosedge,
                type_kind: PortTypeKind::Clock,
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
        );
        design.events.aliases.insert(11, 10);
        design.events.ordered_events.push(10);

        assert_eq!(design.events.canonical(11), 10);
        assert_eq!(design.events.canonical(12), 12);
        assert_eq!(design.events.len(), 1);
        assert!(!design.events.is_empty());
        assert_eq!(design.state_objects[&10].width, 1);
    }
    #[test]
    fn alias_event_kinds_merge_clock_and_reset_by_polarity() {
        for (clock, reset) in [
            (DomainKind::ClockPosedge, DomainKind::ResetAsyncHigh),
            (DomainKind::ClockNegedge, DomainKind::ResetAsyncLow),
        ] {
            for roles in [[clock, reset], [reset, clock]] {
                let mut design = ElaboratedDesign::<u32>::default();
                for (address, kind) in [(10, DomainKind::Other), (11, roles[0]), (12, roles[1])] {
                    design.state_objects.insert(
                        address,
                        VariableMetadata {
                            width: 1,
                            is_4state: true,
                            kind,
                            type_kind: PortTypeKind::Logic,
                            array_dims: Vec::new(),
                            value_kind: Default::default(),
                        },
                    );
                }
                design.events.ordered_events = vec![10];
                design.events.aliases.insert(11, 10);
                design.events.aliases.insert(12, 10);
                assert_eq!(design.event_kinds()[&10], clock);
                assert_eq!(design.state_objects[&10].kind, DomainKind::Other);
            }
        }
    }

    #[test]
    fn alias_event_kinds_are_separate_from_declaration_metadata() {
        let mut design = ElaboratedDesign::<u32>::default();
        for (address, kind) in [
            (10, DomainKind::Other),
            (11, DomainKind::ClockPosedge),
            (20, DomainKind::Other),
            (21, DomainKind::ClockNegedge),
            (30, DomainKind::Other),
            (31, DomainKind::ClockPosedge),
            (32, DomainKind::ClockNegedge),
        ] {
            design.state_objects.insert(
                address,
                VariableMetadata {
                    width: 1,
                    is_4state: false,
                    kind,
                    type_kind: PortTypeKind::Logic,
                    array_dims: Vec::new(),
                    value_kind: Default::default(),
                },
            );
        }
        design.events.ordered_events = vec![10, 20, 30];
        for (alias, canonical) in [(11, 10), (21, 20), (31, 30), (32, 30)] {
            design.events.aliases.insert(alias, canonical);
        }
        let kinds = design.event_kinds();
        assert_eq!(kinds[&10], DomainKind::ClockPosedge);
        assert_eq!(kinds[&20], DomainKind::ClockNegedge);
        assert_eq!(
            kinds[&30],
            DomainKind::Other,
            "conflicting aliases do not select one edge"
        );
        assert_eq!(design.state_objects[&10].kind, DomainKind::Other);
        assert_eq!(design.state_objects[&20].type_kind, PortTypeKind::Logic);
    }
}
