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
    /// does at its own (see `ProcModule::function_nba_sites`).
    pub nbas: bool,
    /// Whether it waits, for an event control or a level wait: a unit
    /// suspends at one of the module's function wait sites, which every
    /// kernel that calls it lists after its own (see
    /// `ProcModule::function_wait_sites`).
    pub waits: bool,
    /// Whether it reads `e.triggered` (IEEE 1800-2023 15.5.3) or a process's
    /// `status()` (9.7), which a level wait's predicate reads as an
    /// observation of the event or process, so that a predicate's call of
    /// it stays inlined.
    pub observes_events: bool,
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
                handles: function
                    .slots
                    .iter()
                    .zip(&function.offsets)
                    .filter(|(slot, _)| slot.kind != celox_design::ValueKind::Bits)
                    .map(|(slot, &offset)| (offset, slot.kind))
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

impl ProcModule<'_> {
    /// The out-of-line functions, when calls go out of line.
    pub(super) fn out_of_line(&mut self) -> Option<&mut OutOfLine> {
        self.calls.as_deref_mut()?.out_of_line.as_mut()
    }

    /// The automatic storage of the module's subroutines (see
    /// [`automatic_names`]). Such a variable is a temporary of each inline
    /// expansion, which a function unit keeps in its frame.
    /// The declared variable that `id` stands for: `id` itself, or the
    /// variable an inlined activation copied it from, through every
    /// activation of a recursion. A function's frame has slots for the
    /// declared variables, whatever activation a call is made in.
    pub(super) fn declared(&self, mut id: SourceVarId) -> SourceVarId {
        while let Some(&(_, original, _)) = self
            .activation_copies
            .iter()
            .rev()
            .find(|&&(_, _, copy)| copy == id)
        {
            id = original;
        }
        id
    }

    /// Point the names that inlined activations copied at the declared
    /// variables again, and return the names to give back to
    /// [`Self::restore_names`]. A function unit is lowered once and serves
    /// every call, so it must not capture the copies of the activation its
    /// first call happens to be lowered in.
    pub(super) fn declare_names(&mut self) -> Vec<(String, SourceVarId)> {
        let current: Vec<(String, SourceVarId)> = self
            .activation_copies
            .iter()
            .map(|(name, _, _)| (name.clone(), self.name_to_id[name]))
            .collect();
        for (name, original, _) in self.activation_copies.iter().rev() {
            self.name_to_id.insert(name.clone(), *original);
        }
        current
    }

    pub(super) fn restore_names(&mut self, names: Vec<(String, SourceVarId)>) {
        for (name, id) in names {
            self.name_to_id.insert(name, id);
        }
    }

    pub(super) fn automatic_storage(&self) -> HashSet<SourceVarId> {
        self.subroutines
            .values()
            .flat_map(automatic_names)
            .filter_map(|name| self.id(name))
            .collect()
    }

    /// The static storage of the module's subroutines: what they declare,
    /// formals and results included, that is not automatic. It is shared by
    /// every call, inlined or not, recursive ones included.
    pub(super) fn static_storage(&self) -> HashSet<SourceVarId> {
        self.subroutines
            .values()
            .flat_map(static_names)
            .filter_map(|name| self.id(name))
            .collect()
    }

    /// Whether assembly may instrument the stores into `id` (see
    /// [`Self::observe_storage`]).
    pub(super) fn is_observed(&self, id: SourceVarId) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.observed.contains(&id))
    }

    /// Whether assembly instruments the stores into `id` in ways only the
    /// units of processes and flip-flops carry (see
    /// [`Self::observe_storage`]), so that a function unit must not make
    /// them.
    pub(super) fn is_excluded(&self, id: SourceVarId) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.excluded.contains(&id))
    }

    /// Record, before `module`'s processes are lowered, the variables whose
    /// stores assembly may instrument (`SimModule::instrumented_storage`),
    /// as far as a function unit can store them.
    ///
    /// A function unit stores no hidden module state but the static storage
    /// of subroutines. Assembly instruments the stores into what a wait or an
    /// event control watches, what combinational logic is sensitive to, and
    /// the targets of nonblocking assignments. Each construct is walked by
    /// the walk its lowering derives that instrumentation from, so the two
    /// cannot disagree: a level wait by [`super::ff::level_reads`],
    /// combinational logic, `@*` and the expressions of event controls
    /// (whose values are continuous assignments of the module) by
    /// [`Self::implicit_reads`], an event control on an activation-local
    /// expression by `level_reads` too. Added are the targets of
    /// nonblocking assignments and `foreign`, the names that hierarchical
    /// names of other modules reach. The set may be more than assembly
    /// instruments, which only keeps a subroutine inlined that could have
    /// gone out of line.
    ///
    /// Assembly instruments a function unit's stores as a process kernel's
    /// for the counters of waits and event controls, which are kept for the
    /// stored variable, whatever process stores it. Its other
    /// instrumentation is for process and flip-flop units only: the
    /// triggers of flip-flop events, the staging of nonblocking
    /// assignments, the counting of what flip-flops write, and the
    /// captures of combinational observers in combinational processes. So a
    /// function unit stores into none of the `excluded` storage: the events
    /// and the writes of flip-flops, what instance ports connect (an
    /// event of the instance, as far as this module can tell), the targets
    /// of nonblocking assignments and `foreign`; and a combinational process
    /// calls no function unit that stores into `observed` storage. Assembly
    /// rejects a function unit that stores into storage it cannot
    /// instrument all the same.
    pub(super) fn observe_storage(&mut self, module: &sv::ir::Module, foreign: &HashSet<String>) {
        if self.out_of_line().is_none() {
            return;
        }
        let assign = |lhs: &sv::ir::LValue, rhs: &sv::ir::Expr| sv::ir::Stmt::Assign {
            lhs: lhs.clone(),
            rhs: rhs.clone(),
            nonblocking: false,
        };
        let mut observers: Vec<sv::ir::Stmt> = Vec::new();
        // What a function unit must not store into: the events of
        // flip-flops, whose stores assembly gives their triggers, what
        // flip-flops write, what instance ports connect (a clock of the
        // instance, as far as this module can tell), and below the targets
        // of nonblocking assignments and the names of other modules.
        let mut hard: Vec<sv::ir::Stmt> = Vec::new();
        let mut hard_names: Vec<String> = Vec::new();
        for process in module.ff_processes() {
            hard_names.extend(
                process
                    .events()
                    .iter()
                    .map(|event| event.signal().to_string()),
            );
            let mut written = HashSet::default();
            super::procedural::written_names(process.body(), &mut written);
            hard_names.extend(written);
        }
        for assignment in module.assignments() {
            observers.push(assign(assignment.lhs_value(), assignment.rhs()));
        }
        for process in module.comb_processes() {
            for assignment in process.assignments() {
                observers.push(assign(assignment.lhs_value(), assignment.rhs()));
            }
            observers.extend(process.body().iter().cloned());
            observers.extend(process.sensitivity_body().iter().cloned());
        }
        for instance in module.instances() {
            for connection in instance.port_connections() {
                if let Some(actual) = connection.actual_expr() {
                    observers.push(sv::ir::Stmt::Eval(actual.clone()));
                    hard.push(sv::ir::Stmt::Eval(actual.clone()));
                }
            }
        }
        let mut levels: Vec<&sv::ir::Expr> = Vec::new();
        let mut targets: Vec<&str> = Vec::new();
        let bodies =
            module
                .initial_processes()
                .iter()
                .map(|process| process.body())
                .chain(module.ff_processes().iter().map(|process| process.body()))
                .chain(module.comb_processes().iter().map(|process| process.body()))
                .chain(module.subroutines().iter().flat_map(|subroutine| {
                    [&subroutine.body[..], &subroutine.sensitivity_body[..]]
                }));
        for body in bodies {
            for stmt in body {
                stmt.walk(&mut |stmt| match stmt {
                    sv::ir::Stmt::Wait(condition) => levels.push(condition),
                    sv::ir::Stmt::WaitEvent(items) => {
                        for item in items {
                            for expr in std::iter::once(&item.expr).chain(&item.iff) {
                                observers.push(sv::ir::Stmt::Eval(expr.clone()));
                                levels.push(expr);
                            }
                        }
                    }
                    // `walk` does not visit the statements whose reads an
                    // `@*` covers.
                    sv::ir::Stmt::WaitImplicit { reads, .. } => {
                        observers.extend(reads.iter().cloned());
                    }
                    sv::ir::Stmt::Assign {
                        lhs,
                        nonblocking: true,
                        ..
                    } => targets.push(lhs.name()),
                    sv::ir::Stmt::AssignConcat {
                        parts,
                        nonblocking: true,
                        ..
                    } => targets.extend(parts.iter().map(sv::ir::LValue::name)),
                    _ => {}
                });
            }
        }
        let mut observed: HashSet<SourceVarId> = self
            .implicit_reads(&observers, true)
            .into_iter()
            .map(|atom| atom.id)
            .collect();
        let levels: Vec<String> = levels
            .into_iter()
            .flat_map(|expr| super::ff::level_reads(self, expr))
            .collect();
        let mut excluded: HashSet<SourceVarId> = self
            .implicit_reads(&hard, true)
            .into_iter()
            .map(|atom| atom.id)
            .collect();
        excluded.extend(
            targets
                .iter()
                .copied()
                .chain(foreign.iter().map(String::as_str))
                .chain(hard_names.iter().map(String::as_str))
                .filter_map(|name| self.id(name)),
        );
        observed.extend(
            targets
                .into_iter()
                .chain(levels.iter().map(String::as_str))
                .chain(foreign.iter().map(String::as_str))
                .filter_map(|name| self.id(name)),
        );
        observed.extend(excluded.iter().copied());
        if let Some(functions) = self.out_of_line() {
            functions.observed = observed;
            functions.excluded = excluded;
        }
    }

    /// Record, before the module's processes are lowered, the subroutines
    /// on a cycle of the module's call graph: those whose bodies, the
    /// argument defaults of their calls included, may call themselves
    /// again (see [`ProcModule::single_activation`]).
    pub(super) fn mark_recursive(&mut self, module: &sv::ir::Module) {
        if self.out_of_line().is_none() {
            return;
        }
        let mut names: Vec<&String> = self.subroutines.keys().collect();
        names.sort_unstable();
        let index: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(position, name)| (name.as_str(), position))
            .collect();
        // The call graph over calls: a node for each subroutine with each
        // pattern of omitted arguments it is called with, one for each
        // distinct pair however many call paths reach it. The node with
        // nothing omitted is the body; a node omitting arguments is a call
        // that runs the body and the defaults of those arguments (IEEE
        // 1800-2023 13.5.3). Each node's sites are the nodes it calls, once
        // per call, which size it; recursion is a cycle of this graph.
        let mut nodes: HashMap<(usize, Vec<bool>), usize> = HashMap::default();
        let mut keys: Vec<(usize, Vec<bool>)> = Vec::new();
        let mut node = |key: (usize, Vec<bool>), keys: &mut Vec<_>| -> usize {
            *nodes.entry(key.clone()).or_insert_with(|| {
                keys.push(key);
                keys.len() - 1
            })
        };
        // The node a call makes: the omitted arguments that take a default.
        let site = |(callee, args): (sv::ir::Callee, Vec<Option<sv::ir::Expr>>)| {
            let sv::ir::Callee::Subroutine(callee) = callee else {
                return None;
            };
            let &target = index.get(callee.as_str())?;
            let mut omitted: Vec<bool> = self.subroutines[names[target]]
                .params
                .iter()
                .enumerate()
                .map(|(position, param)| {
                    param.default.is_some() && matches!(args.get(position), None | Some(None))
                })
                .collect();
            if !omitted.contains(&true) {
                omitted.clear();
            }
            Some((target, omitted))
        };
        for target in 0..names.len() {
            node((target, Vec::new()), &mut keys);
        }
        // The calls of the module's processes, continuous assignments and
        // port connections, which reach the nodes of their omitted arguments.
        let mut roots = Vec::new();
        let bodies = module
            .initial_processes()
            .iter()
            .map(|process| process.body())
            .chain(module.ff_processes().iter().map(|process| process.body()))
            .chain(
                module
                    .comb_processes()
                    .iter()
                    .flat_map(|process| [process.body(), process.sensitivity_body()]),
            );
        for body in bodies {
            for stmt in body {
                stmt.walk(&mut |stmt| stmt_calls(stmt, &mut roots));
            }
        }
        for assignment in module.assignments().iter().chain(
            module
                .comb_processes()
                .iter()
                .flat_map(|process| process.assignments()),
        ) {
            collect_calls(assignment.rhs(), &mut roots);
        }
        for instance in module.instances() {
            for connection in instance.port_connections() {
                if let Some(actual) = connection.actual_expr() {
                    collect_calls(actual, &mut roots);
                }
            }
        }
        for key in roots.into_iter().filter_map(site) {
            node(key, &mut keys);
        }
        // Each node's own statements and sites, in the order nodes appear;
        // a node's sites may add nodes, sized in turn.
        let mut own: Vec<u64> = Vec::new();
        let mut sites: Vec<Vec<usize>> = Vec::new();
        while own.len() < keys.len() {
            let (target, omitted) = keys[own.len()].clone();
            let subroutine = &self.subroutines[names[target]];
            let mut calls = Vec::new();
            let mut statements = 0u64;
            let mut own_sites = Vec::new();
            if omitted.is_empty() {
                for stmt in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
                    stmt.walk(&mut |stmt| {
                        statements += 1;
                        stmt_calls(stmt, &mut calls)
                    });
                }
            } else {
                own_sites.push(node((target, Vec::new()), &mut keys));
                for (param, _) in subroutine
                    .params
                    .iter()
                    .zip(&omitted)
                    .filter(|(_, omitted)| **omitted)
                {
                    statements += 1;
                    collect_calls(
                        param.default.as_ref().expect("a defaulted formal"),
                        &mut calls,
                    );
                }
            }
            for key in calls.into_iter().filter_map(site) {
                own_sites.push(node(key, &mut keys));
            }
            own.push(statements);
            sites.push(own_sites);
        }
        let calls: Vec<Vec<usize>> = sites
            .iter()
            .map(|callees| {
                let mut callees = callees.clone();
                callees.sort_unstable();
                callees.dedup();
                callees
            })
            .collect();
        let recursive_nodes: HashSet<usize> = super::recursion::recursive_components(&calls)
            .into_iter()
            .flatten()
            .collect();
        let recursive_targets: HashSet<usize> =
            recursive_nodes.iter().map(|&node| keys[node].0).collect();
        // A call of a recursive subroutine inlines nothing, whatever it omits.
        let members: HashSet<usize> = (0..keys.len())
            .filter(|&node| recursive_targets.contains(&keys[node].0))
            .collect();
        let sizes = fully_inlined_sizes(&own, &sites, &members);
        // A subroutine's size is that of its largest call: one policy holds
        // for all its calls, and a call that inlines its defaults inlines
        // their calls too.
        let mut inlined_sizes: HashMap<String, u64> = HashMap::default();
        for (node, size) in sizes.into_iter().enumerate() {
            let entry = inlined_sizes
                .entry(names[keys[node].0].clone())
                .or_default();
            *entry = (*entry).max(size);
        }
        let recursive: HashSet<String> = recursive_targets
            .iter()
            .map(|&target| names[target].clone())
            .collect();
        if let Some(functions) = self.out_of_line() {
            functions.recursive = recursive;
            functions.inlined_sizes = inlined_sizes;
        }
    }

    /// Whether a call of subroutine `name` goes out of line whatever its
    /// arguments (see [`Self::out_of_line_policy`]), even constant ones,
    /// which would otherwise keep it inlined to fold: when `name` is on a
    /// cycle of the call graph, whose calls inlining cannot expand, or over
    /// the size threshold, whose body does not fold smaller.
    pub(super) fn out_of_line_whatever_arguments(&self, name: &str) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| {
                functions.recursive.contains(name)
                    || (!functions.forced
                        && functions
                            .inlined_size(name)
                            .is_some_and(|size| size > OUT_OF_LINE_STATEMENTS))
            })
    }

    /// Whether a call of subroutine `name` may go out of line: always when
    /// that is forced; otherwise when `name` is on a cycle of the call
    /// graph, which inlining cannot expand, or when its fully inlined size
    /// exceeds [`OUT_OF_LINE_STATEMENTS`]. Every other call is inlined, as
    /// without out-of-line calls.
    ///
    /// The size is measured on the IR before anything is lowered (see
    /// [`Self::mark_recursive`]), so the choice does not depend on the order
    /// of lowering.
    pub(super) fn out_of_line_policy(&self, name: &str) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| {
                functions.forced
                    || functions.recursive.contains(name)
                    || functions
                        .inlined_size(name)
                        .is_some_and(|size| size > OUT_OF_LINE_STATEMENTS)
            })
    }

    /// Whether at most one activation of the unit of subroutine `name` can
    /// be live at a time, so that its automatic unpacked arrays, which a
    /// frame does not hold, may stay module variables (each activation
    /// initializes them as its declarations say, as an inlined body does).
    ///
    /// A second activation could start only while one is live: by a call
    /// from inside it, which only a cycle of the call graph makes (the
    /// subroutine is not on one); or by another process while the first is
    /// suspended, which only a suspension past the moment lets run (the body
    /// has no delay and no `$stop`, nor do the bodies it reaches; a host
    /// request is served before anything else runs, and `$finish` ends the
    /// process). Processes run one at a time, so no other runs in between.
    pub(super) fn single_activation(&mut self, name: &str) -> bool {
        let recursive = self
            .out_of_line()
            .is_none_or(|functions| functions.recursive.contains(name));
        !recursive && !self.process_state(name).yields
    }

    /// Why subroutine `name` cannot go out of line wherever it is called,
    /// if it cannot: a property of its body and of the bodies it reaches.
    pub(super) fn out_of_line_reason(&mut self, name: &str) -> Option<Reason> {
        if let Some(known) = self
            .out_of_line()
            .and_then(|functions| functions.intrinsic.get(name))
        {
            return known.clone();
        }
        let (reason, state) = self.find_out_of_line_reason(name);
        if let Some(functions) = self.out_of_line() {
            functions.intrinsic.insert(name.to_string(), reason.clone());
            functions.process_state.insert(name.to_string(), state);
        }
        reason
    }

    /// The process state that subroutine `name` and the bodies it reaches
    /// use (see [`ProcessState`]).
    pub(super) fn process_state(&mut self, name: &str) -> ProcessState {
        self.out_of_line_reason(name);
        self.out_of_line()
            .and_then(|functions| functions.process_state.get(name).copied())
            .unwrap_or_default()
    }

    fn find_out_of_line_reason(&mut self, name: &str) -> (Option<Reason>, ProcessState) {
        let mut state = ProcessState::default();
        let mut found: Option<Reason> = None;
        let mut note = |reason: Reason| {
            if found.as_ref().is_none_or(|known| reason < *known) {
                found = Some(reason);
            }
        };
        // The formals are the function's interface: a callee whose formals
        // a unit cannot pass stays inlined, and its body is then part of
        // the caller's unit, which binds them as any inlined call does.
        if let Some(subroutine) = self.subroutine(name) {
            for param in &subroutine.params {
                // A `ref` formal of unpacked structure type binds each member
                // to a member of the actual, which a place does not express.
                if param.direction.is_reference() && !param.leaves.is_empty() {
                    note(Reason::RefFormal);
                }
                if !param.leaves.is_empty() {
                    note(Reason::Other("unpacked structure formal".to_string()));
                }
            }
        }
        // What the body does, and the bodies of the subroutines it calls,
        // which are inlined into the unit unless they go out of line
        // themselves, and can only if they qualify themselves.
        let mut pending = vec![name.to_string()];
        let mut visited = HashSet::default();
        while let Some(name) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some(subroutine) = self.subroutine(&name) else {
                note(Reason::Other(format!("no subroutine `{name}`")));
                continue;
            };
            let mut calls = Vec::new();
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    match stmt {
                        // A delay suspends the process with the unit's
                        // frames, which the runtime keeps with the
                        // activation until the delay ends.
                        sv::ir::Stmt::Delay { .. } => {
                            state.suspends = true;
                            state.yields = true;
                        }
                        // A fork creates processes that run the unit's
                        // code in copies of its frames, and the parent
                        // goes on; `wait fork` parks the process until
                        // they end.
                        sv::ir::Stmt::Fork { .. } | sv::ir::Stmt::WaitFork => {
                            state.suspends = true;
                            state.yields = true;
                            state.forks = true;
                        }
                        sv::ir::Stmt::DisableFork => {
                            state.suspends = true;
                            state.forks = true;
                        }
                        // A wait suspends the process with the unit's
                        // frames until the occurrence it waits for.
                        sv::ir::Stmt::WaitEvent(_) | sv::ir::Stmt::Wait(_) => {
                            state.suspends = true;
                            state.yields = true;
                            state.waits = true;
                        }
                        sv::ir::Stmt::WaitImplicit { .. }
                        | sv::ir::Stmt::DelayedNonblocking { .. }
                        | sv::ir::Stmt::DeferActive
                        | sv::ir::Stmt::AssertionRegion { .. } => note(Reason::MaySuspend),
                        // A nonblocking assignment hands the runtime its
                        // update by suspending; the process goes on at once.
                        sv::ir::Stmt::Assign {
                            nonblocking: true, ..
                        }
                        | sv::ir::Stmt::AssignConcat {
                            nonblocking: true, ..
                        } => {
                            state.suspends = true;
                            state.nbas = true;
                        }
                        // Prints, messages and Veryl's assertion calls are
                        // runtime events, which a unit raises as an inlined
                        // body does; a message carries the time. `$finish`
                        // and `$stop` end or suspend the process, a deferred
                        // action waits in the process's queue (IEEE 1800-2023
                        // 16.4.1), and the other tasks are host requests;
                        // `$exit` depends on the program it runs in.
                        sv::ir::Stmt::SystemTask {
                            task, assertion, ..
                        } => match system_task_kind(*task) {
                            _ if assertion.as_ref().is_some_and(|assertion| {
                                assertion.deferral != celox_design::AssertionDeferral::None
                            }) =>
                            {
                                note(Reason::HostRequest);
                            }
                            Some(SystemTaskKind::Print(..) | SystemTaskKind::Assertion(_)) => {}
                            Some(SystemTaskKind::Message(_) | SystemTaskKind::Fatal) => {
                                state.time = true;
                            }
                            // File output suspends for its host request.
                            None if matches!(
                                task,
                                sv::ir::SystemTask::Fdisplay(_)
                                    | sv::ir::SystemTask::Fwrite(_)
                                    | sv::ir::SystemTask::Fclose
                                    | sv::ir::SystemTask::Fflush
                            ) =>
                            {
                                state.suspends = true;
                            }
                            // `$finish` ends and `$stop` suspends the
                            // process, the unit's frames with it.
                            Some(SystemTaskKind::Control(RuntimeEventKind::Finish)) => {
                                state.suspends = true;
                            }
                            Some(SystemTaskKind::Control(RuntimeEventKind::Stop)) => {
                                state.suspends = true;
                                state.yields = true;
                            }
                            Some(SystemTaskKind::Control(_)) | None => note(Reason::HostRequest),
                        },
                        _ => {}
                    }
                    stmt_calls(stmt, &mut calls);
                });
            }
            let mut seen_defaults = super::procedural::SeenDefaults::default();
            while let Some((callee, args)) = calls.pop() {
                match callee {
                    sv::ir::Callee::System(function) => {
                        use sv::ir::SystemFunction;
                        match function {
                            // `process::self()`, the RNG state of a process,
                            // a new semaphore and the triggers of events are
                            // host requests a unit makes by suspending: none
                            // parks the process, which continues as soon as
                            // the host served it. The calling process holds
                            // its RNG state while the call is suspended: the
                            // frames that hold it give it back first. The
                            // other process methods act on the calling
                            // process, and some park it.
                            SystemFunction::Process(
                                celox_design::ProcessOp::SelfHandle
                                | celox_design::ProcessOp::Srandom
                                | celox_design::ProcessOp::GetRandstate
                                | celox_design::ProcessOp::SetRandstate
                                | celox_design::ProcessOp::SemaphoreNew
                                | celox_design::ProcessOp::Trigger
                                | celox_design::ProcessOp::TriggerNonblocking
                                | celox_design::ProcessOp::Kill
                                | celox_design::ProcessOp::Resume
                                | celox_design::ProcessOp::SemaphorePut
                                | celox_design::ProcessOp::SemaphoreTryGet,
                            ) => state.suspends = true,
                            // `suspend()` of the calling process parks it
                            // until another process resumes it, `await()`
                            // until another ends, an event control on named
                            // events or a wait for a trigger until one is
                            // triggered, and the blocking methods of
                            // semaphores and mailboxes until they can be
                            // served; the frames stay with the activation
                            // meanwhile, and a mailbox method retries when the
                            // process is woken.
                            SystemFunction::Process(
                                celox_design::ProcessOp::Suspend
                                | celox_design::ProcessOp::Await
                                | celox_design::ProcessOp::SemaphoreGet
                                | celox_design::ProcessOp::WaitEvent
                                | celox_design::ProcessOp::WaitTriggered
                                | celox_design::ProcessOp::MailboxPut
                                | celox_design::ProcessOp::MailboxGet
                                | celox_design::ProcessOp::MailboxPeek,
                            ) => {
                                state.suspends = true;
                                state.yields = true;
                            }
                            SystemFunction::Process(
                                celox_design::ProcessOp::MailboxNew
                                | celox_design::ProcessOp::MailboxNum
                                | celox_design::ProcessOp::MailboxTryPut
                                | celox_design::ProcessOp::MailboxTryGet
                                | celox_design::ProcessOp::MailboxTryPeek,
                            ) => state.suspends = true,
                            // A level wait whose predicate reads them is
                            // waiting on the event or process too, which
                            // only the predicate's own code can tell the
                            // host; a unit's code serves every caller.
                            SystemFunction::Process(
                                celox_design::ProcessOp::Triggered
                                | celox_design::ProcessOp::Status,
                            ) => {
                                state.suspends = true;
                                state.observes_events = true;
                            }
                            SystemFunction::Process(_) => note(Reason::HostRequest),
                            _ if function.is_host_request() => state.suspends = true,
                            SystemFunction::Urandom
                            | SystemFunction::UrandomRange
                            | SystemFunction::ScopeRandomize
                            | SystemFunction::ScopeRandomizeEnum => state.random = true,
                            SystemFunction::Random if args.is_empty() => state.dist_seed = true,
                            // They advance the seed in their argument.
                            SystemFunction::Random | SystemFunction::DistUniform => {}
                            SystemFunction::Time
                            | SystemFunction::Stime
                            | SystemFunction::Realtime => state.time = true,
                            // The sampled values are those of the calling
                            // process's site (IEEE 1800-2023 16.9.3).
                            _ if function.reads_simulation_state() => {
                                note(Reason::Other("reads simulation state".to_string()));
                            }
                            _ => {}
                        }
                    }
                    sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => {
                        state.suspends = true;
                    }
                    sv::ir::Callee::Object(op) => match &*op {
                        // Construction seeds the object from the process
                        // RNG (IEEE 1800-2023 18.14.3), and `shuffle` draws
                        // on it.
                        sv::ir::ObjectOp::New { .. }
                        | sv::ir::ObjectOp::Container {
                            op: sv::ir::ContainerOp::Shuffle,
                            ..
                        } => state.random = true,
                        // The rest are heap calls, which need nothing but
                        // the heap: randomization and the object RNG draw
                        // on the object's own RNG, which is in it.
                        _ => {}
                    },
                    sv::ir::Callee::String(_)
                    | sv::ir::Callee::Checked { .. }
                    | sv::ir::Callee::Lookup { .. }
                    | sv::ir::Callee::Stream(_) => {}
                    sv::ir::Callee::Subroutine(callee) => {
                        if self.dpi_imports.contains_key(&callee) {
                            note(Reason::HostRequest);
                        } else if let Some(subroutine) = self.subroutine(&callee) {
                            default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                            pending.push(callee);
                        } else {
                            note(Reason::Other(format!("no subroutine `{callee}`")));
                        }
                    }
                }
            }
        }
        (found, state)
    }

    /// Whether a call of `subroutine` with `args` has only constant
    /// arguments, its defaults included. Such a call stays inlined: the
    /// inlined body folds to a constant where the design needs one, such as
    /// the bound of a loop that must unroll, which a call cannot give.
    pub(super) fn constant_arguments(
        &self,
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
    ) -> bool {
        // Without arguments the result depends on the body alone, which may
        // read state; a constant one is folded before the call is lowered.
        !subroutine.params.is_empty()
            && subroutine
                .params
                .iter()
                .enumerate()
                .all(|(position, param)| {
                    let Some(actual) = args
                        .get(position)
                        .and_then(Option::as_ref)
                        .or(param.default.as_ref())
                    else {
                        return false;
                    };
                    let mut calls = Vec::new();
                    collect_calls(actual, &mut calls);
                    let mut names = HashSet::default();
                    expr_idents(actual, &mut names);
                    calls.is_empty() && names.iter().all(|name| self.constants.contains_key(name))
                })
    }

    /// Whether `body` may call, directly or through inlined subroutines, a
    /// subroutine that goes out of line: a combinational process that does
    /// is lowered procedurally, since its symbolic form expands every call.
    pub(super) fn calls_out_of_line(&mut self, body: &[sv::ir::Stmt]) -> bool {
        if self.out_of_line().is_none() {
            return false;
        }
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = super::procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            // A call lowered as its constant value calls nothing.
            if args.is_empty() && self.constant_functions.value(&name).is_some() {
                continue;
            }
            let Some(subroutine) = self.subroutine(&name).cloned() else {
                continue;
            };
            // A static subroutine keeps its formals and result in module
            // variables, which a combinational process's call cannot share
            // with it (see `Ff::out_of_line_callee`).
            if subroutine.automatic
                && self.out_of_line_policy(&name)
                && self.out_of_line_reason(&name).is_none()
                && (self.out_of_line_whatever_arguments(&name)
                    || !self.constant_arguments(&subroutine, &args))
            {
                return true;
            }
            default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
            if visited.insert(name) {
                for stmt in &subroutine.body {
                    stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_sum_the_calls_that_inline() {
        // 0 calls 1 twice and 2 once; 2 is recursive, so it counts nothing.
        let own = [1, 10, 100];
        let sites = [vec![1, 1, 2], vec![], vec![2]];
        let members: HashSet<usize> = [2].into_iter().collect();
        assert_eq!(
            fully_inlined_sizes(&own, &sites, &members),
            vec![21, 10, 100]
        );
    }

    #[test]
    fn deep_call_chains_are_sized_without_recursion() {
        // A chain of a million calls on a small stack.
        let depth = 1_000_000;
        let sizes = std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || {
                let own = vec![1; depth];
                let sites: Vec<Vec<usize>> = (0..depth)
                    .map(|node| {
                        if node + 1 < depth {
                            vec![node + 1]
                        } else {
                            Vec::new()
                        }
                    })
                    .collect();
                fully_inlined_sizes(&own, &sites, &HashSet::default())
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(sizes[0], depth as u64);
        assert_eq!(sizes[depth - 1], 1);
    }
}
