//! System task statements of a combinational process: memory loads, and
//! the observers of displays, messages and `$finish`.

use super::*;
use crate::lowering::ff::{converts_time, time_format_operand};

/// Combinational runtime events, their sites, and the inputs their
/// operands read.
#[derive(Default)]
pub(in crate::lowering) struct RuntimeEvents {
    pub observers: Vec<CombObserver<SourceVarId>>,
    pub sites: Vec<RuntimeEventSite>,
    pub(super) effect_sensitivity: Sources,
    /// The first module-wide site number of this process's runtime events.
    pub site_base: u32,
}

impl Comb<'_, '_> {
    /// `$readmemh`/`$readmemb`: the words of the file, written as constants.
    pub(super) fn readmem(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        format: sv::ir::MemoryFormat,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<(), sv::AnalyzerError> {
        let name = sv::ir::SystemTask::Readmem(format);
        let mut addresses = [None, None];
        for (slot, arg) in addresses.iter_mut().zip(args.iter().skip(2)) {
            let sv::ir::SystemTaskArg::Expr(expr) = arg else {
                continue;
            };
            let (node, _) = self.eval(store, frames, expr, None)?;
            let signed = self.expr_signed(expr);
            let (value, width) = slt_const(self.arena, &mut self.consts, node)
                .ok_or_else(|| unsupported(format!("`{name}` address that is not constant")))?;
            let value = num_bigint::BigInt::from(value);
            let value = if signed && width > 0 && value.bit(width as u64 - 1) {
                value - (num_bigint::BigInt::from(1u8) << width)
            } else {
                value
            };
            *slot = Some(
                value
                    .to_i128()
                    .ok_or_else(|| unsupported(format!("`{name}` address")))?,
            );
        }
        if self.path_condition()?.is_some() {
            return Err(unsupported(format!("`{name}` under a run-time condition")));
        }
        let (id, words) = self
            .m
            .readmem(format, args, addresses[0], addresses[1], site)?;
        for word in words {
            let width = word.access.msb - word.access.lsb + 1;
            let node = self.alloc(SLTNode::Constant(word.value, word.mask, width, false))?;
            self.write(store, id, word.access, (node, Sources::default()))?;
        }
        Ok(())
    }

    pub(super) fn capture(&mut self, expr: NodeId) -> Result<NodeId, sv::AnalyzerError> {
        let key = self.m.capture_key();
        self.alloc(SLTNode::Capture { expr, key })
    }

    /// A display, message, or `$finish` in a combinational process: an
    /// observer that emits the event whenever the process runs with the
    /// statement on its path (IEEE 1800-2023 9.2.2.2).
    pub(super) fn system_task(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
        assertion_site: Option<&celox_design::AssertionSite>,
    ) -> Result<(), sv::AnalyzerError> {
        let kind =
            system_task_kind(task).ok_or_else(|| unsupported(format!("system task `{task}`")))?;
        let observer_store = store.fork();
        let mut observed = Sources::default();
        let assertion = if matches!(kind, SystemTaskKind::Assertion(_)) {
            let Some(sv::ir::SystemTaskArg::Expr(condition)) = args.first() else {
                return Err(unsupported(format!("condition of `{task}`")));
            };
            let (node, sources) = self.eval(
                store,
                frames,
                &floating::condition(condition, self.m.variables, self.m.name_to_id),
                None,
            )?;
            observed.extend(sources);
            let truth = slt_truth(self.arena, node)?;
            Some(self.capture(truth)?)
        } else {
            None
        };
        let (template, mut values) = system_task_template(self.m, &kind, args)?;
        prepare_time_operands(self.m, template.as_deref(), &mut values)?;
        let mut captured = Vec::with_capacity(values.len());
        let mut arg_widths = Vec::with_capacity(values.len());
        let mut arg_signed = Vec::with_capacity(values.len());
        let mut arg_is_string = Vec::with_capacity(values.len());
        let mut arg_float_precision = Vec::with_capacity(values.len());
        let mut arg_is_handle = Vec::with_capacity(values.len());
        for value in &values {
            let node = match value {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    let expr = if self.m.calls(expr) {
                        self.hoist(store, frames, expr)?
                    } else {
                        expr.clone()
                    };
                    let signed = self.expr_signed(&expr);
                    arg_is_handle.push(self.m.expr_is_string(&expr));
                    arg_float_precision.push(floating::precision(
                        &expr,
                        self.m.variables,
                        self.m.name_to_id,
                    ));
                    let (node, sources) = self.eval(store, frames, &expr, None)?;
                    arg_signed.push(signed);
                    arg_is_string.push(false);
                    observed.extend(sources);
                    node
                }
                sv::ir::SystemTaskArg::Str(text) => {
                    let bytes = unescape(text)?;
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    arg_signed.push(false);
                    arg_is_string.push(true);
                    arg_float_precision.push(None);
                    arg_is_handle.push(false);
                    slt_constant(self.arena, value, width, false)?
                }
                sv::ir::SystemTaskArg::Empty => {
                    return Err(unsupported(format!("empty argument of `{task}`")));
                }
            };
            arg_widths.push(self.width(node));
            captured.push(self.capture(node)?);
        }
        // A `%t` reports in the format the design's `$timeformat` calls set
        // when the process runs (IEEE 1800-2023 20.4.3). The format does not
        // run the process.
        let time_format_arg =
            match time_format_operand(self.m, template.as_deref().is_some_and(converts_time)) {
                Some(format) => {
                    let (node, _) = self.eval(store, frames, &format, None)?;
                    arg_signed.push(false);
                    arg_is_string.push(false);
                    arg_float_precision.push(None);
                    arg_is_handle.push(false);
                    arg_widths.push(self.width(node));
                    captured.push(self.capture(node)?);
                    true
                }
                None => false,
            };
        // A report carries the time it is issued at (IEEE 1800-2023 20.10).
        // The time does not run the process.
        let timestamp_arg = matches!(kind, SystemTaskKind::Message(_) | SystemTaskKind::Fatal);
        if timestamp_arg {
            let time = sv::ir::Expr::Ident(self.m.time_slot());
            let (node, _) = self.eval(store, frames, &time, None)?;
            arg_signed.push(false);
            arg_is_string.push(false);
            arg_float_precision.push(None);
            arg_is_handle.push(false);
            arg_widths.push(self.width(node));
            captured.push(self.capture(node)?);
        }
        let event_kind = kind.event_kind();
        let condition = self.path_condition()?;
        let guard = if let Some(passed) = assertion {
            // An assertion fires only on failure and only on the active path.
            Some(if let Some((node, sources)) = condition {
                observed.extend(sources);
                let inactive = slt_not(self.arena, &mut self.consts, node)?;
                slt_or(self.arena, &mut self.consts, inactive, passed)?
            } else {
                passed
            })
        } else {
            match (&kind, condition) {
                (SystemTaskKind::Print(..) | SystemTaskKind::Control(_), None) => None,
                (SystemTaskKind::Print(..) | SystemTaskKind::Control(_), Some((node, sources))) => {
                    observed.extend(sources);
                    Some(node)
                }
                // Assertion sites fire when their guard is false.
                (SystemTaskKind::Message(_) | SystemTaskKind::Fatal, None) => {
                    Some(self.constant(0, 1)?)
                }
                (SystemTaskKind::Message(_) | SystemTaskKind::Fatal, Some((node, sources))) => {
                    observed.extend(sources);
                    Some(slt_not(self.arena, &mut self.consts, node)?)
                }
                (SystemTaskKind::Assertion(_), _) => unreachable!("assertion guard handled above"),
            }
        };
        let guard = guard.map(|guard| self.capture(guard)).transpose()?;
        // The first pass over a run-time loop body only finds the state it
        // writes; the operands ran for their effects on that state.
        if self.folds.probing > 0 {
            return Ok(());
        }
        self.events
            .effect_sensitivity
            .extend(observed.iter().copied());
        let site_id = self.events.site_base + self.events.sites.len() as u32;
        // The action of a deferred assertion waits in the queue of its
        // process, which lowering names by the process's activation group
        // (IEEE 1800-2023 16.4). A deferred `$fatal` ends the simulation
        // when it matures, not while combinational logic settles.
        let deferred = assertion_site
            .map(|assertion| assertion.deferral)
            .filter(|&region| region != celox_design::AssertionDeferral::None)
            .map(|region| celox_design::DeferredSite::Action { queue: 0, region });
        let fatal = kind.is_fatal() && deferred.is_none();
        // Inside a run-time loop the event is emitted once per iteration by
        // the loop's runner (see `fold_loop`).
        let captured_in_loop = if let Some(effects) = &mut self.folds.loop_effects {
            effects.push(SLTForEffect::Event {
                site_id,
                guard,
                emit_on_true: matches!(
                    kind,
                    SystemTaskKind::Print(..) | SystemTaskKind::Control(_)
                ),
                args: captured.clone(),
                fatal_error_code: fatal.then_some(i64::from(site_id)),
            });
            true
        } else {
            false
        };
        self.events.sites.push(RuntimeEventSite {
            kind: event_kind,
            template,
            sizing: DisplaySizing::Ieee,
            scope: None,
            location: report_location(site),
            assertion: assertion_site.cloned(),
            deferred,
            timestamp_arg,
            time_format_arg,
            arg_widths,
            arg_signed,
            arg_is_string,
            arg_float_precision,
            arg_is_handle,
        });
        let mut preceding_writes = Vec::new();
        for (id, range) in store.iter() {
            if self.m.is_hidden(*id) {
                continue;
            }
            for (&lsb, (value, width, _)) in &range.ranges {
                if value.is_some() {
                    preceding_writes.push(VarAtomBase::new(*id, lsb, lsb + width - 1));
                }
            }
        }
        let mut observed_ids: Vec<SourceVarId> = observed.iter().map(|atom| atom.id).collect();
        observed_ids.sort_by_key(|id| id.0);
        observed_ids.dedup();
        let mut local_inputs = Vec::new();
        for id in observed_ids {
            let Some(range) = observer_store.get(&id) else {
                continue;
            };
            if !range.ranges.values().any(|(value, _, _)| value.is_some()) {
                continue;
            }
            let width = self.m.var(id).width;
            let (node, _) = self.read(&observer_store, id, full(width))?;
            local_inputs.push((id, node));
        }
        self.events.observers.push(CombObserver {
            site_id,
            activation_group: 0,
            guard,
            args: captured,
            loop_runner: None,
            sensitivity: Vec::new(),
            sensitivity_counters: Vec::new(),
            local_inputs,
            observed_inputs: observed
                .into_iter()
                .filter(|atom| !self.m.is_hidden(atom.id))
                .collect(),
            position_inputs: Vec::new(),
            preceding_writes,
            written_before: Vec::new(),
            written_input_atoms: Vec::new(),
            written_inputs: Vec::new(),
            captured_in_loop,
        });
        Ok(())
    }
}
