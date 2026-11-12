//! Subroutines lowered out of line: each into one SIR function unit of the
//! module, which its call sites enter through `SIRInstruction::Call`
//! instead of inlining the body.
//!
//! Only a test switch turns this on (`AnalysisOptions::force_out_of_line_calls`):
//! every subroutine that can be lowered so is, and every other one stays
//! inlined. A subroutine qualifies when it cannot suspend its caller other
//! than for a host request, nor make one a unit cannot make, both
//! transitively (see [`Reason`]; its prints and messages are runtime
//! events, which a unit raises), has no `ref` formal of unpacked structure
//! type (the callees inlined into its unit may), and its unit touches no
//! state but its frame, the variables it reads, and the module variables
//! it writes that the lowering does not keep hidden for a process, such as
//! the static storage of subroutines and static class properties; a
//! combinational process calls it only if it writes nothing but that static
//! storage. Nor may it make a store that assembly instruments per store
//! (a nonblocking one, or one into what a wait, an event control or
//! combinational logic observes), since assembly sees only the stores of
//! the units the runtime runs: the frontend keeps out the stores into what
//! the module's source may observe ([`ProcModule::observe_storage`]), and assembly
//! rejects a function that makes one all the same.
//!
//! A unit whose body suspends the process, for a host request (`$sformatf`,
//! file I/O, `$sscanf`, plusargs, `$system`, the distribution functions,
//! `process::self()`, `srandom`, `get_randstate`, `set_randstate` and a
//! semaphore's `new`), a
//! `#` delay, `$stop` or `$finish`, is resumable (see [`Function::resume`]):
//! a suspension stores the unit's resume point and returns with the status,
//! and each caller up the chain does the same at its call, down to the
//! process kernel, which suspends. The kernel resumes
//! at its call and calls again; each frame continues where it stopped. A
//! request's values pass through staging module variables, its site is one
//! of the module's function sites, which every kernel that calls such units
//! lists after its own, and the runtime keeps the stack below the innermost
//! live frame with the activation until it runs again
//! ([`celox_design::ProcessSlots::call_top`]). Only processes can suspend,
//! so a call from anything else to such a unit stays inlined. A delay or a
//! stop keeps the frames past the time slot: the collector finds the
//! handles in the saved frames through the module's resume table
//! ([`celox_design::CallResume`]), and checkpoints keep the saved stack.
//!
//! A `fork` in a unit is such a suspension too, whose branches are resume
//! points of the unit (see `Ff::fork_in_unit`): the runtime starts each
//! child as an activation of the calling kernel in a copy of the parent's
//! frames, the unit resuming at the child's entry, so the children outlive
//! the call. A variable that the children and the parent share and write
//! lives in a box, a heap object whose handle the copies share (see
//! `calls::SharedBox`). `wait fork` and `disable fork` act on the children
//! of the process the unit runs in.
//!
//! An event control or a level wait in a unit suspends it at one of the
//! module's function wait sites, which every kernel that can suspend
//! inside a call lists after its own; the kernel's call turns the unit's
//! site number into the process's. The unit evaluates a level wait's
//! condition again at each resume, and an event control that reads its
//! frame keeps samples there that it compares at each resume (see
//! `Ff::unit_wait_event`).
//!
//! A `ref` formal is a place (see [`Place`]): the call passes its static
//! part, which selects the function, so one subroutine has a function for
//! each binding its calls make (a variable the unit names itself, the field
//! of a property, the array and shape of an element, or a copy), and its
//! dynamic part through the frame: the handle of a property, the positions
//! of an element, or the value of an automatic of the calling unit, which
//! comes back whenever the call returns. A recursive call that passes its
//! own `ref` formal on binds the same place, and so enters the same
//! function.
//!
//! The frame of a function holds its own frame base and call depth first
//! (the caller passes both: a unit run by the runtime starts with an empty
//! stack at offset zero and depth zero), then the resume slots of a
//! resumable unit, then the state of the calling
//! process that its body and the bodies it reaches use ([`ProcessState`]),
//! then the dynamic parts of the places of its `ref` formals, then its
//! automatic formals, its result and its locals, then the
//! temporaries the lowering of its body created, which a recursive
//! activation must not share. The process state is the RNG state and seed
//! of `$urandom` and the seed of `$random`: the caller passes its own and
//! takes the states back whenever the call returns, also when it suspends,
//! and passes them again when it continues the call, which is drawing on
//! them in place. A unit thereby serves every process. Static storage
//! that the analyzer found observable (`persistent_locals`, IEEE 1800-2023
//! 13.4.2) stays a module variable shared by every activation.
//!
//! A unit is lowered with the variables in place: it reads and writes them
//! in the stable region, as a process kernel does, and the addresses of the
//! frame variables move to the FRAME region afterwards. A static
//! subroutine's formals, result and static locals are module variables
//! (IEEE 1800-2023 13.4.2), which the caller writes before the call and
//! reads after it, as an inlined body would. A call from an
//! `always_ff` process, which keeps its writes in the working region, goes
//! out of line only if the function writes no module variable and reads
//! none the process writes, so both see the same values.

