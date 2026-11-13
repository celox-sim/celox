//! Checkpoints of the lowering state: a lowering that fails is rolled back
//! so that the module lowers as if it had never been tried.

use super::*;

/// What [`ProcModule::rollback`] restores.
pub(in crate::lowering) struct Checkpoint {
    pub(super) next_id: SourceVarId,
    pub(super) created: usize,
    pub(super) temp_counter: usize,
    pub(super) capture_counter: u64,
    pub(super) runtime_event_sites: usize,
    pub(super) function_host_request_sites: usize,
    pub(super) function_wait_sites: usize,
    pub(super) event_assignments: usize,
    pub(super) activation_copies: usize,
    pub(super) box_changes: usize,
    pub(super) function_nba_sites: usize,
    pub(super) runtime_errors: usize,
    pub(super) extern_functions: usize,
    pub(super) extern_calls: usize,
    pub(super) warnings: usize,
    /// The out-of-line functions, which a discarded lowering may have added.
    pub(super) functions: usize,
}

impl Checkpoint {
    /// The number of runtime event sites at the checkpoint.
    pub fn runtime_event_sites(&self) -> usize {
        self.runtime_event_sites
    }
}

impl ProcModule<'_> {
    /// The lowering state, for [`Self::rollback`].
    pub fn checkpoint(&self) -> Checkpoint {
        // Every field, so that a new one cannot be left out: what a
        // lowering grows, which a rollback restores, and what it never
        // changes or only caches.
        let ProcModule {
            // Restored through `next_id`.
            variables: _,
            name_to_id: _,
            // Follows `name_to_id` (see `bind_name`).
            storage: _,
            next_id,
            constants: _,
            parameter_types: _,
            subroutines: _,
            constant_functions: _,
            dpi_imports: _,
            externs:
                Externs {
                    functions: extern_functions,
                    calls: extern_calls,
                },
            four_state: _,
            program: _,
            forceable: _,
            procedurally_forced: _,
            created,
            temp_counter,
            capture_counter,
            runtime:
                RuntimeReports {
                    event_sites: runtime_event_sites,
                    errors: runtime_errors,
                },
            function_sites:
                FunctionSites {
                    host_request: function_host_request_sites,
                    wait: function_wait_sites,
                    nba: function_nba_sites,
                    // Restored with `runtime_event_sites`.
                    flush: _,
                },
            box_changes,
            event_assignments,
            // Filled before any lowering.
            unpacked_bounds: _,
            warnings,
            calls,
            // Facts of subroutines, the same whatever was lowered.
            requesting_subroutines: _,
            activation_copies,
        } = self;
        Checkpoint {
            next_id: *next_id,
            created: created.len(),
            temp_counter: *temp_counter,
            capture_counter: *capture_counter,
            runtime_event_sites: runtime_event_sites.len(),
            function_host_request_sites: function_host_request_sites.len(),
            function_wait_sites: function_wait_sites.len(),
            function_nba_sites: function_nba_sites.len(),
            event_assignments: event_assignments.len(),
            activation_copies: activation_copies.len(),
            box_changes: box_changes.len(),
            runtime_errors: runtime_errors.len(),
            extern_functions: extern_functions.len(),
            extern_calls: *extern_calls,
            warnings: warnings.len(),
            functions: calls
                .as_deref()
                .and_then(|calls| calls.out_of_line.as_ref())
                .map_or(0, super::super::out_of_line::OutOfLine::len),
        }
    }

    /// Forget everything lowered since `checkpoint`, so that the module
    /// lowers as if it had never been.
    pub fn rollback(&mut self, checkpoint: Checkpoint) {
        let Checkpoint {
            next_id,
            created,
            temp_counter,
            capture_counter,
            runtime_event_sites,
            function_host_request_sites,
            function_wait_sites,
            function_nba_sites,
            event_assignments,
            activation_copies,
            box_changes,
            runtime_errors,
            extern_functions,
            extern_calls,
            warnings,
            functions,
        } = checkpoint;
        for id in next_id.0..self.next_id.0 {
            let Some(variable) = self.variables.remove(&SourceVarId(id)) else {
                continue;
            };
            if let Some(name) = variable.path.first()
                && self.name_to_id.get(name) == Some(&SourceVarId(id))
            {
                let name = name.clone();
                self.unbind_name(&name);
            }
        }
        self.next_id = next_id;
        self.created.truncate(created);
        self.temp_counter = temp_counter;
        self.capture_counter = capture_counter;
        self.runtime.event_sites.truncate(runtime_event_sites);
        self.box_changes.truncate(box_changes);
        // A flush site made since then is gone with it.
        if self
            .function_sites
            .flush
            .is_some_and(|site| site as usize >= runtime_event_sites)
        {
            self.function_sites.flush = None;
        }
        self.function_sites
            .host_request
            .truncate(function_host_request_sites);
        self.function_sites.wait.truncate(function_wait_sites);
        self.event_assignments.truncate(event_assignments);
        self.activation_copies.truncate(activation_copies);
        self.function_sites.nba.truncate(function_nba_sites);
        // Codes are numbered from one in registration order.
        self.runtime
            .errors
            .retain(|&code, _| code <= runtime_errors as i64);
        self.externs.functions.truncate(extern_functions);
        self.externs.calls = extern_calls;
        self.warnings.truncate(warnings);
        if let Some(out_of_line) = self.out_of_line() {
            out_of_line.truncate(functions);
        }
    }
}
