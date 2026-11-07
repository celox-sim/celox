//! Out-of-line calls (see [`super::super::out_of_line`]): the call sites,
//! and the lowering of a subroutine body into its function unit.

use super::super::out_of_line::{
    Function, Outcome, Place, Reason, Slot, automatic_names, frame_addr, function_key, layout,
};
use super::*;

/// The function whose unit an [`Ff`] is lowering.
pub(super) struct FrameContext {
    /// Its index among the module's functions.
    function: usize,
    /// The variables of the slots that hold the frame's base and the call
    /// depth of the activation.
    base: SourceVarId,
    depth: SourceVarId,
    /// The variables of the slots of the calling process's state that the
    /// body uses (see [`super::super::out_of_line::ProcessState`]): the RNG
    /// state and the name of the process seed, and the seed of `$random`.
    pub(super) random: Option<((SourceVarId, String), String)>,
    pub(super) dist_seed: Option<(SourceVarId, String)>,
    /// For a resumable unit (see [`Function::resume`]), its resume slots.
    resume: Option<ResumeFrame>,
    /// Registers holding a frame size not known yet (see
    /// [`Function::size_patches`]).
    size_patches: Vec<(RegisterId, usize)>,
    /// The `ref` formals the body names as variables of the module (see
    /// [`Place::Variable`]), each with the variable.
    renames: Vec<(String, SourceVarId)>,
    /// The `ref` formals bound to a property or an element, whose handle
    /// or positions are slots of the frame.
    aliases: Vec<super::references::Alias>,
    /// The first variable the lowering of the unit created: those from it
    /// on that the unit touches are slots of its frame.
    start: SourceVarId,
    /// Whether its automatic unpacked arrays stay module variables (see
    /// `ProcModule::single_activation`); otherwise they are slots.
    single: bool,
    /// The variable of the omitted arguments of the call and the positions
    /// of the formals whose defaults the unit evaluates (see
    /// [`Function::omitted`]).
    omitted: Option<(String, Vec<usize>)>,
}

/// The resume slots of a resumable unit being lowered, and its resume
/// points.
struct ResumeFrame {
    resume: SourceVarId,
    status: SourceVarId,
    request: SourceVarId,
    top: SourceVarId,
    /// The block each resume point continues in; point 0 is the start.
    blocks: Vec<BlockId>,
    /// The module variables through which its host requests pass their
    /// values: the host reads and writes them while the unit is suspended,
    /// which only its innermost activation can be, and the unit copies them
    /// from and into its frame right before and after.
    staging: HashSet<SourceVarId>,
    /// The calls at which it can be suspended inside the callee.
    calls: Vec<celox_design::CallResume>,
}

/// A variable that a fork in a unit shares between the parent and the
/// children, which run in copies of the unit's frames (see
/// [`Ff::fork_in_unit`]): a box, a heap object of one field, holds it, and
/// the frame holds the box's handle, which the copies share. A box is made
/// each time the variable's declaration runs, or at the entry for a formal,
/// as a process's automatic is a new cell each time (IEEE 1800-2023 6.21,
/// 9.3.2). The heap collector finds the handle in the frames of the parent
/// and of every child, so the box lives until the last of them that holds
/// it ends or leaves the call.
#[derive(Debug, Clone)]
pub(super) struct SharedBox {
    pub(super) var: String,
    handle: String,
    field: sv::ir::ObjectField,
}