mod policy;

use super::procedural::{
    ProcModule, SystemTaskKind, collect_calls, default_calls, expr_idents, stmt_calls,
    system_task_kind,
};
use super::*;
use celox_sir::{FrameLayout, FrameSlot, FunctionUnit, RegisterId};
use std::collections::BTreeMap;

type Addr = RegionedVarAddr;

/// Why a subroutine stays inlined, in the order the coverage report counts
/// them: the first that applies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Reason {
    /// It has a `ref` formal of unpacked structure type, or the call binds
    /// two `ref` formals to one automatic of the calling unit, which the
    /// call copies (see [`Place::Frame`]).
    RefFormal,
    /// It, or a subroutine it calls, has an `@*`, a deferred action, a
    /// delayed nonblocking assignment or a concurrent assertion region.
    MaySuspend,
    /// It, or a subroutine it calls, makes a host request a unit cannot
    /// make: `$exit`, monitors, strobes, VCD tasks, or a DPI-C import; or a caller that cannot suspend calls a
    /// unit that can suspend.
    HostRequest,
    /// Anything else, described.
    Other(String),
}

impl Reason {
    fn category(&self) -> &'static str {
        match self {
            Reason::RefFormal => "ref_formal",
            Reason::MaySuspend => "may_suspend",
            Reason::HostRequest => "host_request",
            Reason::Other(_) => "other",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::Other(detail) => write!(f, "other ({detail})"),
            reason => f.write_str(reason.category()),
        }
    }
}

/// The state of the calling process that a subroutine's body and the bodies
/// it reaches use, which a unit of it receives through its frame: the
/// caller passes its own and takes the RNG states back after the call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ProcessState {
    /// The process RNG of `$urandom`, `std::randomize`, object seeding and
    /// `shuffle` (IEEE 1800-2023 18.13, 18.14).
    pub random: bool,
    /// The seed of `$random` without a seed argument.
    pub dist_seed: bool,
    /// The simulation time, which `$time` and its kin read.
    pub time: bool,
    /// Whether the body suspends the calling process: for a host request,
    /// a delay, `$stop` or `$finish`. A unit of it is resumable (see
    /// [`Function::resume`]).
    pub suspends: bool,
    /// Whether it suspends the calling process past the moment, for a delay
    /// or `$stop`, so that other processes run before the call continues.
    /// A host request does not: the host serves it before anything else
    /// runs.
    pub yields: bool,
    /// Whether it forks (`fork`, `wait fork`, `disable fork`): a fork
    /// suspends with the branches as resume points of the unit, and each
    /// child continues the call in a copy of the frames (see
    /// [`celox_design::PROCESS_FORK_RESUME_SHIFT`]); the calling kernel
    /// keeps a process frame, as one that forks itself.
    pub forks: bool,
    /// Whether it makes a nonblocking assignment: a unit stages the value
    /// at one of the module's function NBA sites and suspends, as a kernel
    /// does at its own (see `FunctionSites::nba`).
    pub nbas: bool,
    /// Whether it waits, for an event control or a level wait: a unit
    /// suspends at one of the module's function wait sites, which every
    /// kernel that calls it lists after its own (see
    /// `FunctionSites::wait`).
    pub waits: bool,
    /// Whether it reads `e.triggered` (IEEE 1800-2023 15.5.3) or a process's
    /// `status()` (9.7), which a level wait's predicate reads as an
    /// observation of the event or process, so that a predicate's call of
    /// it stays inlined.
    pub observes_events: bool,
    /// Whether it issues the action of a deferred assertion, which waits in
    /// the queue of the process the unit runs in (IEEE 1800-2023 16.4.1):
    /// only a process of its own, a kernel's, runs such a unit (see
    /// `celox_design::DeferredSite::ProcessAction`).
    pub deferred: bool,
}

