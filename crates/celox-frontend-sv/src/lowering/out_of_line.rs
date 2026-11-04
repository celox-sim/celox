//! Subroutines lowered out of line: each into one SIR function unit of the
//! module, which its call sites enter through `SIRInstruction::Call`
//! instead of inlining the body.
//!
//! Only a test switch turns this on (`AnalysisOptions::force_out_of_line_calls`):
//! every subroutine that can be lowered so is, and every other one stays
//! inlined. A subroutine qualifies when it cannot suspend its caller and
//! makes no host request, both transitively (see [`Reason`]), has no `ref`
//! formal, and its unit touches no state but its frame, the variables it
//! reads, and the static storage of subroutines. Nor may it make a store
//! that assembly instruments per store (a nonblocking one, or one into an
//! event, clock or forced object), since assembly sees only the stores of
//! the units the runtime runs: it rejects a function that does. Calls inside a function
//! unit cannot suspend either, so every frame lives only while the unit run
//! that entered it.
//!
//! The frame of a function holds its own frame base and call depth first
//! (the caller passes both: a unit run by the runtime starts with an empty
//! stack at offset zero and depth zero), then its automatic formals, its
//! result and its locals, then the temporaries the lowering of its body
//! created, which a recursive activation must not share. Static storage
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

use super::procedural::{ProcModule, collect_calls, default_calls, expr_idents, stmt_calls};
use super::*;
use celox_sir::{FrameLayout, FrameSlot, FunctionUnit, RegisterId};
use std::collections::BTreeMap;

type Addr = RegionedVarAddr;

/// Why a subroutine stays inlined, in the order the coverage report counts
/// them: the first that applies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Reason {
    /// It, or a subroutine it calls, has a `ref` formal.
    RefFormal,
    /// It, or a subroutine it calls, has a timing control, a fork or a wait.
    MaySuspend,
    /// It, or a subroutine it calls, makes a host request: a system task,
    /// `$sformatf`, file I/O, plusargs, `$system`, or a DPI-C import.
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
    pub handle: bool,
}

/// A subroutine lowered out of line, or being lowered.
#[derive(Debug, Clone)]
pub(super) struct Function {
    pub name: String,
    /// The frame's slots: the frame base, the call depth, then the formals,
    /// result and locals, all known before the body is lowered, then the
    /// temporaries of the body.
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
    /// The outcome of the calls of each called subroutine, for the report.
    outcomes: BTreeMap<String, Outcome>,
    /// How many function bodies are being lowered, nested.
    pub(super) lowering: usize,
    /// The number of frame slots of the module so far, which numbers their
    /// addresses.
    pub(super) slots: u32,
}

impl OutOfLine {
    /// Keep subroutines `names` inlined: their units store into storage
    /// whose stores assembly instruments (see `specialize_module`).
    pub(super) fn keep_inlined(&mut self, names: &[String]) {
        for name in names {
            self.rejected.insert(
                name.clone(),
                Reason::Other("stores into observed storage".to_string()),
            );
        }
    }

    /// The subroutines whose units store into `instrumented`, by name.
    pub(super) fn storing_into(&self, instrumented: &HashSet<SourceVarId>) -> Vec<String> {
        self.functions
            .iter()
            .filter(|function| function.writes.iter().any(|id| instrumented.contains(id)))
            .map(|function| function.name.clone())
            .collect()
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
            self.by_name.remove(&function.name);
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
                            handle: slot.handle,
                        })
                        .collect(),
                },
                effects: Default::default(),
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

