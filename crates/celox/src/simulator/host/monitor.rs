//! Whole-variable monitoring through change-occurrence counters (21.2.3).
use super::*;
use celox_design::{HostRequestKind, HostRequestSite};
use celox_runtime::Halt;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Monitor {
    pub(super) call: StrobeCall,
    counters: Vec<u32>,
    force: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct MonitorState {
    console: Option<Monitor>,
    pub(super) files: Vec<Monitor>,
    enabled: bool,
}
impl Default for MonitorState {
    fn default() -> Self {
        Self {
            console: None,
            files: Vec::new(),
            enabled: true,
        }
    }
}
impl MonitorState {
    pub(crate) fn active(&self) -> bool {
        self.console.is_some() || !self.files.is_empty() || !self.enabled
    }
}

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn monitor_state(&self) -> MonitorState {
        if !self.slot_end.postponed_output {
            return MonitorState::default();
        }
        self.pending_host_events.lock().unwrap().monitors.clone()
    }
    pub(crate) fn restore_monitor_state(&mut self, state: &MonitorState) {
        if !self.slot_end.postponed_output {
            return;
        }
        self.pending_host_events.lock().unwrap().monitors = state.clone();
    }
    fn monitor_counters(&self, call: &StrobeCall) -> Result<Vec<u32>, Halt> {
        let site = &self.program.runtime_schema.host_request_sites[call.site];
        site.args
            .iter()
            .skip(usize::from(site.operation == 18))
            .map(|arg| {
                let counter = self
                    .program
                    .runtime_schema
                    .watches
                    .iter()
                    .find(|watch| watch.signal == arg.id)
                    .and_then(|watch| watch.changes)
                    .ok_or(RuntimeErrorCode::InternalError)?;
                let signal = self.backend.resolve_signal(&counter);
                Ok(self
                    .backend
                    .get(signal)
                    .to_u32_digits()
                    .first()
                    .copied()
                    .unwrap_or(0))
            })
            .collect()
    }
    pub(crate) fn valid_monitor_state(&self, state: &MonitorState) -> bool {
        let valid = |monitor: &Monitor, file| {
            self.program
                .runtime_schema
                .host_request_sites
                .get(monitor.call.site)
                .is_some_and(|site| {
                    site.kind == HostRequestKind::FileIo
                        && site.operation == if file { 18 } else { 17 }
                        && monitor.call.descriptor.is_some() == file
                        && self
                            .monitor_counters(&monitor.call)
                            .is_ok_and(|counters| counters.len() == monitor.counters.len())
                })
        };
        state
            .console
            .as_ref()
            .is_none_or(|monitor| valid(monitor, false))
            && state.files.iter().all(|monitor| valid(monitor, true))
    }
    pub(crate) fn monitor_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
    ) -> Result<(), Halt> {
        if matches!(site.operation, 19 | 20) {
            let mut pending = self.pending_host_events.lock().unwrap();
            pending.monitors.enabled = site.operation == 19;
            if site.operation == 19
                && let Some(monitor) = &mut pending.monitors.console
            {
                monitor.force = true;
            }
            return Ok(());
        }
        let call = self.postponed_call(site)?;
        let counters = self.monitor_counters(&call)?;
        let monitor = Monitor {
            call,
            counters,
            force: true,
        };
        let mut pending = self.pending_host_events.lock().unwrap();
        if site.operation == 17 {
            pending.monitors.console = Some(monitor);
        } else {
            pending.monitors.files.push(monitor);
        }
        Ok(())
    }
    pub(super) fn run_monitors(&mut self, timestamp: u64) -> Result<(), Halt> {
        let mut state = std::mem::take(&mut self.pending_host_events.lock().unwrap().monitors);
        let result = (|| {
            for monitor in state
                .console
                .iter_mut()
                .filter(|_| state.enabled)
                .chain(state.files.iter_mut())
            {
                let counters = self.monitor_counters(&monitor.call)?;
                if monitor.force || counters != monitor.counters {
                    self.run_postponed_call(monitor.call.clone(), timestamp)?;
                    monitor.force = false;
                }
                monitor.counters = counters;
            }
            Ok(())
        })();
        self.pending_host_events.lock().unwrap().monitors = state;
        result
    }
}