impl ProcessState {
    /// The state that a body using `self` or `other` uses.
    pub(super) fn union(self, other: Self) -> Self {
        Self {
            random: self.random || other.random,
            dist_seed: self.dist_seed || other.dist_seed,
            time: self.time || other.time,
            suspends: self.suspends || other.suspends,
            yields: self.yields || other.yields,
            forks: self.forks || other.forks,
            nbas: self.nbas || other.nbas,
            waits: self.waits || other.waits,
            observes_events: self.observes_events || other.observes_events,
            deferred: self.deferred || other.deferred,
        }
    }
}

/// What became of the calls of one subroutine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// At least one call went out of line.
    OutOfLine,
    /// Every call was inlined, the first for this reason.
    Inlined(Reason),
}

/// One slot of a frame: the variable whose storage it is.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    pub id: SourceVarId,
    /// The `var_id` of the slot's FRAME address, unique among the slots of
    /// the module: one variable may have a slot in several frames, such as
    /// a formal of a subroutine that is inlined into two functions.
    pub address: SourceVarId,
    pub width: usize,
    pub four_state: bool,
    /// What the slot holds: a heap handle, which the collector scans, or
    /// bits.
    pub kind: celox_design::ValueKind,
    /// The width of an element when it holds an unpacked array, which the
    /// unit accesses at positions computed at run time.
    pub element_width: Option<usize>,
}

/// What a `ref` formal of a call refers to (IEEE 1800-2023 13.5.2), as an
/// out-of-line call passes it: a static part, which selects the function
/// (see [`Place::key`]), and a dynamic part, the variables of the caller
/// whose values the call passes in the frame.
#[derive(Debug, Clone)]
pub(super) enum Place {
    /// A variable the unit names itself, as an inlined body renames the
    /// formal to it.
    Variable(SourceVarId),
    /// Property `field` of the object that the caller's variable `handle`
    /// holds, the object the call evaluated.
    Property {
        field: sv::ir::ObjectField,
        handle: String,
    },
    /// An element of an unpacked array whose positions the call fixed in
    /// the caller's variables `positions`; `element` names the `k`-th of
    /// them `#k`.
    Element {
        element: Box<sv::ir::LValue>,
        signed: bool,
        width: usize,
        total: usize,
        positions: Vec<String>,
    },
    /// An automatic variable of the calling unit, named `name` there, whose
    /// value the call copies into the formal's slot and back whenever the
    /// call returns: nothing else can name it while the call runs.
    Frame { id: SourceVarId, name: String },
}

impl Place {
    /// The static part of the place, which the unit is lowered for.
    pub fn key(&self) -> String {
        match self {
            Place::Variable(id) => format!("v{}", id.0),
            Place::Property { field, .. } => format!("p{field:?}"),
            Place::Element {
                element,
                signed,
                width,
                total,
                ..
            } => format!("e{element:?}/{signed}/{width}/{total}"),
            Place::Frame { .. } => "f".to_string(),
        }
    }
}

/// The key of the function of `name` for the places of its `ref` formals.
pub(super) fn function_key(name: &str, places: &[Place]) -> String {
    if places.is_empty() {
        return name.to_string();
    }
    let keys: Vec<String> = places.iter().map(Place::key).collect();
    format!("{name}({})", keys.join(","))
}