/// The functions of `module` a unit of it calls, directly or through other
/// functions, in `module.functions` with the calls renumbered: a function
/// lowered for a call that then stayed inlined (from an `always_ff`
/// process, say) is dropped.
pub(super) fn attach_functions(module: &mut SimModule, mut functions: Vec<FunctionUnit<Addr>>) {
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
    for function in &mut functions {
        renumber(&mut function.unit, &kept);
    }
    for_each_unit_mut(module, &kept);
    module.functions = functions;

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
        let mut names = Vec::new();
        for subroutine in self.subroutines.values() {
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
            names.extend(own.into_iter().filter(|name| !automatic.contains(name)));
        }
        names.into_iter().filter_map(|name| self.id(name)).collect()
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
        let reason = self.find_out_of_line_reason(name);
        if let Some(functions) = self.out_of_line() {
            functions.intrinsic.insert(name.to_string(), reason.clone());
        }
        reason
    }

    fn find_out_of_line_reason(&mut self, name: &str) -> Option<Reason> {
        let mut found: Option<Reason> = None;
        let mut note = |reason: Reason| {
            if found.as_ref().is_none_or(|known| reason < *known) {
                found = Some(reason);
            }
        };
        if self.subroutine_makes_host_request(name) {
            note(Reason::HostRequest);
        }
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
            for param in &subroutine.params {
                if param.direction.is_reference() {
                    note(Reason::RefFormal);
                }
                if !param.leaves.is_empty() {
                    note(Reason::Other("unpacked structure formal".to_string()));
                }
            }
            let mut calls = Vec::new();
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    match stmt {
                        sv::ir::Stmt::Delay { .. }
                        | sv::ir::Stmt::WaitEvent(_)
                        | sv::ir::Stmt::WaitImplicit { .. }
                        | sv::ir::Stmt::Wait(_)
                        | sv::ir::Stmt::WaitFork
                        | sv::ir::Stmt::DisableFork
                        | sv::ir::Stmt::Fork { .. }
                        | sv::ir::Stmt::DelayedNonblocking { .. }
                        | sv::ir::Stmt::DeferActive
                        | sv::ir::Stmt::AssertionRegion { .. } => note(Reason::MaySuspend),
                        sv::ir::Stmt::Assign {
                            nonblocking: true, ..
                        }
                        | sv::ir::Stmt::AssignConcat {
                            nonblocking: true, ..
                        } => note(Reason::Other("instrumented store".to_string())),
                        sv::ir::Stmt::SystemTask { .. } => note(Reason::HostRequest),
                        _ => {}
                    }
                    stmt_calls(stmt, &mut calls);
                });
            }
            while let Some((callee, args)) = calls.pop() {
                match callee {
                    sv::ir::Callee::System(function) => {
                        if function.is_host_request() {
                            note(Reason::HostRequest);
                        } else if function.reads_simulation_state() {
                            // `$time`, `$urandom` and the like read the
                            // process or the simulation, not the frame.
                            note(Reason::Other("reads simulation state".to_string()));
                        }
                    }
                    sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => {
                        note(Reason::HostRequest);
                    }
                    sv::ir::Callee::Object(op) => match &*op {
                        sv::ir::ObjectOp::Load(_)
                        | sv::ir::ObjectOp::Store(_)
                        | sv::ir::ObjectOp::ClassOf
                        | sv::ir::ObjectOp::Check(_)
                        | sv::ir::ObjectOp::Box(_)
                        | sv::ir::ObjectOp::Unbox(_) => {}
                        sv::ir::ObjectOp::Container {
                            op: sv::ir::ContainerOp::Shuffle,
                            ..
                        } => note(Reason::Other("process random state".to_string())),
                        sv::ir::ObjectOp::Container { .. } => {}
                        // Construction draws on the process RNG (IEEE
                        // 1800-2023 18.14), the others are not needed yet.
                        op => {
                            let debug = format!("{op:?}");
                            let variant = debug
                                .split(|c: char| !c.is_alphanumeric())
                                .next()
                                .unwrap_or_default();
                            note(Reason::Other(format!("object operation {variant}")));
                        }
                    },
                    sv::ir::Callee::String(_)
                    | sv::ir::Callee::Checked { .. }
                    | sv::ir::Callee::Lookup { .. }
                    | sv::ir::Callee::Stream(_) => {}
                    sv::ir::Callee::Subroutine(callee) => {
                        if self.dpi_imports.contains_key(&callee) {
                            note(Reason::HostRequest);
                        } else if let Some(subroutine) = self.subroutine(&callee) {
                            default_calls(subroutine, &args, &mut calls);
                            pending.push(callee);
                        } else {
                            note(Reason::Other(format!("no subroutine `{callee}`")));
                        }
                    }
                }
            }
        }
        found
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
                && self.out_of_line_reason(&name).is_none()
                && !self.constant_arguments(&subroutine, &args)
            {
                return true;
            }
            default_calls(&subroutine, &args, &mut calls);
            if visited.insert(name) {
                for stmt in &subroutine.body {
                    stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                }
            }
        }
        false
    }
}