impl Ff<'_, '_> {
    /// The function a call of `name` enters, when the call goes out of line,
    /// with its `ref` formals bound to `places`.
    pub(super) fn out_of_line_callee(
        &mut self,
        name: &str,
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
        places: Option<&[Place]>,
    ) -> Result<Option<usize>, sv::AnalyzerError> {
        if !self.m.out_of_line_policy(name) {
            return Ok(None);
        }
        if let Some(reason) = self.m.out_of_line_reason(name) {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        // Code that must not call, such as the probe of an event expression,
        // and a call that inlining folds, unless it is recursive and so
        // cannot be inlined.
        let recursive = self.active_calls.iter().any(|active| active == name);
        if self.inline_calls {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("event expression".to_string())),
            );
            return Ok(None);
        }
        // A level wait's predicate observes the events whose `triggered` it
        // reads, which the inlined body tells the host (see
        // `ProcessState::observes_events`).
        if self.process_predicate && self.m.process_state(name).observes_events {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("level wait observes its events".to_string())),
            );
            return Ok(None);
        }
        // A call of a subroutine on a cycle of the call graph, or over the
        // size threshold, goes out of line whatever its arguments: neither
        // folds by inlining.
        if !recursive
            && !self.m.out_of_line_whatever_arguments(name)
            && self.m.constant_arguments(subroutine, args)
        {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("constant arguments".to_string())),
            );
            return Ok(None);
        }
        let Some(places) = places else {
            self.record_outcome(name, Outcome::Inlined(Reason::RefFormal));
            return Ok(None);
        };
        let key = function_key(name, places);
        let Some(functions) = self.m.out_of_line() else {
            return Ok(None);
        };
        if let Some(reason) = functions.rejected.get(&key).cloned() {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        let index = match functions.by_name.get(&key).copied() {
            Some(index) => index,
            None => match self.lower_function(name, &key, places) {
                Ok(index) => index,
                Err(reason) => {
                    self.record_outcome(name, Outcome::Inlined(reason));
                    return Ok(None);
                }
            },
        };
        // A unit that can suspend needs a caller that can: a process, or a
        // unit that can itself.
        let functions = self.m.out_of_line().expect("checked above");
        if functions.functions[index].resume.is_some() && !self.can_request() {
            self.record_outcome(name, Outcome::Inlined(Reason::HostRequest));
            return Ok(None);
        }
        // An always_ff process keeps its writes in the working region,
        // which the function, reading stable state, would not see.
        if self.kernel.is_none() && !self.direct {
            let functions = self.m.out_of_line().expect("checked above");
            let consistent = functions.transitive(index).is_some_and(|(reads, writes)| {
                writes.is_empty()
                    && reads.iter().all(|id| {
                        !self.working.contains(id)
                            && !self.m.variables.get(id).is_some_and(SvVariable::is_scratch)
                    })
            });
            if !consistent {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other("state of an always_ff process".to_string())),
                );
                return Ok(None);
            }
        }
        // A combinational process gives the hidden variables it stores, the
        // static storage of the subroutines it inlines among them, a private
        // copy each (see `lower_comb_process`), which the function would not
        // see: the call passes and returns every value through the frame, and
        // the function touches no hidden variable.
        if self.kernel.is_none() && self.direct && self.frame.is_none() {
            let storage: Vec<SourceVarId> = subroutine
                .params
                .iter()
                .map(|param| &param.name)
                .chain(&subroutine.return_var)
                .filter_map(|name| self.m.id(name))
                .map(|id| self.m.declared(id))
                .collect();
            let functions = self.m.out_of_line().expect("checked above");
            let function = &functions.functions[index];
            let in_frame = storage.iter().all(|&id| function.slot(id).is_some());
            let touched = functions.transitive(index);
            let private = touched.as_ref().is_none_or(|(reads, writes)| {
                reads.iter().chain(writes).any(|&id| self.m.is_hidden(id))
            });
            if !in_frame || private {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other(
                        "static storage in a combinational process".to_string(),
                    )),
                );
                return Ok(None);
            }
            // The process's outputs are the variables its own statements
            // assign: a function may write only the static storage of
            // subroutines, which the process does not own, and none that
            // assembly instruments, which it does in the process's own
            // stores only (comb-observer captures).
            let persistent = self.m.static_storage();
            if touched.is_some_and(|(_, writes)| {
                writes
                    .iter()
                    .any(|id| !persistent.contains(id) || self.m.is_observed(*id))
            }) {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other(
                        "module state in a combinational process".to_string(),
                    )),
                );
                return Ok(None);
            }
        }
        // The children of a unit's fork are activations of the kernel.
        if self.kernel.is_some() && self.m.process_state(name).forks {
            self.with_kernel(ProcessKernelBuilder::forks_in_call);
        }
        self.record_outcome(name, Outcome::OutOfLine);
        Ok(Some(index))
    }

    /// The places of the `ref` formals of `subroutine` for a call that binds
    /// them as `references` (the formals that name a variable, with it)
    /// and `aliases` (the others), in the order of the formals; `None` when
    /// one has no place an out-of-line call can pass: a formal of unpacked
    /// structure type, or one automatic of the calling unit bound to two
    /// formals, which the call copies.
    pub(super) fn ref_places(
        &self,
        subroutine: &sv::ir::Subroutine,
        references: &[(String, SourceVarId, SourceVarId)],
        aliases: &[super::references::Alias],
        actual_names: &HashMap<String, String>,
    ) -> Option<Vec<Place>> {
        let mut places = Vec::new();
        let mut copied = HashSet::default();
        for param in subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference())
        {
            if !param.leaves.is_empty() {
                return None;
            }
            if let Some(&(_, _, target)) = references
                .iter()
                .find(|(formal, _, _)| *formal == param.name)
            {
                let name = actual_names.get(&param.name)?;
                // An array in the frame of this activation has no place a
                // call can pass: the call stays inlined, where the formal
                // names the slot.
                if self.frame_array(target, name) {
                    return None;
                }
                if self.frame_variable(target, name) {
                    if !copied.insert(target) {
                        return None;
                    }
                    places.push(Place::Frame {
                        id: target,
                        name: name.clone(),
                    });
                } else {
                    places.push(Place::Variable(target));
                }
                continue;
            }
            let alias = aliases.iter().find(|alias| alias.formal == param.name)?;
            places.push(match &alias.target {
                super::references::Target::Property { handle, field } => Place::Property {
                    // Places of properties that no wait watches share a
                    // function whichever property they are.
                    field: sv::ir::ObjectField {
                        key: if self.m.id(&property_changes(field.key)).is_some() {
                            field.key
                        } else {
                            0
                        },
                        ..*field
                    },
                    handle: handle.clone(),
                },
                super::references::Target::Element {
                    element,
                    signed,
                    width,
                    total,
                } => {
                    let sv::ir::LValue::Select { name, msb, lsb, .. } = &**element else {
                        return None;
                    };
                    // The array is the caller's variable, which the callee
                    // names by its own name: an inlined activation's copy of
                    // a local is not the variable the local's name declares.
                    // An array in the frame of this activation has no name a
                    // call can pass: the call stays inlined.
                    let id = self.m.id(name)?;
                    if self.frame_array(id, name) {
                        return None;
                    }
                    let own = self.m.var(id).path.first().filter(|own| {
                        self.m.var(id).path.len() == 1 && *own != name && self.m.id(own) == Some(id)
                    });
                    let array = own.cloned().unwrap_or_else(|| name.clone());
                    // The positions the call fixed are hidden variables; the
                    // static part names them by their order.
                    let mut names = HashSet::default();
                    let read = sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Literal("1'b0".into())),
                        msb: msb.clone(),
                        lsb: lsb.clone(),
                        signed: false,
                    };
                    super::super::procedural::expr_idents(&read, &mut names);
                    let mut positions: Vec<String> = names
                        .into_iter()
                        .filter(|position| position != name)
                        .filter(|position| {
                            self.m.id(position).is_some_and(|id| self.m.is_hidden(id))
                        })
                        .collect();
                    positions.sort_unstable();
                    let renames: HashMap<String, String> = positions
                        .iter()
                        .enumerate()
                        .map(|(k, position)| (position.clone(), format!("#{k}")))
                        .collect();
                    let mut element = (**element).clone();
                    if let sv::ir::LValue::Select { name, msb, lsb, .. } = &mut element {
                        *name = array;
                        *msb = super::rename_const_idents(msb, &renames);
                        *lsb = super::rename_const_idents(lsb, &renames);
                    }
                    Place::Element {
                        element: Box::new(element),
                        signed: *signed,
                        width: *width,
                        total: *total,
                        positions,
                    }
                }
            });
        }
        Some(places)
    }

    /// Whether variable `id`, named `name`, is an automatic of the unit
    /// being lowered that a call copies into a callee's frame and back (see
    /// [`Place::Frame`]): one that is not an unpacked array, which is a
    /// module variable that a callee names, or a slot (see
    /// [`Self::frame_array`]).
    fn frame_variable(&self, id: SourceVarId, name: &str) -> bool {
        self.m.var(id).array_dims.is_empty() && self.automatic_of_unit(id, name)
    }

    /// Whether variable `id`, named `name`, is an automatic unpacked array
    /// that is a slot of the frame of the unit being lowered: one of a unit
    /// that can have several live activations.
    fn frame_array(&self, id: SourceVarId, name: &str) -> bool {
        !self.m.var(id).array_dims.is_empty()
            && self.frame.as_ref().is_some_and(|frame| !frame.single)
            && self.automatic_of_unit(id, name)
    }

    /// Whether variable `id`, named `name`, is an automatic of the unit
    /// being lowered: a slot of its own, an automatic of a subroutine whose
    /// body it expands, or a variable its lowering created.
    fn automatic_of_unit(&self, id: SourceVarId, name: &str) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };
        let own = self
            .m
            .calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.functions[frame.function].slot(id).is_some());
        own || id.0 >= frame.start.0
            || self.active_calls.iter().any(|active| {
                self.m.subroutine(active).is_some_and(|subroutine| {
                    automatic_names(subroutine).any(|local| local == name)
                        && !subroutine
                            .params
                            .iter()
                            .any(|param| param.direction.is_reference() && param.name == name)
                })
            })
    }

    fn record_outcome(&mut self, name: &str, outcome: Outcome) {
        if let Some(functions) = self.m.out_of_line() {
            functions.record(name, outcome);
        }
    }

    /// Lower subroutine `name` into a new function unit, or say why it
    /// cannot be. A failed lowering leaves nothing behind, so the call is
    /// inlined as if it had never been tried.
    fn lower_function(&mut self, name: &str, key: &str, places: &[Place]) -> Result<usize, Reason> {
        let subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| Reason::Other(format!("no subroutine `{name}`")))?;
        let checkpoint = self.m.checkpoint();
        let names = self.m.declare_names();
        let lowered = self.lower_function_from(&subroutine, key, places);
        self.m.restore_names(names);
        match lowered {
            Ok(index) => {
                let functions = self.m.out_of_line().expect("lowering out of line");
                functions.patch_sizes();
                Ok(index)
            }
            Err(reason) => {
                self.m.rollback(checkpoint);
                if let Some(functions) = self.m.out_of_line() {
                    functions.rejected.insert(key.to_string(), reason.clone());
                }
                Err(reason)
            }
        }
    }

    fn lower_function_from(
        &mut self,
        subroutine: &sv::ir::Subroutine,
        key: &str,
        places: &[Place],
    ) -> Result<usize, Reason> {
        let process = self.m.process_state(&subroutine.name);
        // The simulation time is a module variable, which the unit reads.
        if process.time {
            self.m.time_slot();
        }
        let start = self.m.next_id;
        let (base, _) = self.m.temp("call_frame", 32, false, false);
        let (depth, _) = self.m.temp("call_depth", 32, false, false);
        // Only the automatic storage of the subroutine lives in its frame;
        // its static storage stays module variables (IEEE 1800-2023 13.4.2).
        let mut slots = vec![self.slot_of(base), self.slot_of(depth)];
        // The resume point and what a suspension leaves.
        let mut resume = None;
        let mut resume_slot = None;
        if process.suspends {
            resume_slot = Some(slots.len());
            let mut var = |purpose, width| {
                let (id, _) = self.m.temp(purpose, width, false, false);
                slots.push(self.slot_of(id));
                id
            };
            let ids = [
                var(
                    "call_resume",
                    celox_frontend_core::process::PROCESS_RESUME_WIDTH,
                ),
                var("call_status", celox_design::PROCESS_STATUS_WIDTH),
                var("call_request", celox_design::PROCESS_DELAY_WIDTH),
                var("call_top", celox_design::PROCESS_CALL_TOP_WIDTH),
            ];
            resume = Some(ResumeFrame {
                resume: ids[0],
                status: ids[1],
                request: ids[2],
                top: ids[3],
                blocks: Vec::new(),
                staging: HashSet::default(),
                calls: Vec::new(),
            });
        }
        // The calling process's state, which the caller passes in and,
        // for the RNG states, takes back.
        let mut random = None;
        let mut process_slots = (None, None);
        if process.random {
            process_slots.0 = Some((slots.len(), slots.len() + 1));
            let state = self.m.temp("call_random", 33, false, false);
            let (seed, seed_name) = self.m.temp("call_random_seed", 32, false, false);
            slots.push(self.slot_of(state.0));
            slots.push(self.slot_of(seed));
            random = Some((state, seed_name));
        }
        let mut dist_seed = None;
        if process.dist_seed {
            process_slots.1 = Some(slots.len());
            let seed = self.m.temp("call_dist_seed", 32, false, false);
            slots.push(self.slot_of(seed.0));
            dist_seed = Some(seed);
        }
        // The dynamic parts of the places of the `ref` formals (see
        // [`Place`]), which the caller passes.
        let mut place_slots = Vec::new();
        let mut renames = Vec::new();
        let mut aliases = Vec::new();
        let mut referents = HashSet::default();
        let references = subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference());
        for (param, place) in references.zip(places) {
            let mut own = Vec::new();
            match place {
                Place::Variable(id) => {
                    renames.push((param.name.clone(), *id));
                    referents.insert(*id);
                }
                Place::Property { field, .. } => {
                    let (handle, handle_name) =
                        self.m
                            .temp(super::references::OBJECT_TEMP, 32, false, false);
                    self.m
                        .variables
                        .get_mut(&handle)
                        .expect("a temp")
                        .value_kind = celox_design::ValueKind::ObjRef;
                    own.push(slots.len());
                    slots.push(self.slot_of(handle));
                    aliases.push(super::references::Alias {
                        formal: param.name.clone(),
                        source_name: param.source_name.clone(),
                        target: super::references::Target::Property {
                            handle: handle_name,
                            field: *field,
                        },
                    });
                }
                Place::Element {
                    element,
                    signed,
                    width,
                    total,
                    positions,
                } => {
                    // The array is the caller's, which the unit writes.
                    if let sv::ir::LValue::Select { name, .. } = &**element
                        && let Some(id) = self.m.id(name)
                    {
                        referents.insert(id);
                    }
                    let mut names = HashMap::default();
                    for (k, caller) in positions.iter().enumerate() {
                        let position = self
                            .m
                            .id(caller)
                            .map(|id| self.m.var(id).clone())
                            .ok_or_else(|| Reason::Other(format!("position `{caller}`")))?;
                        let (temp, temp_name) = self.m.temp(
                            "position",
                            position.width,
                            position.signed,
                            position.is_4state,
                        );
                        own.push(slots.len());
                        slots.push(self.slot_of(temp));
                        names.insert(format!("#{k}"), temp_name);
                    }
                    let mut element = (**element).clone();
                    if let sv::ir::LValue::Select { msb, lsb, .. } = &mut element {
                        *msb = super::rename_const_idents(msb, &names);
                        *lsb = super::rename_const_idents(lsb, &names);
                    }
                    aliases.push(super::references::Alias {
                        formal: param.name.clone(),
                        source_name: param.source_name.clone(),
                        target: super::references::Target::Element {
                            element: Box::new(element),
                            signed: *signed,
                            width: *width,
                            total: *total,
                        },
                    });
                }
                Place::Frame { .. } => {
                    let id = self
                        .m
                        .id(&param.name)
                        .ok_or_else(|| Reason::Other(format!("formal `{}`", param.name)))?;
                    own.push(slots.len());
                    slots.push(self.slot_of(id));
                }
            }
            place_slots.push(own);
        }
        // The defaults of the input formals are evaluated by the unit, static
        // or automatic (see [`Function::omitted`]).
        let defaulted: Vec<usize> = subroutine
            .params
            .iter()
            .enumerate()
            .filter(|(position, param)| {
                *position < 64
                    && param.default.is_some()
                    && param.direction.is_read()
                    && !param.direction.is_written()
                    && !param.direction.is_reference()
                    && param.leaves.is_empty()
            })
            .map(|(position, _)| position)
            .collect();
        let mut omitted = None;
        let mut omitted_slot = None;
        if !defaulted.is_empty() {
            let (id, name) = self.m.temp("call_omitted", 64, false, false);
            omitted_slot = Some((slots.len(), defaulted.clone()));
            slots.push(self.slot_of(id));
            omitted = Some((name, defaulted));
        }
        // An automatic unpacked array has no frame slot: it stays a module
        // variable when no two activations of the unit can be live at once
        // (see `ProcModule::single_activation`).
        let single = self.m.single_activation(&subroutine.name);
        for name in automatic_names(subroutine) {
            let Some(id) = self.m.id(name) else {
                continue;
            };
            if slots.iter().any(|slot| slot.id == id) {
                continue;
            }
            if !self.m.var(id).array_dims.is_empty() {
                if single {
                    continue;
                }
                array_slot(self.m.var(id))?;
            }
            slots.push(self.slot_of(id));
        }
        let functions = self.m.out_of_line().expect("lowering out of line");
        let index = functions.functions.len();
        functions.functions.push(Function {
            key: key.to_string(),
            places: place_slots,
            slots,
            unit: None,
            size: 0,
            offsets: Vec::new(),
            reads: HashSet::default(),
            writes: HashSet::default(),
            callees: Vec::new(),
            size_patches: Vec::new(),
            random: process_slots.0,
            dist_seed: process_slots.1,
            resume: resume_slot,
            calls: Vec::new(),
            omitted: omitted_slot,
        });
        functions.by_name.insert(key.to_string(), index);
        functions.lowering += 1;
        let frame = FrameContext {
            function: index,
            base,
            depth,
            random,
            dist_seed,
            resume,
            size_patches: Vec::new(),
            renames,
            aliases,
            start,
            single,
            omitted,
        };
        let lowered = Ff::new(&mut *self.m, 0).lower_function_unit(subroutine, frame);
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.lowering -= 1;
        let (unit, size_patches, staging, calls) =
            lowered.map_err(|error| Reason::Other(error.to_string()))?;
        self.finish_function(
            index,
            unit,
            size_patches,
            start,
            &staging,
            &referents,
            single,
        )?;
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.functions[index].calls = calls;
        Ok(index)
    }

    fn slot_of(&mut self, id: SourceVarId) -> Slot {
        let functions = self.m.out_of_line().expect("lowering out of line");
        // Frame addresses are a namespace of their own (the FRAME region):
        // numbering them from the top of the id space keeps them apart from
        // the module's variables in dumps.
        let address = SourceVarId(u32::MAX / 2 + functions.slots);
        functions.slots += 1;
        let variable = self.m.var(id);
        Slot {
            id,
            address,
            width: variable.width,
            four_state: variable.is_4state && self.m.four_state,
            kind: variable.value_kind,
            element_width: super::super::unpacked_element_width(variable),
        }
    }

    /// Lower the body of `subroutine` as the unit of function `index`, in
    /// a fresh [`Ff`].
    fn lower_function_unit(
        mut self,
        subroutine: &sv::ir::Subroutine,
        frame: FrameContext,
    ) -> Result<
        (
            ExecutionUnit<Addr>,
            Vec<(RegisterId, usize)>,
            HashSet<SourceVarId>,
            Vec<celox_design::CallResume>,
        ),
        sv::AnalyzerError,
    > {
        self.direct = true;
        // The body draws on the RNG states in the frame, which the caller
        // passes.
        let (random, dist_seed) = (frame.random.is_some(), frame.dist_seed.is_some());
        self.random = frame.random.as_ref().map(|(state, _)| state.clone());
        self.dist_seed = frame.dist_seed.clone();
        self.frame = Some(frame);
        let start = self.b.current_block();
        if let Some(resume) = self.resume_frame() {
            resume.blocks.push(start);
        }
        let return_block = self.b.new_block();
        self.functions.push(FunctionBlocks {
            return_block,
            return_var: subroutine.return_var.clone(),
            loop_depth: 0,
        });
        if let Some(return_var) = &subroutine.return_var
            && !subroutine.persistent_locals.contains(return_var)
        {
            let id = self
                .m
                .id(return_var)
                .ok_or_else(|| unsupported(format!("result of `{}`", subroutine.name)))?;
            self.init_default(id);
        }
        // The body names the variable of each `ref` formal bound to one,
        // and reads and writes the property or element of the others
        // through the slots of the handle and positions.
        let (renames, aliases) = self
            .frame
            .as_mut()
            .map(|frame| {
                (
                    std::mem::take(&mut frame.renames),
                    std::mem::take(&mut frame.aliases),
                )
            })
            .unwrap_or_default();
        let mut body = subroutine.body.clone();
        // A formal the call omits takes its default, evaluated here.
        if let Some((mask, positions)) = self.frame.as_mut().and_then(|frame| frame.omitted.take())
        {
            let prologue = positions.into_iter().map(|position| {
                let param = &subroutine.params[position];
                let bit = sv::ir::ConstExpr::Literal(position.to_string());
                sv::ir::Stmt::If {
                    condition: sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Ident(mask.clone())),
                        msb: bit.clone(),
                        lsb: bit,
                        signed: false,
                    },
                    then_body: vec![sv::ir::Stmt::Assign {
                        lhs: sv::ir::LValue::Ident(param.name.clone()),
                        rhs: param.default.clone().expect("a defaulted formal"),
                        nonblocking: false,
                    }],
                    else_body: Vec::new(),
                }
            });
            body.splice(0..0, prologue);
        }
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let constant = |position: &sv::ir::ConstExpr| {
            sv::typecheck::eval_const_expr_with_types(position, constants, parameter_types)
        };
        self.split_property_concats(&mut body, &aliases);
        for alias in &aliases {
            body = alias.rewrite(body, &constant, &mut |field| self.part_scratch(field))?;
        }
        let (body, shared) = self.share_forked(subroutine, body)?;
        let originals: Vec<(String, Option<SourceVarId>)> = renames
            .iter()
            .map(|(formal, target)| {
                (
                    formal.clone(),
                    self.m.name_to_id.insert(formal.clone(), *target),
                )
            })
            .collect();
        self.active_calls.push(subroutine.name.clone());
        let falls = self.exec_block(&body);
        self.shared_boxes.truncate(self.shared_boxes.len() - shared);
        for (formal, original) in originals {
            match original {
                Some(id) => self.m.name_to_id.insert(formal, id),
                None => self.m.name_to_id.remove(&formal),
            };
        }
        if falls? {
            self.jump(return_block);
        }
        self.b.switch_to_block(return_block);
        if let Some(status) = self.resume_frame().map(|resume| resume.status) {
            let done = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
            self.b.emit(SIRInstruction::Imm(done, SIRValue::new(0u8)));
            self.store(
                status,
                SIROffset::Static(0),
                celox_design::PROCESS_STATUS_WIDTH,
                done,
            );
        }
        self.b.seal_block(SIRTerminator::Return);
        // The entry continues the body where the activation last suspended.
        let mut entry = start;
        if let Some((resume, blocks)) = self
            .resume_frame()
            .map(|resume| (resume.resume, resume.blocks.clone()))
        {
            let invalid = self.b.new_block();
            self.b.switch_to_block(invalid);
            let code = self
                .m
                .runtime_error(format!("invalid resume point of `{}`", subroutine.name));
            self.b.seal_block(SIRTerminator::Error(code));
            entry = self.b.new_block();
            self.b.switch_to_block(entry);
            let point = self
                .b
                .alloc_bit(celox_frontend_core::process::PROCESS_RESUME_WIDTH, false);
            self.b.emit(SIRInstruction::Load(
                point,
                stable(resume),
                SIROffset::Static(0),
                celox_frontend_core::process::PROCESS_RESUME_WIDTH,
            ));
            celox_frontend_core::process::resume_dispatch(&mut self.b, point, &blocks, invalid);
        }
        // The eligibility check keeps out what needs the state of a process;
        // this is the net under it.
        if !self.host_request_sites.is_empty()
            || !self.wait_sites.is_empty()
            || !self.nba_sites.is_empty()
            || !self.event_probes.is_empty()
            || (self.random.is_some() && !random)
            || (self.dist_seed.is_some() && !dist_seed)
        {
            return Err(unsupported("process state in a function unit"));
        }
        let unit = seal_drained_from(std::mem::take(&mut self.b), entry);
        let frame = self.frame.take().expect("lowering a function unit");
        let (staging, calls) = frame
            .resume
            .map(|resume| (resume.staging, resume.calls))
            .unwrap_or_default();
        Ok((unit, frame.size_patches, staging, calls))
    }

    /// The delay word `word` of a suspension with status `code` from a call
    /// of the kernel being lowered, as the process's: a wait at function
    /// wait site `k` is a wait at the process's site `own + k`, `own` the
    /// number of the kernel's own wait sites, and a nonblocking assignment
    /// at function NBA site `k` one at its NBA site `own + k`, `own` the
    /// number of its own NBA sites; each number is patched in when the
    /// kernel is finished (see `Ff::wait_offset_patches`). `waits` and
    /// `nbas` say which the callee can suspend for.
    fn function_site_word(
        &mut self,
        code: RegisterId,
        mut word: RegisterId,
        waits: bool,
        nbas: bool,
    ) -> RegisterId {
        let width = celox_design::PROCESS_DELAY_WIDTH;
        let shift = |this: &mut Self,
                     statuses: &[celox_design::ProcessStatus],
                     nba: bool,
                     word: RegisterId| {
            let mut matched: Option<RegisterId> = None;
            for status in statuses {
                let value = this.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
                this.b
                    .emit(SIRInstruction::Imm(value, SIRValue::new(status.code())));
                let equal = this.b.alloc_bit(1, false);
                this.b
                    .emit(SIRInstruction::Binary(equal, code, BinaryOp::Eq, value));
                matched = Some(match matched {
                    Some(any) => {
                        let either = this.b.alloc_bit(1, false);
                        this.b
                            .emit(SIRInstruction::Binary(either, any, BinaryOp::Or, equal));
                        either
                    }
                    None => equal,
                });
            }
            let matched = matched.expect("a status");
            let zeros = this.b.alloc_bit(width - 1, false);
            this.b.emit(SIRInstruction::Imm(zeros, SIRValue::new(0u8)));
            let factor = this.b.alloc_bit(width, false);
            this.b
                .emit(SIRInstruction::Concat(factor, vec![zeros, matched]));
            let own = this.b.alloc_bit(width, false);
            this.b.emit(SIRInstruction::Imm(own, SIRValue::new(0u8)));
            if nba {
                this.nba_offset_patches.push(own);
            } else {
                this.wait_offset_patches.push(own);
            }
            let offset = this.b.alloc_bit(width, false);
            this.b
                .emit(SIRInstruction::Binary(offset, factor, BinaryOp::Mul, own));
            let translated = this.b.alloc_bit(width, false);
            this.b.emit(SIRInstruction::Binary(
                translated,
                word,
                BinaryOp::Add,
                offset,
            ));
            translated
        };
        if waits {
            word = shift(
                self,
                &[
                    celox_design::ProcessStatus::Wait,
                    celox_design::ProcessStatus::Pending,
                ],
                false,
                word,
            );
        }
        if nbas {
            word = shift(self, &[celox_design::ProcessStatus::Nba], true, word);
        }
        word
    }

    /// A nonblocking update of bits `lsb..lsb + width` of `target`, its
    /// value in variable `staged` and its run-time position, if any, in
    /// `position`, from the unit being lowered: the value and position go
    /// into the staging objects of a new function NBA site, and the unit
    /// suspends with [`celox_design::ProcessStatus::Nba`], whose word the
    /// calling kernel turns into its process's site.
    pub(super) fn unit_nonblocking(
        &mut self,
        delay: u64,
        target: SourceVarId,
        lsb: usize,
        width: usize,
        staged: SourceVarId,
        position: Option<NbaPosition<SourceVarId>>,
    ) -> Result<(), sv::AnalyzerError> {
        use celox_frontend_core::process::NBA_POSITION_WIDTH;
        if !self.can_suspend_unit() {
            return Err(unsupported(
                "a nonblocking assignment in a unit that cannot suspend",
            ));
        }
        let four_state = self.m.var(staged).is_4state;
        let (value, _) = self.m.temp("nba_value", width, false, four_state);
        self.copy_variable(staged, value);
        let slots = match position {
            Some(position) => {
                let offset = self
                    .m
                    .temp("nba_offset", NBA_POSITION_WIDTH, false, false)
                    .0;
                let width = self.m.temp("nba_width", NBA_POSITION_WIDTH, false, false).0;
                self.copy_variable(position.offset, offset);
                self.copy_variable(position.width, width);
                Some(NbaPosition { offset, width })
            }
            None => None,
        };
        if let Some(resume) = self.resume_frame() {
            resume.staging.insert(value);
            if let Some(slots) = slots {
                resume.staging.extend([slots.offset, slots.width]);
            }
        }
        let index = self.m.function_nba_sites.len() as u64;
        self.m.function_nba_sites.push(NbaSite {
            delay,
            process: 0,
            target,
            lsb,
            width,
            value,
            position: slots,
            pending: None,
        });
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(index)));
        self.suspend_unit_with(celox_design::ProcessStatus::Nba, request)
    }

    /// Whether a unit with resume slots is being lowered, which can
    /// suspend the process it runs in.
    pub(super) fn can_suspend_unit(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.resume.is_some())
    }

    /// The end of the frame of the unit being lowered, function `own`
    /// whose frame starts at the value of `base`.
    fn own_top(&mut self, base: SourceVarId, own: usize) -> RegisterId {
        let current = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Load(
            current,
            stable(base),
            SIROffset::Static(0),
            32,
        ));
        // The frame size is patched once known.
        let size = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Imm(size, SIRValue::new(0u32)));
        self.frame
            .as_mut()
            .expect("lowering a function unit")
            .size_patches
            .push((size, own));
        let end = self.b.alloc_bit(32, false);
        self.b
            .emit(SIRInstruction::Binary(end, current, BinaryOp::Add, size));
        end
    }

    /// Suspend the unit being lowered with the delay word `word` of a wait
    /// at one of the module's function wait sites (see
    /// `ProcModule::function_wait_sites`); the open block afterwards is the
    /// resume point, whose value it returns.
    pub(super) fn unit_wait(&mut self, word: u64) -> Result<u32, sv::AnalyzerError> {
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(word)));
        self.unit_wait_word(request)
    }

    /// [`Self::unit_wait`] with the delay word in register `request`.
    fn unit_wait_word(&mut self, request: RegisterId) -> Result<u32, sv::AnalyzerError> {
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Wait.code()),
        ));
        self.suspend_unit(status, request, None)
    }

    /// The process continues after a wait in the unit being lowered: a
    /// flush point of its deferred assertions (IEEE 1800-2023 16.4.2), as
    /// a kernel's resume from a wait is. The site is the module's one for
    /// every unit; it flushes the queue of the process the unit runs in.
    pub(super) fn unit_flush_point(&mut self) {
        let site = match self.m.unit_flush_site {
            Some(site) => site,
            None => {
                let site = self.m.event_site(RuntimeEventSite {
                    kind: RuntimeEventKind::Write,
                    template: None,
                    sizing: DisplaySizing::Ieee,
                    scope: None,
                    location: None,
                    assertion: None,
                    deferred: Some(DeferredSite::ProcessFlushPoint),
                    timestamp_arg: false,
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                });
                self.m.unit_flush_site = Some(site);
                site
            }
        };
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id: site,
            args: Vec::new(),
        });
    }

    /// At resume point `point` of a wait with delay word `word`: continue
    /// when `woken`, a one-bit register, holds; otherwise report that the
    /// wait is still pending and return to the same point, as a kernel's
    /// `wake_if` does. The continuation is open afterwards.
    pub(super) fn unit_wake_if(
        &mut self,
        woken: RegisterId,
        point: u32,
        word: u64,
    ) -> Result<(), sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let resume = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a wait in a unit that cannot suspend"))?;
        let (resume_var, status_var, request_var, top_var) =
            (resume.resume, resume.status, resume.request, resume.top);
        let continuation = self.b.new_block();
        let pending = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: woken,
            true_block: (continuation, Vec::new()),
            false_block: (pending, Vec::new()),
        });
        self.b.switch_to_block(pending);
        let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
        let point_value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            point_value,
            SIRValue::new(u64::from(point)),
        ));
        self.store(resume_var, SIROffset::Static(0), width, point_value);
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Pending.code()),
        ));
        self.store(
            status_var,
            SIROffset::Static(0),
            celox_design::PROCESS_STATUS_WIDTH,
            status,
        );
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(word)));
        self.store(
            request_var,
            SIROffset::Static(0),
            celox_design::PROCESS_DELAY_WIDTH,
            request,
        );
        let top = self.own_top(base, own);
        self.store(
            top_var,
            SIROffset::Static(0),
            celox_design::PROCESS_CALL_TOP_WIDTH,
            top,
        );
        self.b.seal_block(SIRTerminator::Return);
        self.b.switch_to_block(continuation);
        Ok(())
    }

    /// An event control in the unit being lowered (IEEE 1800-2023 9.4.2).
    ///
    /// Each occurrence of an item is counted where it happens, as for a
    /// kernel, when the items are module variables whose `iff` is either a
    /// module variable or reads only the unit's frame. Such a frame-only
    /// condition, such as whether a virtual interface the frame holds
    /// denotes an instance (the analyzer makes one item per instance it may
    /// denote), cannot change while the process waits: the unit evaluates
    /// it at the start of the wait and waits at the site whose terms are
    /// the items it selects, one site for each selection, among the sites
    /// it lays out for the control. Items whose conditions compare one value
    /// with distinct constants select at most one of them; otherwise each
    /// set of them is a site, for up to [`MAX_SELECTED_ITEMS`] such items.
    ///
    /// Any other item reads the frame in its expression: the unit keeps a
    /// sample of each item in its frame, waits for a change of what the
    /// items read, and at each resume compares each item with its sample by
    /// the edge rules of the item (Table 9-2), waiting again when none
    /// occurred; samples in module variables, as a kernel's `$event[k]`
    /// proxies take them, would be shared by every process waiting at the
    /// site.
    pub(super) fn unit_wait_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<(), sv::AnalyzerError> {
        let items: Vec<sv::ir::EventItem> = items
            .iter()
            .map(|item| sv::ir::EventItem {
                edge: item.edge,
                expr: self.propagate(&item.expr),
                iff: item.iff.as_ref().map(|iff| self.propagate(iff)),
            })
            .collect();
        let shared = |this: &Self, expr: &sv::ir::Expr, one_bit: bool| match expr {
            sv::ir::Expr::Ident(name) => this.m.id(name).is_some_and(|id| {
                let variable = this.m.var(id);
                !this.m.is_hidden(id)
                    && variable.array_dims.is_empty()
                    && (!one_bit || (variable.width == 1 && variable.float.is_none()))
            }),
            _ => false,
        };
        // Items that read no slot of the frame are those of a kernel's event
        // control, over module storage, static storage of subroutines
        // included: the kernel's terms, gates and proxies serve them.
        if items.iter().all(|item| {
            !self.reads_frame(&item.expr)
                && item.iff.as_ref().is_none_or(|iff| !self.reads_frame(iff))
        }) {
            let mut terms = Vec::new();
            let mut gates = Vec::new();
            let mut samples = Vec::new();
            self.event_terms(&items, &mut terms, &mut gates, &mut samples)?;
            return self.suspend_on(terms, gates, samples);
        }
        // The items whose condition the frame decides, by position.
        let mut selected = Vec::new();
        let counted = items.iter().enumerate().all(|(position, item)| {
            shared(self, &item.expr, false)
                && match &item.iff {
                    None => true,
                    Some(iff) if self.frame_only(iff) => {
                        selected.push(position);
                        true
                    }
                    Some(iff) => !self.reads_frame(iff),
                }
        });
        if !counted {
            return self.unit_sampled_event(&items);
        }
        // The weight of each selected item in the number of the site: with
        // conditions that select at most one item, its rank; otherwise a bit.
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let value = |expr: &sv::ir::Expr| {
            let constant = match expr {
                sv::ir::Expr::Literal(literal) => sv::ir::ConstExpr::Literal(literal.clone()),
                sv::ir::Expr::Ident(name) if constants.contains_key(name) => {
                    sv::ir::ConstExpr::Ident(name.clone())
                }
                _ => return None,
            };
            sv::typecheck::eval_const_expr_with_types(&constant, constants, parameter_types)
        };
        let exclusive = exclusive_selection(
            selected.iter().map(|&position| {
                items[position]
                    .iff
                    .as_ref()
                    .expect("a selected item has a condition")
            }),
            value,
        );
        if !exclusive && selected.len() > MAX_SELECTED_ITEMS {
            return self.unit_sampled_event(&items);
        }
        let selections: Vec<Vec<usize>> = if exclusive {
            std::iter::once(Vec::new())
                .chain(selected.iter().map(|&position| vec![position]))
                .collect()
        } else {
            (0..1usize << selected.len())
                .map(|mask| {
                    (0..selected.len())
                        .filter(|bit| mask & (1 << bit) != 0)
                        .map(|bit| selected[bit])
                        .collect()
                })
                .collect()
        };
        let mut first = None;
        let mut samples = Vec::new();
        for chosen in &selections {
            let site_items: Vec<sv::ir::EventItem> = items
                .iter()
                .enumerate()
                .filter(|(position, _)| !selected.contains(position) || chosen.contains(position))
                .map(|(position, item)| sv::ir::EventItem {
                    edge: item.edge,
                    expr: item.expr.clone(),
                    iff: if selected.contains(&position) {
                        None
                    } else {
                        item.iff.clone()
                    },
                })
                .collect();
            let mut terms = Vec::new();
            let mut gates = Vec::new();
            self.event_terms(&site_items, &mut terms, &mut gates, &mut samples)?;
            if gates.iter().all(Option::is_none) {
                gates.clear();
            }
            let site = self.wait_site(terms, gates);
            first.get_or_insert(site);
        }
        self.unit_samples(std::mem::take(&mut samples))?;
        let first = first.expect("one site at least");
        // The number of the site the frame selects now.
        let mut index = sv::ir::Expr::Literal("64'd0".into());
        for (rank, &position) in selected.iter().enumerate() {
            let weight = if exclusive { rank + 1 } else { 1 << rank };
            index = binary(
                index,
                sv::ir::BinaryOp::Add,
                sv::ir::Expr::Mux {
                    condition: Box::new(iff_truth(
                        items[position]
                            .iff
                            .clone()
                            .expect("a selected item has a condition"),
                    )),
                    then_expr: Box::new(sv::ir::Expr::Literal(format!("64'd{weight}"))),
                    else_expr: Box::new(sv::ir::Expr::Literal("64'd0".into())),
                },
            );
        }
        let index = self.integer_of_width(&index, 64, false)?;
        let base = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(base, SIRValue::new(u64::from(first))));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(word, base, BinaryOp::Add, index));
        self.unit_wait_word(word)?;
        self.unit_flush_point();
        Ok(())
    }

    /// Whether variable `name` is a slot of the frame of the unit being
    /// lowered: an automatic of the unit or of a body it expands, or a
    /// temporary of its lowering. A static formal or local of a static
    /// subroutine is module-wide storage, which another call may change.
    fn frame_slot(&self, name: &str) -> bool {
        self.m
            .id(name)
            .is_some_and(|id| self.m.is_hidden(id) && self.automatic_of_unit(id, name))
    }

    /// Whether `expr` reads only the frame of the unit being lowered and
    /// constants, and calls nothing: its value does not change while the
    /// process waits, since only the process writes its frame.
    fn frame_only(&self, expr: &sv::ir::Expr) -> bool {
        let mut calls = Vec::new();
        collect_calls(expr, &mut calls);
        let mut names = HashSet::default();
        super::super::procedural::expr_idents(expr, &mut names);
        calls.is_empty()
            && names
                .iter()
                .all(|name| self.m.constants.contains_key(name) || self.frame_slot(name))
    }

    /// Whether `expr` reads a slot of the frame of the unit being lowered.
    fn reads_frame(&self, expr: &sv::ir::Expr) -> bool {
        let mut names = HashSet::default();
        super::super::procedural::expr_idents(expr, &mut names);
        names.iter().any(|name| self.frame_slot(name))
    }

    /// Whether `expr` reads a hidden variable that is not a slot of the
    /// frame: static storage of a subroutine, which a concurrent call may
    /// change.
    fn reads_static_storage(&self, expr: &sv::ir::Expr) -> bool {
        let mut names = HashSet::default();
        super::super::procedural::expr_idents(expr, &mut names);
        names.iter().any(|name| {
            self.m
                .id(name)
                .is_some_and(|id| self.m.is_hidden(id) && !self.frame_slot(name))
        })
    }

    /// Take `samples` of an event control of the unit being lowered, as a
    /// kernel takes them before it suspends (see `Ff::event_expression`):
    /// the proxies and their operand copies are module variables, which
    /// every process waiting there may share, since the operands are the
    /// module's storage, never the frame's.
    pub(super) fn unit_samples(
        &mut self,
        samples: Vec<(String, sv::ir::Expr)>,
    ) -> Result<(), sv::AnalyzerError> {
        for (target, value) in samples {
            if let Some(id) = self.m.id(&target)
                && let Some(resume) = self.resume_frame()
            {
                resume.staging.insert(id);
            }
            self.assign(&sv::ir::LValue::Ident(target), &value)?;
        }
        Ok(())
    }

    /// The event control `items` by samples in the frame (see
    /// [`Self::unit_wait_event`]).
    fn unit_sampled_event(&mut self, items: &[sv::ir::EventItem]) -> Result<(), sv::AnalyzerError> {
        let mut ids = Vec::new();
        let mut samples = Vec::new();
        for item in items {
            let expr = item.expr.clone();
            // The unit evaluates the items, and their gates, at each resume.
            self.check_event_purity(&expr)?;
            if let Some(iff) = &item.iff {
                self.check_event_purity(iff)?;
            }
            if !matches!(item.edge, sv::ir::EventEdge::Any)
                && self.m.expr_float_precision(&expr).is_some()
            {
                return Err(sv::AnalyzerError::RealEdgeEvent {
                    edge: match item.edge {
                        sv::ir::EventEdge::Pos => "posedge",
                        sv::ir::EventEdge::Neg => "negedge",
                        _ => "edge",
                    },
                });
            }
            ids.extend(self.level_terms(&expr)?);
            // An occurrence counts only if the `iff` condition holds when it
            // happens (IEEE 1800-2023 9.4.2.3), which this comparison at the
            // resume cannot judge for a condition that may change meanwhile;
            // a kernel's activation probe and gate counters do. A condition
            // on the frame alone cannot change during the wait: its value at
            // the start is its value at every occurrence.
            // An operand in the static storage of a subroutine, which a
            // concurrent call may change, neither wakes the unit nor is in
            // its frame: the unit stays inlined, where a kernel watches it.
            if self.reads_static_storage(&expr) {
                return Err(unsupported(
                    "an event control on the frame of a unit reading static storage",
                ));
            }
            if item.iff.as_ref().is_some_and(|iff| !self.frame_only(iff)) {
                return Err(unsupported(
                    "an event control on the frame of a unit gated by module state",
                ));
            }
            let float = self.m.expr_float_precision(&expr);
            let width = match float {
                Some(celox_sir::FloatPrecision::F64) => 64,
                Some(celox_sir::FloatPrecision::F32) => 32,
                None => sv_expr_natural_width(
                    &self.m.typed_calls(&expr),
                    self.m.variables,
                    self.m.name_to_id,
                    self.m.constants,
                    self.m.parameter_types,
                )
                .ok_or_else(|| unsupported("event expression of unknown width"))?,
            };
            let signed = self.expr_signed(&expr);
            let mut temp = |purpose| {
                let (id, name) = self.m.temp(purpose, width, signed, float.is_none());
                self.m.variables.get_mut(&id).expect("a sample").float = float;
                name
            };
            let sample = temp("event_sample");
            let current = temp("event_current");
            let gate = match &item.iff {
                Some(_) => {
                    let (_, gate) = self.m.temp("event_gate", 1, false, false);
                    Some(gate)
                }
                None => None,
            };
            samples.push((sample, current, expr, item.edge, item.iff.clone(), gate));
        }
        ids.sort_unstable();
        ids.dedup();
        let site = self.wait_site(
            ids.into_iter()
                .map(|id| (id, CounterKind::Changes))
                .collect(),
            Vec::new(),
        );
        for (sample, _, expr, _, iff, gate) in &samples {
            self.assign(&sv::ir::LValue::Ident(sample.clone()), expr)?;
            if let (Some(iff), Some(gate)) = (iff, gate) {
                self.assign(
                    &sv::ir::LValue::Ident(gate.clone()),
                    &iff_truth(iff.clone()),
                )?;
            }
        }
        let word = u64::from(site);
        let point = self.unit_wait(word)?;
        let mut occurred: Option<sv::ir::Expr> = None;
        for (sample, current, expr, edge, _, gate) in &samples {
            self.assign(&sv::ir::LValue::Ident(current.clone()), expr)?;
            let mut item = edge_occurred(sample, current, *edge);
            if let Some(gate) = gate {
                item = binary(
                    item,
                    sv::ir::BinaryOp::LogicAnd,
                    sv::ir::Expr::Ident(gate.clone()),
                );
            }
            occurred = Some(match occurred {
                Some(any) => binary(any, sv::ir::BinaryOp::LogicOr, item),
                None => item,
            });
        }
        let occurred = occurred.ok_or_else(|| unsupported("an empty event control"))?;
        let (woken, _) = self.eval_truth(&occurred)?;
        let continuation = self.b.new_block();
        let again = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: woken,
            true_block: (continuation, Vec::new()),
            false_block: (again, Vec::new()),
        });
        // No occurrence: the values now are the samples of the next check.
        self.b.switch_to_block(again);
        for (sample, current, ..) in &samples {
            self.assign(
                &sv::ir::LValue::Ident(sample.clone()),
                &sv::ir::Expr::Ident(current.clone()),
            )?;
        }
        let never = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(never, SIRValue::new(0u8)));
        self.unit_wake_if(never, point, word)?;
        // The continuation of `unit_wake_if` is never taken.
        self.b.seal_block(SIRTerminator::Return);
        self.b.switch_to_block(continuation);
        self.unit_flush_point();
        Ok(())
    }

    fn resume_frame(&mut self) -> Option<&mut ResumeFrame> {
        self.frame.as_mut().and_then(|frame| frame.resume.as_mut())
    }

    /// Whether the code being lowered may make a host request: a process
    /// kernel, or a unit that suspends the process for it.
    pub(super) fn can_request(&self) -> bool {
        self.kernel.is_some()
            || self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.resume.is_some())
    }

    /// Make host request `site` of family `kind`, which the lowering just
    /// added to [`Self::host_request_sites`]: suspend the process with it,
    /// and continue when the host has served it.
    pub(super) fn request_host(
        &mut self,
        kind: celox_design::HostRequestKind,
        site: u32,
    ) -> Result<(), sv::AnalyzerError> {
        if self.kernel.is_some() {
            return self
                .with_kernel(|kernel| kernel.host_request(kind, site))
                .map_err(kernel_error);
        }
        // In a unit, the site is one of the module's function sites, and
        // its values pass through staging variables (see
        // [`ResumeFrame::staging`]).
        let mut request = self
            .host_request_sites
            .pop()
            .filter(|request| request.kind == kind)
            .ok_or_else(|| unsupported("a host request without its site"))?;
        let mut staged = Vec::new();
        for value in request.args.iter_mut().chain(&mut request.results) {
            let variable = self.m.var(value.id).clone();
            let (stage, _) = self.m.temp(
                "call_stage",
                variable.width,
                variable.signed,
                variable.is_4state,
            );
            self.m
                .variables
                .get_mut(&stage)
                .expect("staging")
                .value_kind = variable.value_kind;
            self.copy_variable(value.id, stage);
            staged.push((value.id, stage));
            value.id = stage;
        }
        let results: Vec<(SourceVarId, SourceVarId)> = staged[request.args.len()..].to_vec();
        if let Some(resume) = self.resume_frame() {
            resume
                .staging
                .extend(staged.iter().map(|&(_, stage)| stage));
        }
        let number = self
            .m
            .function_host_request_sites
            .iter()
            .filter(|known| known.kind == kind)
            .count() as u32;
        self.m.function_host_request_sites.push(request);
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::HostRequest.code()),
        ));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            word,
            SIRValue::new(kind.encode(celox_design::FUNCTION_SITE_NUMBER | number)),
        ));
        self.suspend_unit(status, word, None)?;
        for (id, stage) in results {
            self.copy_variable(stage, id);
        }
        Ok(())
    }

    /// Suspend the unit being lowered with `status` and the delay word
    /// `request`, as a kernel suspends itself: for a delay, `$stop` or
    /// `$finish`. The open block afterwards is the resume point.
    pub(super) fn suspend_unit_with(
        &mut self,
        status: celox_design::ProcessStatus,
        request: RegisterId,
    ) -> Result<(), sv::AnalyzerError> {
        let code = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(code, SIRValue::new(status.code())));
        self.suspend_unit(code, request, None).map(drop)
    }

    /// `fork` in the unit being lowered (IEEE 1800-2023 9.3.2): a
    /// suspension with [`celox_design::ProcessStatus::Fork`] whose entries
    /// are resume points of the unit, one per branch. The runtime starts each
    /// child in a copy of the frames with the unit's resume slot set to its
    /// entry, whose address the word carries (see
    /// [`celox_design::PROCESS_FORK_RESUME_SHIFT`]); the parent continues
    /// at the open block afterwards. A branch that runs to its end ends its
    /// process.
    pub(super) fn fork_in_unit(
        &mut self,
        join: celox_design::ForkJoin,
        branches: &[Vec<sv::ir::Stmt>],
    ) -> Result<(), sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let first = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a fork in a unit that cannot suspend"))?
            .blocks
            .len();
        if first + branches.len() > celox_frontend_core::process::MAX_PROCESS_SUSPENSIONS {
            return Err(unsupported("a function with too many suspension points"));
        }
        let entries: Vec<BlockId> = branches.iter().map(|_| self.b.new_block()).collect();
        self.resume_frame()
            .expect("checked above")
            .blocks
            .extend(entries.iter().copied());
        // The frame is laid out in the order of its slots, and the resume
        // slot follows slots of fixed shape, so its offset is known.
        let functions = self.m.out_of_line().expect("lowering out of line");
        let function = &functions.functions[own];
        let slot = function.resume.expect("a resumable unit");
        let offset = layout(&function.slots[..=slot]).0[slot];
        let frame_base = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Load(
            frame_base,
            stable(base),
            SIROffset::Static(0),
            32,
        ));
        let high = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Imm(high, SIRValue::new(0u32)));
        let current = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Concat(current, vec![high, frame_base]));
        let mode = match join {
            celox_design::ForkJoin::All => 0u64,
            celox_design::ForkJoin::Any => 1,
            celox_design::ForkJoin::None => 2,
        };
        let fixed = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Imm(
            fixed,
            SIRValue::new(
                first as u64
                    | ((branches.len() as u64) << 16)
                    | (mode << 32)
                    | ((offset as u64) << celox_design::PROCESS_FORK_RESUME_SHIFT),
            ),
        ));
        let shift = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Imm(
            shift,
            SIRValue::new(u64::from(celox_design::PROCESS_FORK_RESUME_SHIFT)),
        ));
        let address = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Binary(
            address,
            current,
            BinaryOp::Shl,
            shift,
        ));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(word, fixed, BinaryOp::Add, address));
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Fork.code()),
        ));
        self.suspend_unit(status, word, None)?;
        // The parent's continuation, empty until the branches are lowered.
        let continuation = self.b.seal_block(SIRTerminator::Return);
        // A branch runs in a process of its own: no loop or subroutine of
        // the parent encloses it.
        let loops = std::mem::take(&mut self.loops);
        let functions = std::mem::take(&mut self.functions);
        let overlay = std::mem::take(&mut self.overlay);
        let timing_boundary = self.timing_call_boundary;
        self.timing_call_boundary = self.active_calls.len();
        let mut result = Ok(());
        for (entry, branch) in entries.into_iter().zip(branches) {
            self.b.switch_to_block(entry);
            match self.exec_block(branch) {
                Ok(true) => {
                    let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
                    self.b
                        .emit(SIRInstruction::Imm(request, SIRValue::new(0u64)));
                    if let Err(error) =
                        self.suspend_unit_with(celox_design::ProcessStatus::Done, request)
                    {
                        result = Err(error);
                        break;
                    }
                    // The process ended: its resume point is never taken.
                    self.b.seal_block(SIRTerminator::Return);
                }
                Ok(false) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        self.timing_call_boundary = timing_boundary;
        self.loops = loops;
        self.functions = functions;
        self.overlay = overlay;
        // A branch that failed leaves its block open; the unit is dropped.
        if result.is_ok() {
            self.b.switch_to_block(continuation);
        }
        result
    }

    /// `body` of `subroutine`, being lowered into a unit, with the variables
    /// a fork in it shares in boxes (see [`SharedBox`]), which it pushes to
    /// [`Ff::shared_boxes`]; the boxes of the formals are made here. A
    /// variable is shared when a branch names it, other than one the branch
    /// declares, and the body writes it besides its declaration: one that no
    /// one writes after the fork reads the same in every copy. Returns the
    /// number of boxes pushed.
    pub(super) fn share_forked(
        &mut self,
        subroutine: &sv::ir::Subroutine,
        mut body: Vec<sv::ir::Stmt>,
    ) -> Result<(Vec<sv::ir::Stmt>, usize), sv::AnalyzerError> {
        use super::super::procedural::expr_idents;
        if self.frame.is_none() || self.kernel.is_some() {
            return Ok((body, 0));
        }
        let mut branches: Vec<&Vec<sv::ir::Stmt>> = Vec::new();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Fork {
                    branches: forked, ..
                } = stmt
                {
                    branches.extend(forked);
                }
            });
        }
        // The variables the body passes to a `ref` formal that a fork of
        // the callee shares, which the callee's children reach through the
        // place the call passes: the variable is shared as its own would be
        // (IEEE 1800-2023 13.5.2).
        let mut passed = HashSet::default();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                let mut calls = Vec::new();
                stmt_calls(stmt, &mut calls);
                for (callee, args) in calls {
                    let sv::ir::Callee::Subroutine(callee) = callee else {
                        continue;
                    };
                    for (position, arg) in args.iter().enumerate() {
                        // An element of an array reaches the array: the
                        // variable is shared whatever part the call binds.
                        if let Some(name) = arg.as_ref().and_then(actual_root)
                            && forks_through_ref(self.m, &callee, position)
                        {
                            passed.insert(name.to_string());
                        }
                    }
                }
            });
        }
        if branches.is_empty() && passed.is_empty() {
            return Ok((body, 0));
        }
        let lvalue_idents = |lvalue: &sv::ir::LValue, names: &mut HashSet<String>| {
            names.insert(lvalue.name().to_string());
            if let sv::ir::LValue::Select { name, msb, lsb, .. } = lvalue {
                expr_idents(
                    &sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                        msb: msb.clone(),
                        lsb: lsb.clone(),
                        signed: false,
                    },
                    names,
                );
            }
        };
        let mut named = HashSet::default();
        for branch in branches {
            let mut declared = HashSet::default();
            let mut names = HashSet::default();
            for stmt in branch {
                stmt.walk(&mut |stmt| {
                    if let sv::ir::Stmt::Local { name, .. } = stmt {
                        declared.insert(name.clone());
                    }
                });
                let mut stmt = stmt.clone();
                let mut targets = HashSet::default();
                stmt.visit_mut(
                    &mut |expr| expr_idents(expr, &mut names),
                    &mut |lvalue| lvalue_idents(lvalue, &mut targets),
                    &mut |_| {},
                );
                names.extend(targets);
            }
            named.extend(names.into_iter().filter(|name| !declared.contains(name)));
        }
        // What the body writes, but by declarations: by assignments, and
        // through the arguments that the tasks and subroutines it calls write.
        let written = self.m.written_names(&body);
        let mut shared: Vec<&String> = automatic_names(subroutine)
            .filter(|name| {
                named.contains(*name) && written.contains(*name) || passed.contains(*name)
            })
            .collect();
        shared.sort_unstable();
        shared.dedup();
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let constant = |position: &sv::ir::ConstExpr| {
            sv::typecheck::eval_const_expr_with_types(position, constants, parameter_types)
        };
        let mut boxes = Vec::new();
        for name in shared {
            let param = subroutine.params.iter().find(|param| param.name == *name);
            if param.is_some_and(|param| param.direction.is_written())
                || subroutine.return_var.as_ref() == Some(name)
            {
                return Err(unsupported(format!(
                    "fork sharing the output `{name}` of `{}`",
                    subroutine.name
                )));
            }
            let id = self
                .m
                .id(name)
                .ok_or_else(|| unsupported(format!("variable `{name}`")))?;
            let variable = self.m.var(id);
            if !variable.array_dims.is_empty() {
                return Err(unsupported(format!(
                    "fork sharing the unpacked array `{name}` of `{}`",
                    subroutine.name
                )));
            }
            let field = sv::ir::ObjectField {
                offset: 0,
                width: variable.width as u32,
                signed: variable.signed,
                handle: match variable.value_kind {
                    celox_design::ValueKind::Bits => None,
                    celox_design::ValueKind::StrRef => Some(sv::ir::HandleProperty::String),
                    celox_design::ValueKind::ObjRef => Some(sv::ir::HandleProperty::Object),
                },
                four_state: variable.is_4state,
                nullable: false,
                real: variable.float.is_some(),
                key: 0,
            };
            let (handle_id, handle) =
                self.m
                    .temp("fork_box", celox_design::HANDLE_WIDTH, false, false);
            self.m
                .variables
                .get_mut(&handle_id)
                .expect("a box handle")
                .value_kind = celox_design::ValueKind::ObjRef;
            let alias = super::references::Alias {
                formal: name.clone(),
                source_name: name.clone(),
                target: super::references::Target::Property {
                    handle: handle.clone(),
                    field,
                },
            };
            body = alias.rewrite(body, &constant, &mut |field| self.part_scratch(field))?;
            boxes.push((
                param.is_some(),
                SharedBox {
                    var: name.clone(),
                    handle,
                    field,
                },
            ));
        }
        // A variable the body does not declare, a formal or a temporary the
        // analyzer made, gets its box at the entry; one it declares, each
        // time the declaration runs.
        let mut declared = HashSet::default();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Local { name, .. } = stmt {
                    declared.insert(name.clone());
                }
            });
        }
        let count = boxes.len();
        for (formal, shared) in boxes {
            if formal || !declared.contains(&shared.var) {
                self.open_box(&shared)?;
            }
            self.shared_boxes.push(shared);
        }
        Ok((body, count))
    }

    /// Make the box of `shared` (see [`SharedBox`]) holding the variable's
    /// value. The box is no class object: its creation draws on no RNG.
    pub(super) fn open_box(&mut self, shared: &SharedBox) -> Result<(), sv::AnalyzerError> {
        use celox_heap::ops;
        let size = self.constant64(u64::from(shared.field.storage()));
        let object = self.heap_call(
            ops::encode(ops::OBJECT_NEW, u64::from(celox_heap::BOX_CLASS)),
            &[size],
        );
        if let Some(kind) = shared.field.handle {
            let offset = self.constant64(0);
            let kind = self.constant64(match kind {
                sv::ir::HandleProperty::String => 0,
                sv::ir::HandleProperty::Object => 1,
            });
            self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
        }
        let handle = self.b.alloc_bit(celox_design::HANDLE_WIDTH, false);
        self.b.emit(SIRInstruction::Slice(
            handle,
            object,
            0,
            celox_design::HANDLE_WIDTH,
        ));
        let handle_id = self
            .m
            .id(&shared.handle)
            .ok_or_else(|| unsupported("a box handle"))?;
        self.store(
            handle_id,
            SIROffset::Static(0),
            celox_design::HANDLE_WIDTH,
            handle,
        );
        self.exec(&sv::ir::Stmt::Eval(sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(Box::new(sv::ir::ObjectOp::Store(shared.field))),
            args: vec![
                Some(sv::ir::Expr::Ident(shared.handle.clone())),
                Some(sv::ir::Expr::Ident(shared.var.clone())),
            ],
            arg_order: Vec::new(),
        }))?;
        Ok(())
    }

    /// Store variable `from` into variable `to`, of the same shape.
    pub(super) fn copy_variable(&mut self, from: SourceVarId, to: SourceVarId) {
        let variable = self.m.var(from);
        let width = variable.width;
        let value = if variable.is_4state && self.m.four_state {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, variable.signed)
        };
        self.b.emit(SIRInstruction::Load(
            value,
            stable(from),
            SIROffset::Static(0),
            width,
        ));
        self.store(to, SIROffset::Static(0), width, value);
    }

    /// Suspend the unit being lowered with `status` and the delay word
    /// `request`, leaving `top` as the end of the innermost live frame, its
    /// own when `None`. The open block afterwards is the resume point,
    /// whose value it returns.
    fn suspend_unit(
        &mut self,
        status: RegisterId,
        request: RegisterId,
        top: Option<RegisterId>,
    ) -> Result<u32, sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let resume = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a suspension in a unit that cannot suspend"))?;
        let (resume_var, status_var, request_var, top_var) =
            (resume.resume, resume.status, resume.request, resume.top);
        let point = resume.blocks.len();
        if point > celox_frontend_core::process::MAX_PROCESS_SUSPENSIONS {
            return Err(unsupported("a function with too many suspension points"));
        }
        let top = match top {
            Some(top) => top,
            None => self.own_top(base, own),
        };
        let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
        let point_value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            point_value,
            SIRValue::new(point as u64),
        ));
        self.store(resume_var, SIROffset::Static(0), width, point_value);
        self.store(
            status_var,
            SIROffset::Static(0),
            celox_design::PROCESS_STATUS_WIDTH,
            status,
        );
        self.store(
            request_var,
            SIROffset::Static(0),
            celox_design::PROCESS_DELAY_WIDTH,
            request,
        );
        self.store(
            top_var,
            SIROffset::Static(0),
            celox_design::PROCESS_CALL_TOP_WIDTH,
            top,
        );
        self.b.seal_block(SIRTerminator::Return);
        let block = self.b.new_block();
        self.resume_frame()
            .expect("checked above")
            .blocks
            .push(block);
        self.b.switch_to_block(block);
        Ok(point as u32)
    }

    /// Move the frame variables of function `index` into its frame, add
    /// the temporaries its body created (`start` and up) to the frame, and
    /// check that the unit touches no other state than it may.
    fn finish_function(
        &mut self,
        index: usize,
        mut unit: ExecutionUnit<Addr>,
        size_patches: Vec<(RegisterId, usize)>,
        start: SourceVarId,
        staging: &HashSet<SourceVarId>,
        referents: &HashSet<SourceVarId>,
        single: bool,
    ) -> Result<(), Reason> {
        let end = self.m.next_id;
        let functions = self.m.out_of_line().expect("lowering out of line");
        let mut slots = functions.functions[index].slots.clone();
        let mut accessed = Vec::new();
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                accessed.extend(instruction.memory_read().map(|access| access.addr.var_id));
                accessed.extend(instruction.memory_write().map(|access| access.addr.var_id));
            }
        }
        accessed.sort_unstable();
        accessed.dedup();
        // The automatic storage of the subroutines inlined into the body:
        // each activation needs its own as much as the function's.
        let automatic = self.m.automatic_storage();
        // The automatic arrays that stay module variables.
        let mut in_place = HashSet::default();
        for id in accessed {
            // The variable a `ref` formal names is the caller's.
            if slots.iter().any(|slot| slot.id == id)
                || staging.contains(&id)
                || referents.contains(&id)
            {
                continue;
            }
            let created = (start.0..end.0).contains(&id.0);
            if !created && !automatic.contains(&id) {
                continue;
            }
            let variable = self.m.var(id);
            if created && (!variable.hidden || !variable.path[0].contains("@t")) {
                return Err(Reason::Other(
                    "module state created in a function".to_string(),
                ));
            }
            if !variable.array_dims.is_empty() {
                // The automatic arrays of the bodies the unit inlines too:
                // they are live only within an activation of the unit.
                if single && !created {
                    in_place.insert(id);
                    continue;
                }
                array_slot(variable)?;
            }
            slots.push(self.slot_of(id));
        }
        // A function may write the module's variables, static storage of
        // subroutines and static class properties among them, which every
        // call shares, inlined or not; and the variables its `ref` formals
        // name, which the caller passes, such as the locals of a process
        // (the place selects the function, so only calls passing them reach
        // it). Not the other hidden ones the lowering keeps for a process,
        // nor what is observed: assembly instruments the stores into that,
        // which only the stores of the units the runtime runs can be.
        let persistent = self.m.static_storage();
        let frame: HashMap<SourceVarId, (usize, SourceVarId, Option<usize>)> = slots
            .iter()
            .map(|slot| (slot.id, (slot.width, slot.address, slot.element_width)))
            .collect();
        let mut reads = HashSet::default();
        let mut writes = HashSet::default();
        let mut callees = Vec::new();
        let other = |what: &str| Reason::Other(what.to_string());
        for block in unit.blocks.values_mut() {
            for instruction in &mut block.instructions {
                let (addr, offset, width, store) = match instruction {
                    SIRInstruction::Load(_, addr, offset, width) => (addr, &*offset, *width, false),
                    SIRInstruction::Store(addr, offset, width, _, triggers, captures) => {
                        if !triggers.is_empty() || !captures.is_empty() {
                            return Err(other("instrumented store"));
                        }
                        (addr, &*offset, *width, true)
                    }
                    SIRInstruction::Commit(..) => return Err(other("commit")),
                    SIRInstruction::RuntimeEvent { .. } => continue,
                    SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                        return Err(Reason::HostRequest);
                    }
                    SIRInstruction::ExternCall { func, .. }
                        if *func != celox_sir::extern_abi::HEAP_HELPER_ID =>
                    {
                        return Err(Reason::HostRequest);
                    }
                    SIRInstruction::Call { function, .. } => {
                        callees.push(function.0 as usize);
                        continue;
                    }
                    instruction if instruction.accesses_unknown_state() => {
                        return Err(other("kernel call"));
                    }
                    _ => continue,
                };
                if let Some(&(slot_width, address, element)) = frame.get(&addr.var_id) {
                    // A slot holding an array is also accessed at positions
                    // computed at run time, within one element, as the
                    // lowering accesses an array of the state.
                    match (offset, element) {
                        (SIROffset::Static(offset), _)
                        | (
                            SIROffset::PackedElements {
                                bit_offset: offset, ..
                            },
                            Some(_),
                        ) if offset + width <= slot_width => {}
                        (
                            SIROffset::Element {
                                element_width,
                                bit_offset,
                                dynamic_bit_offset,
                                ..
                            },
                            Some(element),
                        ) if *element_width == element
                            && width <= element
                            && (dynamic_bit_offset.is_some() || bit_offset + width <= element) => {}
                        (SIROffset::Dynamic(_), Some(element)) if width <= element => {}
                        _ => return Err(other("dynamic frame access")),
                    }
                    *addr = frame_addr(address);
                } else if addr.region != STABLE_REGION {
                    return Err(other("non-stable state access"));
                } else if store {
                    if self.m.is_hidden(addr.var_id)
                        && !persistent.contains(&addr.var_id)
                        && !staging.contains(&addr.var_id)
                        && !in_place.contains(&addr.var_id)
                        && !referents.contains(&addr.var_id)
                    {
                        return Err(other("writes hidden module state"));
                    }
                    if self.m.is_excluded(addr.var_id) {
                        return Err(other("stores into observed storage"));
                    }
                    writes.insert(addr.var_id);
                } else {
                    reads.insert(addr.var_id);
                }
            }
        }
        let (offsets, size) = layout(&slots);
        if size > celox_sir::STACK_SIZE {
            return Err(other("frame larger than the stack"));
        }
        let functions = self.m.out_of_line().expect("lowering out of line");
        let function = &mut functions.functions[index];
        function.slots = slots;
        function.offsets = offsets;
        function.size = size;
        function.reads = reads;
        function.writes = writes;
        function.callees = callees;
        function.size_patches = size_patches;
        function.unit = Some(unit);
        Ok(())
    }

    /// A hidden variable of the shape of `id`, which receives a value that
    /// a call leaves in the slot of `id`. A call made in a function unit
    /// thereby keeps it in its own frame, apart from the callee's.
    pub(super) fn call_result_temp(&mut self, id: SourceVarId) -> (SourceVarId, String) {
        let variable = self.m.var(id).clone();
        let (copy, name) = self.m.temp(
            "call_result",
            variable.width,
            variable.signed,
            variable.is_4state,
        );
        self.m.variables.insert(
            copy,
            SvVariable {
                path: vec![name.clone()],
                kind: VariableKind::Variable,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                ..variable
            },
        );
        (copy, name)
    }

    /// Call function `index`, the unit of `subroutine`: pass `inputs`, the
    /// values of the formals the caller writes, store each `(formal, temp)`
    /// of `copies` from the formal's slot after the call, and return the
    /// variable that holds the result.
    pub(super) fn call_out_of_line(
        &mut self,
        index: usize,
        subroutine: &sv::ir::Subroutine,
        inputs: Vec<(SourceVarId, Result<RegisterId, String>)>,
        copies: &[(SourceVarId, SourceVarId)],
    ) -> Result<Option<String>, sv::AnalyzerError> {
        let function = self.m.out_of_line().expect("calling out of line").functions[index].clone();
        let mut args = Vec::new();
        for (id, value) in inputs {
            let value = match value {
                Ok(value) => value,
                Err(name) => self.eval(&sv::ir::Expr::Ident(name), None)?,
            };
            match function.slot(self.m.declared(id)) {
                Some(slot) => args.push((slot, value)),
                // A static formal whose value persists: the callee reads
                // the module variable, as an inlined body would.
                None => {
                    let width = self.m.var(id).width;
                    let offset = sv_memory_offset(self.m.var(id), 0, width);
                    self.store(id, offset, width, value);
                }
            }
        }
        let (base, depth) = self.call_frame(index, &subroutine.name, &function)?;
        args.insert(0, (0, base));
        args.insert(1, (1, depth));
        let result = subroutine.return_var.as_ref().and_then(|name| {
            let id = self.m.id(name)?;
            Some((id, function.slot(self.m.declared(id))))
        });
        let mut outputs: Vec<(usize, SourceVarId)> = Vec::new();
        let mut result_name = None;
        if let Some((id, slot)) = result {
            match slot {
                Some(slot) => {
                    let (temp, name) = self.call_result_temp(id);
                    outputs.push((slot, temp));
                    result_name = Some(name);
                }
                None => result_name = subroutine.return_var.clone(),
            }
        }
        for &(formal, temp) in copies {
            let slot = function
                .slot(self.m.declared(formal))
                .ok_or_else(|| unsupported("output formal without a frame slot"))?;
            outputs.push((slot, temp));
        }
        // The calling process's RNG states come back: at the end of the
        // call, or when it suspends the process, which then holds them
        // until it continues the call.
        if let Some((state_slot, _)) = function.random {
            let (state, _) = self.urandom_state();
            outputs.push((state_slot, state));
        }
        if let Some(slot) = function.dist_seed {
            let (seed, _) = self.dist_seed();
            outputs.push((slot, seed));
        }
        args.extend(self.process_args(&function)?);
        // A resumable unit starts its body, and leaves whether it suspended.
        let mut control = Vec::new();
        if let Some(resume) = function.resume {
            let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
            let start = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Imm(start, SIRValue::new(0u8)));
            args.push((resume, start));
            for (offset, purpose, width) in [
                (1, "call_status", celox_design::PROCESS_STATUS_WIDTH),
                (2, "call_request", celox_design::PROCESS_DELAY_WIDTH),
                (3, "call_top", celox_design::PROCESS_CALL_TOP_WIDTH),
            ] {
                let (temp, _) = self.m.temp(purpose, width, false, false);
                outputs.push((resume + offset, temp));
                control.push((temp, width));
            }
        }
        self.emit_call(index, &function, base, args, &outputs);
        if let [status, request, top] = control[..] {
            // A suspended call suspends the caller, which continues it by
            // calling again: the callee's frame keeps its formals and its
            // resume point, and only the process state goes in again.
            let check = self.b.new_block();
            self.jump(check);
            self.b.switch_to_block(check);
            let load = |b: &mut SIRBuilder<Addr>, (id, width): (SourceVarId, usize)| {
                let register = b.alloc_bit(width, false);
                b.emit(SIRInstruction::Load(
                    register,
                    stable(id),
                    SIROffset::Static(0),
                    width,
                ));
                register
            };
            let code = load(&mut self.b, status);
            let zero = self.b.alloc_bit(status.1, false);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            let suspended = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(suspended, code, BinaryOp::Ne, zero));
            let suspend = self.b.new_block();
            let done = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: suspended,
                true_block: (suspend, Vec::new()),
                false_block: (done, Vec::new()),
            });
            self.b.switch_to_block(suspend);
            let code = load(&mut self.b, status);
            let mut word = load(&mut self.b, request);
            let end = load(&mut self.b, top);
            // A unit waits at a function wait site, and makes a nonblocking
            // assignment at a function NBA site, which the process lists
            // after the kernel's own sites.
            let state = self.m.process_state(&subroutine.name);
            if self.kernel.is_some() && (state.waits || state.nbas) {
                word = self.function_site_word(code, word, state.waits, state.nbas);
            }
            if self.kernel.is_some() {
                let call_top = match self.call_top {
                    Some(id) => id,
                    None => {
                        let (id, _) = self.m.temp(
                            "call_top",
                            celox_design::PROCESS_CALL_TOP_WIDTH,
                            false,
                            false,
                        );
                        self.call_top = Some(id);
                        id
                    }
                };
                let point = self
                    .with_kernel(|kernel| kernel.suspend_in_call(code, word, end, call_top))
                    .map_err(kernel_error)?;
                self.call_resumes.push(celox_design::CallResume {
                    point,
                    function: index as u32,
                });
            } else {
                let point = self.suspend_unit(code, word, Some(end))?;
                self.resume_frame()
                    .expect("a unit that suspends is resumable")
                    .calls
                    .push(celox_design::CallResume {
                        point,
                        function: index as u32,
                    });
            }
            let (base, depth) = self.call_frame(index, &subroutine.name, &function)?;
            let mut args = vec![(0, base), (1, depth)];
            args.extend(self.process_args(&function)?);
            self.emit_call(index, &function, base, args, &outputs);
            self.jump(check);
            self.b.switch_to_block(done);
        }
        Ok(result_name)
    }

    /// The values of the calling process's state that a call of `function`
    /// passes (see [`Function::random`]): its RNG states and seed, as an
    /// inlined body would draw on them.
    fn process_args(
        &mut self,
        function: &Function,
    ) -> Result<Vec<(usize, RegisterId)>, sv::AnalyzerError> {
        let mut args = Vec::new();
        if let Some((state_slot, seed_slot)) = function.random {
            let (_, name) = self.urandom_state();
            let value = self.eval(&sv::ir::Expr::Ident(name), None)?;
            args.push((state_slot, value));
            let seed = self.process_seed();
            let seed = self.eval(&seed, Some((32, false)))?;
            args.push((seed_slot, seed));
        }
        if let Some(slot) = function.dist_seed {
            let (_, name) = self.dist_seed();
            let value = self.eval(&sv::ir::Expr::Ident(name), None)?;
            args.push((slot, value));
        }
        Ok(args)
    }

    /// Call function `index` in the frame at `base` with `args`, and store
    /// each `(slot, variable)` of `outputs` from the slot when it returns.
    fn emit_call(
        &mut self,
        index: usize,
        function: &Function,
        base: RegisterId,
        args: Vec<(usize, RegisterId)>,
        outputs: &[(usize, SourceVarId)],
    ) {
        let width: usize = outputs
            .iter()
            .map(|&(slot, _)| function.slots[slot].width)
            .sum();
        let dst = (!outputs.is_empty()).then(|| {
            if outputs
                .iter()
                .any(|&(slot, _)| function.slots[slot].four_state)
            {
                self.b.alloc_logic(width)
            } else {
                self.b.alloc_bit(width, false)
            }
        });
        self.b.emit(SIRInstruction::Call {
            function: celox_sir::FunctionId(index as u32),
            frame: base,
            args,
            outputs: outputs.iter().map(|&(slot, _)| slot).collect(),
            dst,
            effects: Default::default(),
        });
        if let Some(dst) = dst {
            // The first output is the least significant part.
            let mut offset = 0;
            for &(slot, temp) in outputs {
                let slot_width = function.slots[slot].width;
                let part = if function.slots[slot].four_state && self.m.var(temp).is_4state {
                    self.b.alloc_logic(slot_width)
                } else {
                    self.b.alloc_bit(slot_width, false)
                };
                self.b
                    .emit(SIRInstruction::Slice(part, dst, offset, slot_width));
                let variable = self.m.var(temp);
                let store_offset = sv_memory_offset(variable, 0, variable.width);
                self.store(temp, store_offset, slot_width, part);
                offset += slot_width;
            }
        }
    }

    /// The frame base and call depth of a call of function `index`, and the
    /// checks that the call fits the stack and the depth limit, which raise
    /// a runtime error otherwise. A unit run by the runtime calls with an
    /// empty stack; a function calls in the frame after its own.
    fn call_frame(
        &mut self,
        index: usize,
        name: &str,
        callee: &Function,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        let word = |b: &mut SIRBuilder<Addr>, value: u64| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        };
        let Some(frame) = &self.frame else {
            let base = word(&mut self.b, 0);
            let depth = word(&mut self.b, 1);
            return Ok((base, depth));
        };
        let (own, base_var, depth_var) = (frame.function, frame.base, frame.depth);
        let load = |b: &mut SIRBuilder<Addr>, id| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Load(
                register,
                stable(id),
                SIROffset::Static(0),
                32,
            ));
            register
        };
        let add = |b: &mut SIRBuilder<Addr>, left, right| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Binary(register, left, BinaryOp::Add, right));
            register
        };
        let current = load(&mut self.b, base_var);
        let own_size = word(&mut self.b, 0);
        let base = add(&mut self.b, current, own_size);
        let current_depth = load(&mut self.b, depth_var);
        let one = word(&mut self.b, 1);
        let depth = add(&mut self.b, current_depth, one);
        // A callee still being lowered (a recursive call) has no size yet.
        let size = word(&mut self.b, callee.size as u64);
        let frame = self.frame.as_mut().expect("lowering a function unit");
        frame.size_patches.push((own_size, own));
        if callee.unit.is_none() {
            frame.size_patches.push((size, index));
        }
        let end = add(&mut self.b, base, size);
        let limit = word(&mut self.b, celox_sir::STACK_SIZE as u64);
        let overflow = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(overflow, limit, BinaryOp::LtU, end));
        self.raise_if(overflow, format!("stack overflow in a call of `{name}`"));
        let limit = word(&mut self.b, celox_sir::MAX_CALL_DEPTH as u64);
        let deep = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(deep, limit, BinaryOp::LtU, depth));
        self.raise_if(
            deep,
            format!(
                "call depth exceeds {} in a call of `{name}`",
                celox_sir::MAX_CALL_DEPTH
            ),
        );
        Ok((base, depth))
    }

    /// End the run with a runtime error reporting `message` when `condition`
    /// holds, and continue in a new block otherwise.
    fn raise_if(&mut self, condition: RegisterId, message: String) {
        let error = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (error, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(error);
        let code = self.m.runtime_error(message);
        self.b.seal_block(SIRTerminator::Error(code));
        self.b.switch_to_block(next);
    }
}