/// A subroutine lowered out of line, or being lowered.
#[derive(Debug, Clone)]
pub(super) struct Function {
    /// The subroutine's name with the places of its `ref` formals (see
    /// [`function_key`]): the functions of one subroutine differ in them.
    pub key: String,
    /// For each `ref` formal, in order, the slots of the dynamic part of its
    /// place: the handle of a property, the positions of an element, the
    /// copy of an automatic; none for a variable.
    pub places: Vec<Vec<usize>>,
    /// The frame's slots: the frame base, the call depth, the calling
    /// process's state, then the formals, result and locals, all known
    /// before the body is lowered, then the temporaries of the body.
    pub slots: Vec<Slot>,
    /// `None` while the body is being lowered.
    pub unit: Option<ExecutionUnit<Addr>>,
    /// The bytes of the frame, once the body is lowered.
    pub size: usize,
    /// The byte offset of each slot.
    pub offsets: Vec<usize>,
    /// The module variables the unit reads and writes itself.
    pub reads: HashSet<SourceVarId>,
    pub writes: HashSet<SourceVarId>,
    /// The functions the unit calls.
    pub callees: Vec<usize>,
    /// The registers of the unit that hold the frame size of a function
    /// that was still being lowered when the call was built (a recursive
    /// call), set once every size is known.
    pub size_patches: Vec<(RegisterId, usize)>,
    /// The slots of the calling process's state (see [`ProcessState`]):
    /// those of the RNG state and the process seed, and that of the seed
    /// of `$random`.
    pub random: Option<(usize, usize)>,
    pub dist_seed: Option<usize>,
    /// For a unit that can suspend the calling process (see
    /// [`ProcessState::suspends`]), the slot of its resume point, which a
    /// call passes as zero to start the body and leaves as it is to
    /// continue it. The three slots after it are outputs the unit leaves at
    /// each return: the status, zero once the body completed or the
    /// [`celox_design::ProcessStatus`] it suspends with; the delay word of
    /// the suspension; and the end of the innermost live frame.
    pub resume: Option<usize>,
    /// The calls at which the unit can be suspended inside the callee, once
    /// the body is lowered.
    pub calls: Vec<celox_design::CallResume>,
    /// The slot whose bit `p` says that a call omits argument `p`, and the
    /// positions of the formals whose defaults the unit evaluates then (IEEE
    /// 1800-2023 13.5.3), in its own frame, where the calls of a default are
    /// calls of the unit: defaults that call each other then recurse at run
    /// time, as any recursion, instead of expanding at the call forever.
    pub omitted: Option<(usize, Vec<usize>)>,
}

impl Function {
    /// The slot of variable `id`.
    pub fn slot(&self, id: SourceVarId) -> Option<usize> {
        self.slots.iter().position(|slot| slot.id == id)
    }
}

/// The out-of-line functions of a module.
#[derive(Debug, Clone, Default)]
pub(crate) struct OutOfLine {
    /// By the module-local `FunctionId` their calls name.
    pub(super) functions: Vec<Function>,
    pub(super) by_name: HashMap<String, usize>,
    /// Subroutines whose lowering as a unit failed, and why.
    pub(super) rejected: HashMap<String, Reason>,
    /// Why each subroutine asked about cannot go out of line wherever it is
    /// called, if it cannot.
    intrinsic: HashMap<String, Option<Reason>>,
    /// The process state each subroutine asked about uses.
    process_state: HashMap<String, ProcessState>,
    /// The least reason and the process state of the bodies each subroutine
    /// asked about reaches (see `policy::reached_bodies`).
    reached: HashMap<String, (Option<Reason>, ProcessState)>,
    /// The outcome of the calls of each called subroutine, for the report.
    outcomes: BTreeMap<String, Outcome>,
    /// How many function bodies are being lowered, nested.
    pub(super) lowering: usize,
    /// The number of frame slots of the module so far, which numbers their
    /// addresses.
    pub(super) slots: u32,
    /// The variables whose stores assembly instruments (see
    /// [`ProcModule::observe_storage`]): a combinational process does not
    /// call a function unit that stores into them.
    pub(super) observed: HashSet<SourceVarId>,
    /// Those of them whose instrumentation assembly makes in process and
    /// flip-flop units only, which a function unit must not store into.
    pub(super) excluded: HashSet<SourceVarId>,
    /// The module's names that hierarchical names of other modules reach.
    pub(super) foreign: HashSet<String>,
    /// The subroutines that may call themselves, through any chain of
    /// calls (see [`ProcModule::mark_recursive`]).
    pub(super) recursive: HashSet<String>,
    /// Whether every subroutine that can go out of line does, which a test
    /// switch asks for (`AnalysisOptions::force_out_of_line_calls`);
    /// otherwise only those of [`ProcModule::out_of_line_policy`] do.
    pub(super) forced: bool,
    /// The fully inlined size of each subroutine, in statements (see
    /// [`ProcModule::mark_recursive`]).
    inlined_sizes: HashMap<String, u64>,
}

/// The fully inlined size, in statements, above which a subroutine goes out
/// of line (see [`ProcModule::out_of_line_policy`]).
pub(super) const OUT_OF_LINE_STATEMENTS: u64 = 256;

