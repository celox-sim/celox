//! Hidden variables: the temporaries, copies and counters the lowering
//! creates, whose names are no SystemVerilog identifiers, and the
//! procedural locals.

use super::*;

/// The name of [`ProcModule::held_gate`], which is no SystemVerilog
/// identifier.
pub(in crate::lowering) const HELD_GATE: &str = "$event_held";

impl ProcModule<'_> {
    /// A fresh hidden variable of the given shape that holds a value of the
    /// activation that computes it, such as an operand kept across a
    /// suspension: in a process with a frame, each activation has its own
    /// (see `SvVariable::activation_scratch`). Its name cannot clash with a
    /// SystemVerilog identifier.
    pub fn temp(
        &mut self,
        purpose: &str,
        width: usize,
        signed: bool,
        is_4state: bool,
    ) -> (SourceVarId, String) {
        let (id, name) = self.module_temp(purpose, width, signed, is_4state);
        self.variables
            .get_mut(&id)
            .expect("a new temporary")
            .activation_scratch = true;
        (id, name)
    }

    /// A fresh hidden variable of the given shape that is the module's,
    /// shared by every activation of the process that makes it, such as a
    /// count other processes watch.
    pub fn module_temp(
        &mut self,
        purpose: &str,
        width: usize,
        signed: bool,
        is_4state: bool,
    ) -> (SourceVarId, String) {
        let name = format!("{purpose}@t{}", self.temp_counter);
        self.temp_counter += 1;
        let id = self.hidden(name.clone(), width, signed, is_4state);
        (id, name)
    }

    /// The shared generation counter of the named block `block` that a
    /// `disable` can leave: every activation of the block snapshots it on
    /// entry, and disabling the block advances it so each other active
    /// activation leaves at its next statement (IEEE 1800-2023 9.6.2).
    /// One variable serves every process and inlining of the block.
    pub fn disable_epoch(&mut self, block: u64) -> String {
        let name = format!("$disable_epoch_{block:x}");
        if self.id(&name).is_none() {
            self.hidden(name.clone(), 64, false, false);
        }
        name
    }

    /// The hidden variable through which the module's processes read the
    /// simulation time, which the scheduler stores into it
    /// (`RuntimeSchema::time_slots`). Its name is no SystemVerilog
    /// identifier.
    pub fn time_slot(&mut self) -> String {
        if !self.name_to_id.contains_key(TIME_SLOT) {
            self.hidden(TIME_SLOT.to_string(), TIME_SLOT_WIDTH, false, false);
        }
        TIME_SLOT.to_string()
    }

    /// The one-bit module variable that holds 1 (see [`HELD_GATE`]): the gate of an event item
    /// without `iff` whose occurrences a process counts itself (see
    /// `Ff::property_event`). Each wait that uses it stores the 1 before it
    /// suspends. It is no hidden variable, so the writes of a waited
    /// variable judge it, and its name is no SystemVerilog identifier.
    pub fn held_gate(&mut self) -> SourceVarId {
        if let Some(id) = self.id(HELD_GATE) {
            return id;
        }
        let id = self.hidden(HELD_GATE.to_string(), 1, false, false);
        self.variables.get_mut(&id).expect("the held gate").hidden = false;
        id
    }

    pub(super) fn hidden(
        &mut self,
        name: String,
        width: usize,
        signed: bool,
        is_4state: bool,
    ) -> SourceVarId {
        let id = next_var_id(&mut self.next_id);
        self.variables.insert(
            id,
            SvVariable {
                float: None,

                value_kind: celox_design::ValueKind::Bits,
                path: vec![name.clone()],
                width: width.max(1),
                signed,
                is_4state,
                is_unpacked_struct: false,
                packed_ranges: vec![(i128::try_from(width.max(1)).unwrap_or(1) - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: if is_4state {
                    PortTypeKind::Logic
                } else {
                    PortTypeKind::Bit
                },
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
        self.bind_name(name, id);
        self.created.push(id);
        id
    }

    /// A hidden variable with the shape of `id`, two-state when `two_state`.
    /// The hidden copy that holds the sampled value of variable `id`
    /// (IEEE 1800-2023 16.5.1), created on first use.
    pub fn sampled_copy(&mut self, id: SourceVarId) -> String {
        if let Some((name, _)) = self
            .name_to_id
            .iter()
            .find(|(_, copy)| self.variables[copy].sampled_from == Some(id))
        {
            return name.clone();
        }
        let (copy, name) = self.shadow(id, "sampled", false);
        self.variables
            .get_mut(&copy)
            .expect("a new copy")
            .sampled_from = Some(id);
        name
    }

    /// A new hidden register of `$past` history for variable `id`, which
    /// starts with the variable's value.
    pub fn history_copy(&mut self, id: SourceVarId) -> String {
        let (copy, name) = self.shadow(id, "past", false);
        self.variables
            .get_mut(&copy)
            .expect("a new copy")
            .history_of = Some(id);
        name
    }

    pub fn shadow(
        &mut self,
        id: SourceVarId,
        purpose: &str,
        two_state: bool,
    ) -> (SourceVarId, String) {
        let name = format!("{purpose}@t{}", self.temp_counter);
        self.temp_counter += 1;
        let mut variable = self.variables[&id].clone();
        variable.path = vec![name.clone()];
        variable.source = None;
        variable.hidden = true;
        variable.activation_scratch = false;
        variable.domain_kind = DomainKind::Other;
        if two_state {
            variable.is_4state = false;
            variable.type_kind = PortTypeKind::Bit;
        }
        let shadow = next_var_id(&mut self.next_id);
        self.variables.insert(shadow, variable);
        self.bind_name(name.clone(), shadow);
        self.created.push(shadow);
        (shadow, name)
    }

    /// A hidden one-bit copy of the event signal `signal`, sensed as `kind`.
    /// It has no name a SystemVerilog identifier can denote, and is not
    /// addressable by path.
    pub fn event_copy(&mut self, signal: SourceVarId, kind: DomainKind) -> SourceVarId {
        let source = &self.variables[&signal];
        let suffix = match kind {
            DomainKind::ClockPosedge => "posedge",
            DomainKind::ClockNegedge => "negedge",
            DomainKind::ResetAsyncHigh => "reset_high",
            DomainKind::ResetAsyncLow => "reset_low",
            DomainKind::Other => "event",
        };
        let mut path = source.path.clone();
        if let Some(last) = path.last_mut() {
            last.push('@');
            last.push_str(suffix);
        }
        let variable = SvVariable {
            float: None,

            value_kind: celox_design::ValueKind::Bits,
            path,
            width: 1,
            signed: false,
            is_4state: source.is_4state,
            is_unpacked_struct: false,
            packed_ranges: vec![(0, 0)],
            array_dims: Vec::new(),
            unpacked_ranges: Vec::new(),
            domain_kind: kind,
            kind: VariableKind::Variable,
            type_kind: source.type_kind,
            source: None,
            hidden: true,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        let id = next_var_id(&mut self.next_id);
        self.variables.insert(id, variable);
        id
    }

    /// A module-scope variable of event expression `index` with the shape
    /// of `variable`: `$event[index]` itself for `slot == None`, otherwise
    /// its copy `$event[index].slot`. Like a process slot it is no signal of
    /// the design, but state that persists while processes wait.
    pub fn event_variable(
        &mut self,
        index: usize,
        slot: Option<String>,
        variable: SvVariable,
    ) -> (SourceVarId, String) {
        let path: Vec<_> = std::iter::once(format!("{EVENT_PREFIX}{index}]"))
            .chain(slot)
            .collect();
        let name = path.join(".");
        let id = next_var_id(&mut self.next_id);
        self.variables.insert(
            id,
            SvVariable {
                path,
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
                ..variable
            },
        );
        self.bind_name(name.clone(), id);
        (id, name)
    }

    /// The index of the next event expression of the module.
    pub fn next_event_index(&self) -> usize {
        (0..)
            .take_while(|index| {
                self.name_to_id
                    .contains_key(&format!("{EVENT_PREFIX}{index}]"))
            })
            .count()
    }
}

/// Register the procedural locals of `module` as hidden variables.
pub(in crate::lowering) fn register_locals(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    next_id: &mut SourceVarId,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<(), sv::AnalyzerError> {
    for local in module.locals() {
        let type_info = signal_type_from_sv(&local.r#type, constants, parameter_types)?;
        let id = next_var_id(next_id);
        variables.insert(
            id,
            SvVariable {
                float: type_info.float,

                value_kind: type_info.value_kind,
                path: vec![local.source_name.clone()],
                width: type_info.width,
                signed: type_info.signed,
                is_4state: type_info.is_4state,
                is_unpacked_struct: type_info.is_unpacked_struct,
                packed_ranges: type_info.packed_ranges,
                array_dims: type_info.array_dims,
                unpacked_ranges: type_info.unpacked_ranges,
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: type_info.type_kind,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
        name_to_id.insert(local.name.clone(), id);
    }
    Ok(())
}

impl ProcModule<'_> {
    /// The counter that the stores of `field` bump, for the waits that read
    /// it: a watched class property's (see `ff::property_changes`) or a
    /// box's (see [`Self::box_changes`]).
    pub(in crate::lowering) fn change_counter(
        &self,
        field: &sv::ir::ObjectField,
    ) -> Option<SourceVarId> {
        match field.key {
            sv::ir::ChangeKey::None => None,
            sv::ir::ChangeKey::Property(key) => self.id(&super::super::ff::property_changes(key)),
            sv::ir::ChangeKey::Box(id) => self
                .box_changes
                .iter()
                .copied()
                .find(|counter| counter.0 == id),
        }
    }
}