/// Check that unpacked array `variable` can be a slot of a frame: one of
/// bits, which the collector need not scan element by element.
fn array_slot(variable: &SvVariable) -> Result<(), Reason> {
    if variable.value_kind != celox_design::ValueKind::Bits {
        return Err(Reason::Other(
            "unpacked array of handles in the frame".to_string(),
        ));
    }
    Ok(())
}

/// Whether formal `position` of subroutine `name` is a `ref` formal that a
/// branch of a fork in its body writes (see `ProcModule::written_names`),
/// also by passing it on to such a formal of another subroutine: the
/// children of that fork, which run in copies of the frames, then write the
/// caller's variable through it. A child that only reads it reads the
/// caller's variable while the caller waits in the call: a `ref` formal
/// cannot be named in a fork the call can return before (IEEE 1800-2023
/// 9.3.2).
fn forks_through_ref(m: &ProcModule<'_>, name: &str, position: usize) -> bool {
    let mut writes = false;
    let mut pending = vec![(name.to_string(), position)];
    let mut visited = HashSet::default();
    while let Some((name, position)) = pending.pop() {
        if !visited.insert((name.clone(), position)) {
            continue;
        }
        let Some(subroutine) = m.subroutine(&name) else {
            continue;
        };
        let Some(param) = subroutine
            .params
            .get(position)
            .filter(|param| param.direction.is_reference())
        else {
            continue;
        };
        let formal = &param.name;
        for stmt in &subroutine.body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Fork { branches, .. } = stmt {
                    writes |= branches
                        .iter()
                        .any(|branch| m.written_names(branch).contains(formal));
                }
                let mut calls = Vec::new();
                stmt_calls(stmt, &mut calls);
                for (callee, args) in calls {
                    let sv::ir::Callee::Subroutine(callee) = callee else {
                        continue;
                    };
                    for (next, arg) in args.iter().enumerate() {
                        if arg.as_ref().and_then(actual_root) == Some(formal.as_str()) {
                            pending.push((callee.clone(), next));
                        }
                    }
                }
            });
        }
        if writes {
            return true;
        }
    }
    false
}