impl OutOfLine {
    /// No functions yet, forced to take every subroutine out of line or
    /// not.
    pub(super) fn new(forced: bool) -> Self {
        Self {
            forced,
            ..Self::default()
        }
    }

    /// The fully inlined size of subroutine `name`, in statements.
    pub(super) fn inlined_size(&self, name: &str) -> Option<u64> {
        self.inlined_sizes.get(name).copied()
    }

    /// Note how a call of `name` was lowered: going out of line once is
    /// what counts, else the first reason.
    pub(super) fn record(&mut self, name: &str, outcome: Outcome) {
        match self.outcomes.get(name) {
            Some(Outcome::OutOfLine) => {}
            Some(Outcome::Inlined(_)) if outcome != Outcome::OutOfLine => {}
            _ => {
                self.outcomes.insert(name.to_string(), outcome);
            }
        }
    }

    /// Forget the functions from `len` on, which a discarded lowering made.
    pub(super) fn truncate(&mut self, len: usize) {
        for function in self.functions.drain(len.min(self.functions.len())..) {
            self.by_name.remove(&function.key);
        }
    }

    /// The number of functions, which [`Self::truncate`] restores.
    pub(super) fn len(&self) -> usize {
        self.functions.len()
    }

    /// Fill in the frame sizes that recursive calls left open, once no body
    /// is being lowered and so every size is known.
    pub(super) fn patch_sizes(&mut self) {
        if self.lowering != 0 {
            return;
        }
        let sizes: Vec<usize> = self
            .functions
            .iter()
            .map(|function| function.size)
            .collect();
        for function in &mut self.functions {
            let patches = std::mem::take(&mut function.size_patches);
            let Some(unit) = function.unit.as_mut() else {
                continue;
            };
            for (register, callee) in patches {
                for block in unit.blocks.values_mut() {
                    for instruction in &mut block.instructions {
                        if let SIRInstruction::Imm(dst, value) = instruction
                            && *dst == register
                        {
                            *value = SIRValue::new(sizes[callee] as u64);
                        }
                    }
                }
            }
        }
    }

    /// The module variables function `index` may read and write, its calls
    /// included; `None` if one of the functions it reaches is still being
    /// lowered.
    pub(super) fn transitive(
        &self,
        index: usize,
    ) -> Option<(HashSet<SourceVarId>, HashSet<SourceVarId>)> {
        let mut reads = HashSet::default();
        let mut writes = HashSet::default();
        let mut seen = HashSet::default();
        let mut work = vec![index];
        while let Some(index) = work.pop() {
            if !seen.insert(index) {
                continue;
            }
            let function = &self.functions[index];
            function.unit.as_ref()?;
            reads.extend(function.reads.iter().copied());
            writes.extend(function.writes.iter().copied());
            work.extend(function.callees.iter().copied());
        }
        Some((reads, writes))
    }

    /// The function units, by the indices their calls name.
    pub(super) fn units(&self) -> Vec<FunctionUnit<Addr>> {
        self.functions
            .iter()
            .map(|function| FunctionUnit {
                unit: function
                    .unit
                    .clone()
                    .expect("every function body is lowered once the module is"),
                frame: FrameLayout {
                    size: function.size,
                    slots: function
                        .slots
                        .iter()
                        .zip(&function.offsets)
                        .map(|(slot, &offset)| FrameSlot {
                            addr: frame_addr(slot.address),
                            offset,
                            width: slot.width,
                            four_state: slot.four_state,
                            handle: slot.kind != celox_design::ValueKind::Bits,
                            element_width: slot.element_width,
                        })
                        .collect(),
                },
                effects: Default::default(),
            })
            .collect()
    }

    /// What the runtime reads in a saved frame of each function, in the
    /// order of [`Self::units`] (see [`celox_design::FunctionFrame`]); the
    /// callees are module-local function indices.
    pub(super) fn frames(&self) -> Vec<celox_design::FunctionFrame> {
        self.functions
            .iter()
            .map(|function| celox_design::FunctionFrame {
                size: function.size,
                resume: function.resume.map(|slot| {
                    (
                        function.offsets[slot],
                        celox_frontend_core::process::PROCESS_RESUME_WIDTH,
                    )
                }),
                calls: function.calls.clone(),
                // Each element of an unpacked array of handles is a handle
                // slot, as in state memory.
                handles: function
                    .slots
                    .iter()
                    .zip(&function.offsets)
                    .filter(|(slot, _)| slot.kind != celox_design::ValueKind::Bits)
                    .flat_map(|(slot, &offset)| {
                        (0..slot.width / celox_design::HANDLE_WIDTH).map(move |index| {
                            (offset + index * celox_heap::HANDLE_BYTES, slot.kind)
                        })
                    })
                    .collect(),
            })
            .collect()
    }

    /// The coverage lines of the report: how many called subroutines went
    /// out of line and how many stayed inlined, by reason, then one line
    /// per subroutine.
    pub(super) fn report(&self) -> Vec<String> {
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for category in ["ref_formal", "may_suspend", "host_request", "other"] {
            counts.insert(category, 0);
        }
        let mut out_of_line = 0;
        for outcome in self.outcomes.values() {
            match outcome {
                Outcome::OutOfLine => out_of_line += 1,
                Outcome::Inlined(reason) => *counts.entry(reason.category()).or_default() += 1,
            }
        }
        let mut lines = vec![format!(
            "  out_of_line_coverage out_of_line {out_of_line} inlined {} ref_formal {} \
             may_suspend {} host_request {} other {}",
            self.outcomes.len() - out_of_line,
            counts["ref_formal"],
            counts["may_suspend"],
            counts["host_request"],
            counts["other"],
        )];
        for (name, outcome) in &self.outcomes {
            lines.push(match outcome {
                Outcome::OutOfLine => format!("  out_of_line : {name}"),
                Outcome::Inlined(reason) => format!("  inlined {reason} : {name}"),
            });
        }
        lines
    }
}

/// The fully inlined size of each subroutine `node`, whose own statements
/// are `own[node]` and whose calls, one entry per call, are `sites[node]`:
/// its own statements and, at each of its calls, the size of the callee,
/// unless the callee is a member of a recursive component (`members`) and
/// so goes out of line. The callees that count form no cycle, so each size
/// is the sum over a tree of calls, taken in post-order on a stack of its
/// own: a chain of calls may be deeper than the host stack.
fn fully_inlined_sizes(own: &[u64], sites: &[Vec<usize>], members: &HashSet<usize>) -> Vec<u64> {
    let mut sizes: Vec<Option<u64>> = vec![None; own.len()];
    for start in 0..own.len() {
        let mut stack = vec![(start, 0)];
        while let Some((node, next)) = stack.pop() {
            if sizes[node].is_some() {
                continue;
            }
            if let Some(&callee) = sites[node].get(next) {
                stack.push((node, next + 1));
                if !members.contains(&callee) && sizes[callee].is_none() {
                    stack.push((callee, 0));
                }
                continue;
            }
            let total = sites[node]
                .iter()
                .filter(|callee| !members.contains(callee))
                .fold(own[node], |total, &callee| {
                    total.saturating_add(sizes[callee].expect("a callee is sized first"))
                });
            sizes[node] = Some(total);
        }
    }
    sizes.into_iter().map(|size| size.unwrap_or(0)).collect()
}

/// The automatic storage of `subroutine` (IEEE 1800-2023 13.4.2): its
/// automatic locals and, when it is automatic, its formals (but `ref` ones,
/// which have no storage of their own) and result; less any whose value the
/// analyzer found observable from one call to the next. The storage of a
/// static subroutine is static unless declared `automatic`.
pub(super) fn automatic_names(subroutine: &sv::ir::Subroutine) -> impl Iterator<Item = &String> {
    let own = subroutine.automatic.then_some(subroutine).into_iter();
    own.clone()
        .flat_map(|subroutine| {
            subroutine
                .params
                .iter()
                .filter(|param| !param.direction.is_reference())
                .map(|param| &param.name)
        })
        .chain(own.flat_map(|subroutine| &subroutine.return_var))
        .chain(&subroutine.automatic_locals)
        .filter(|name| !subroutine.persistent_locals.contains(name))
}