/// The variable a `ref` actual names, whole or in part: an element or
/// another select of it.
fn actual_root(actual: &sv::ir::Expr) -> Option<&str> {
    match actual {
        sv::ir::Expr::Ident(name) => Some(name),
        sv::ir::Expr::Select { expr, .. } => actual_root(expr),
        _ => None,
    }
}

fn binary(left: sv::ir::Expr, op: sv::ir::BinaryOp, right: sv::ir::Expr) -> sv::ir::Expr {
    sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

/// Whether the value of an event expression went from that of variable
/// `before` to that of variable `after` by an occurrence of `edge` (IEEE
/// 1800-2023 9.4.2): any change of the value, or an edge of its least
/// significant bit by Table 9-2, where a value of x or z is neither 0
/// nor 1.
fn edge_occurred(before: &str, after: &str, edge: sv::ir::EventEdge) -> sv::ir::Expr {
    use sv::ir::BinaryOp::{EqCase, LogicAnd, LogicOr, NeCase};
    let ident = |name: &str| sv::ir::Expr::Ident(name.to_string());
    let bit = |name: &str| sv::ir::Expr::Select {
        expr: Box::new(ident(name)),
        msb: sv::ir::ConstExpr::Literal("0".into()),
        lsb: sv::ir::ConstExpr::Literal("0".into()),
        signed: false,
    };
    let is =
        |name: &str, op, value: &str| binary(bit(name), op, sv::ir::Expr::Literal(value.into()));
    // From `from` to anything else, or from neither to `to`.
    let toward = |from: &str, to: &str| {
        binary(
            binary(is(before, EqCase, from), LogicAnd, is(after, NeCase, from)),
            LogicOr,
            binary(
                binary(is(before, NeCase, from), LogicAnd, is(before, NeCase, to)),
                LogicAnd,
                is(after, EqCase, to),
            ),
        )
    };
    match edge {
        sv::ir::EventEdge::Any => binary(ident(before), NeCase, ident(after)),
        sv::ir::EventEdge::Pos => toward("1'b0", "1'b1"),
        sv::ir::EventEdge::Neg => toward("1'b1", "1'b0"),
        sv::ir::EventEdge::Both => binary(toward("1'b0", "1'b1"), LogicOr, toward("1'b1", "1'b0")),
    }
}

/// The most items whose `iff` reads only the frame that an event control
/// of a unit selects among by a site for each set of them (see
/// `Ff::unit_wait_event`), unless their conditions exclude each other.
const MAX_SELECTED_ITEMS: usize = 4;

/// Whether at most one of `conditions` can hold at a time: each compares
/// the same expression for equality with a constant, the constants'
/// values all distinct, as the conditions that the analyzer makes for the
/// instances a virtual interface may denote are. `value` evaluates a
/// constant; a condition whose constant has no value makes them not
/// exclusive.
fn exclusive_selection<'a>(
    conditions: impl Iterator<Item = &'a sv::ir::Expr>,
    value: impl Fn(&sv::ir::Expr) -> Option<i128>,
) -> bool {
    let mut compared: Option<&sv::ir::Expr> = None;
    let mut values = HashSet::default();
    for condition in conditions {
        let sv::ir::Expr::Binary {
            left,
            op: sv::ir::BinaryOp::Eq,
            right,
        } = condition
        else {
            return false;
        };
        let Some(constant) = value(right) else {
            return false;
        };
        if compared.is_some_and(|compared| compared != &**left) || !values.insert(constant) {
            return false;
        }
        compared = Some(left);
    }
    true
}

/// Whether the `iff` condition `iff` of an event item holds: a known
/// nonzero value, x and z not (IEEE 1800-2023 9.4.2.3), as one bit that a
/// kernel's gate and a unit's selection and samples alike take.
pub(super) fn iff_truth(iff: sv::ir::Expr) -> sv::ir::Expr {
    let not = |expr| sv::ir::Expr::Unary {
        op: sv::ir::UnaryOp::LogicNot,
        expr: Box::new(expr),
    };
    binary(
        not(not(iff)),
        sv::ir::BinaryOp::EqCase,
        sv::ir::Expr::Literal("1'b1".into()),
    )
}