/// The static storage of `subroutine` (see [`automatic_names`]): what it
/// declares, formals and result included, that is not automatic.
fn static_names(subroutine: &sv::ir::Subroutine) -> Vec<&String> {
    let automatic: HashSet<&String> = automatic_names(subroutine).collect();
    let mut own: Vec<&String> = subroutine
        .params
        .iter()
        .map(|param| &param.name)
        .chain(&subroutine.return_var)
        .chain(&subroutine.persistent_locals)
        .collect();
    for stmt in &subroutine.body {
        stmt.walk(&mut |stmt| {
            if let sv::ir::Stmt::Local { name, .. } = stmt {
                own.push(name);
            }
        });
    }
    own.retain(|name| !automatic.contains(name));
    own
}

/// The storage of a module's subroutines by variable: for each variable,
/// how many of the names of their automatic storage ([`automatic_names`])
/// and of their static storage ([`static_names`]) denote it. The names are
/// fixed with the subroutines; what they denote changes as calls are
/// inlined, which [`ProcModule::bind_name`] follows, so a query costs a
/// lookup rather than a walk of every subroutine.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StorageIndex {
    automatic_names: HashSet<String>,
    static_names: HashSet<String>,
    pub(super) automatic: HashMap<SourceVarId, usize>,
    pub(super) persistent: HashMap<SourceVarId, usize>,
}

impl StorageIndex {
    pub(super) fn new(
        subroutines: &HashMap<String, sv::ir::Subroutine>,
        name_to_id: &HashMap<String, SourceVarId>,
    ) -> Self {
        let automatic_names: HashSet<String> = subroutines
            .values()
            .flat_map(automatic_names)
            .cloned()
            .collect();
        let static_names: HashSet<String> = subroutines
            .values()
            .flat_map(static_names)
            .cloned()
            .collect();
        let count = |names: &HashSet<String>| {
            let mut counts = HashMap::<SourceVarId, usize>::default();
            for id in names.iter().filter_map(|name| name_to_id.get(name)) {
                *counts.entry(*id).or_default() += 1;
            }
            counts
        };
        Self {
            automatic: count(&automatic_names),
            persistent: count(&static_names),
            automatic_names,
            static_names,
        }
    }

    /// Follow name `name` from denoting `old` to denoting `new`.
    pub(super) fn rebind(
        &mut self,
        name: &str,
        old: Option<SourceVarId>,
        new: Option<SourceVarId>,
    ) {
        for (names, counts) in [
            (&self.automatic_names, &mut self.automatic),
            (&self.static_names, &mut self.persistent),
        ] {
            if !names.contains(name) {
                continue;
            }
            if let Some(old) = old
                && let Some(count) = counts.get_mut(&old)
            {
                *count -= 1;
                if *count == 0 {
                    counts.remove(&old);
                }
            }
            if let Some(new) = new {
                *counts.entry(new).or_default() += 1;
            }
        }
    }

    /// Whether the index is what [`Self::new`] builds now.
    pub(super) fn is_current(
        &self,
        subroutines: &HashMap<String, sv::ir::Subroutine>,
        name_to_id: &HashMap<String, SourceVarId>,
    ) -> bool {
        *self == Self::new(subroutines, name_to_id)
    }
}

/// The functions of `module` a unit of it calls, directly or through other
/// functions, in `module.functions` with the calls renumbered: a function
/// lowered for a call that then stayed inlined (from an `always_ff`
/// process, say) is dropped.
pub(super) fn attach_functions(
    module: &mut SimModule,
    mut functions: Vec<FunctionUnit<Addr>>,
    mut frames: Vec<celox_design::FunctionFrame>,
) {
    fn calls<A>(unit: &ExecutionUnit<A>, called: &mut Vec<usize>) {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                if let SIRInstruction::Call { function, .. } = instruction {
                    called.push(function.0 as usize);
                }
            }
        }
    }
    fn renumber<A>(unit: &mut ExecutionUnit<A>, kept: &[Option<u32>]) {
        for block in unit.blocks.values_mut() {
            for instruction in &mut block.instructions {
                if let SIRInstruction::Call { function, .. } = instruction {
                    function.0 = kept[function.0 as usize].expect("a called function is kept");
                }
            }
        }
    }
    // A store of a function unit repeats at every call, so each signal a
    // unit stores gets counters, as one a kernel may store twice between
    // two checks does: assembly counts the stores of a signal a wait or an
    // event domain watches, in the units as in the kernels.
    let counted: HashSet<SourceVarId> = module
        .counter_slots
        .iter()
        .map(|slots| slots.signal)
        .collect();
    let stored: Vec<SourceVarId> = functions
        .iter()
        .flat_map(|function| function.unit.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter_map(celox_frontend_core::watch::write_destination)
        .filter(|address| address.region == STABLE_REGION && !counted.contains(&address.var_id))
        .map(|address| address.var_id)
        .collect();
    let slots = celox_frontend_core::watch::declare_counter_slots(&mut module.variables, stored);
    module.counter_slots.extend(slots);
    let mut work = Vec::new();
    module_units(module).for_each(|unit| calls(unit, &mut work));
    for path in &module.comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            calls(unit, &mut work);
        }
    }
    let mut reached = vec![false; functions.len()];
    while let Some(function) = work.pop() {
        if !std::mem::replace(&mut reached[function], true) {
            calls(&functions[function].unit, &mut work);
        }
    }
    if reached.iter().all(|&reached| reached) {
        module.functions = functions;
        module.function_frames = frames;
        return;
    }
    let mut next = 0;
    let kept: Vec<Option<u32>> = reached
        .iter()
        .map(|&reached| {
            reached.then(|| {
                next += 1;
                next - 1
            })
        })
        .collect();
    let mut position = 0;
    functions.retain(|_| {
        position += 1;
        reached[position - 1]
    });
    let mut position = 0;
    frames.retain(|_| {
        position += 1;
        reached[position - 1]
    });
    for function in &mut functions {
        renumber(&mut function.unit, &kept);
    }
    // A call that can suspend inside its callee calls a kept function.
    let resumes = frames.iter_mut().flat_map(|frame| &mut frame.calls).chain(
        module
            .processes
            .iter_mut()
            .flat_map(|process| &mut process.call_resumes),
    );
    for call in resumes {
        call.function = kept[call.function as usize].expect("a called function is kept");
    }
    for_each_unit_mut(module, &kept);
    module.functions = functions;
    module.function_frames = frames;

    fn for_each_unit_mut(module: &mut SimModule, kept: &[Option<u32>]) {
        let units = module
            .eval_only_ff_blocks
            .values_mut()
            .chain(module.apply_ff_blocks.values_mut())
            .chain(module.eval_apply_ff_blocks.values_mut())
            .chain(
                module
                    .split_commit_ff_blocks
                    .values_mut()
                    .flat_map(|split| [&mut split.active, &mut split.nba]),
            )
            .chain(
                module
                    .parallel_ff_parts
                    .values_mut()
                    .flatten()
                    .flat_map(|part| [&mut part.evaluate, &mut part.apply]),
            )
            .chain(
                module
                    .processes
                    .iter_mut()
                    .map(|process| &mut process.kernel),
            );
        for unit in units {
            renumber(unit, kept);
        }
        for path in &mut module.comb_blocks {
            if let LogicPathTarget::Process { unit, .. } = &mut path.target {
                renumber(unit, kept);
            }
        }
    }
}

/// The units of `module` whose addresses carry a region: all but its
/// combinational process units.
fn module_units(module: &SimModule) -> impl Iterator<Item = &ExecutionUnit<Addr>> {
    module
        .eval_only_ff_blocks
        .values()
        .chain(module.apply_ff_blocks.values())
        .chain(module.eval_apply_ff_blocks.values())
        .chain(
            module
                .split_commit_ff_blocks
                .values()
                .flat_map(|split| [&split.active, &split.nba]),
        )
        .chain(
            module
                .parallel_ff_parts
                .values()
                .flatten()
                .flat_map(|part| [&part.evaluate, &part.apply]),
        )
        .chain(module.processes.iter().map(|process| &process.kernel))
}

/// The FRAME address of the slot of variable `id`.
pub(super) fn frame_addr(id: SourceVarId) -> Addr {
    RegionedVarAddrBase {
        region: celox_design::FRAME_REGION,
        var_id: id,
    }
}

/// The byte offsets of `slots` and the size of their frame. Each slot
/// starts at a multiple of eight bytes, so wide accesses stay aligned.
pub(super) fn layout(slots: &[Slot]) -> (Vec<usize>, usize) {
    let mut offsets = Vec::with_capacity(slots.len());
    let mut end = 0;
    for slot in slots {
        offsets.push(end);
        let planes = if slot.four_state { 2 } else { 1 };
        end += (slot.width.div_ceil(8) * planes).next_multiple_of(8);
    }
    (offsets, end.max(8))
}

#[cfg(test)]
mod tests;
