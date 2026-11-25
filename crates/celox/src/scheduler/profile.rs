//! Activation counters of the compiled scheduler
//! (`SimulatorBuilder::activation_profile`): how often each part of a time
//! slot runs, what started it, and how often it passed an edge or a change
//! on. Each counter is a 64-bit word of the native arena
//! (`native_arena::Header::profile`), which the unit increments where the
//! counted work runs; [`ActivationSite`] names it.
use super::*;
use crate::activation_profile::{ActivationKind, ActivationSite};
use crate::simulator::native_io::ArenaHeader;

/// The counters a unit being generated increments.
#[derive(Default)]
pub(super) struct Counters {
    sites: Vec<ActivationSite>,
    index: HashMap<ActivationSite, usize>,
}

impl Counters {
    fn index(&mut self, site: ActivationSite) -> usize {
        if let Some(&index) = self.index.get(&site) {
            return index;
        }
        let index = self.sites.len();
        self.sites.push(site.clone());
        self.index.insert(site, index);
        index
    }

    pub(super) fn into_sites(self) -> Vec<ActivationSite> {
        self.sites
    }
}

impl Generator<'_> {
    /// Whether the unit counts its activations.
    pub(super) fn profiling(&self) -> bool {
        self.profile.is_some()
    }

    /// Count one activation of `kind`'s `measure` on `subject` where the
    /// unit is now.
    pub(super) fn count(
        &mut self,
        kind: ActivationKind,
        measure: &'static str,
        subject: impl FnOnce(&LaidOutProgram) -> String,
    ) {
        self.count_by(kind, measure, subject, None);
    }

    /// Count one activation when the one-bit `condition` is set.
    pub(super) fn count_if(
        &mut self,
        kind: ActivationKind,
        measure: &'static str,
        condition: RegisterId,
        subject: impl FnOnce(&LaidOutProgram) -> String,
    ) {
        self.count_by(kind, measure, subject, Some(condition));
    }

    fn count_by(
        &mut self,
        kind: ActivationKind,
        measure: &'static str,
        subject: impl FnOnce(&LaidOutProgram) -> String,
        condition: Option<RegisterId>,
    ) {
        let program = self.program;
        let Some(counters) = &mut self.profile else {
            return;
        };
        let index = counters.index(ActivationSite {
            kind,
            subject: subject(program),
            measure,
        });
        let pointer = self.arena_header_load(std::mem::offset_of!(ArenaHeader, profile));
        let word = self.constant(index as u64, 64);
        let old = self.arena_load(pointer, word);
        let amount = match condition {
            None => self.constant(1, 64),
            Some(condition) => {
                let one = self.constant(1, 64);
                let zero = self.constant(0, 64);
                let amount = self.b.alloc_bit(64, false);
                self.b
                    .emit(SIRInstruction::Mux(amount, condition, one, zero));
                amount
            }
        };
        let new = self.binary(64, old, BinaryOp::Add, amount);
        self.arena_store(pointer, word, new);
    }

    /// The name of event `event`'s signal.
    pub(super) fn event_name(program: &LaidOutProgram, event: &Event) -> String {
        program.get_path(&event.signal)
    }
}

/// The name of process kernel `index`: its origin in the source.
pub(super) fn kernel_name(program: &LaidOutProgram, index: usize) -> String {
    program
        .runtime
        .runtime_schema
        .process_origins
        .get(index)
        .map_or_else(
            || format!("process {index}"),
            |origin| format!("{index}: {origin}"),
        )
}

impl Generator<'_> {
    /// Count a judgment of which domains a wave triggers, of `which` waves,
    /// and whether it found none (`quiet`).
    pub(super) fn count_judgment(&mut self, which: &'static str, quiet: RegisterId) {
        self.count(ActivationKind::DomainJudge, "judged", |_| which.into());
        self.count_if(ActivationKind::DomainJudge, "quiet", quiet, |_| {
            which.into()
        });
    }
}

/// The name of group `index` of an event-driven settle, `activation`'s
/// units: when it runs and the first state its units write.
pub(super) fn group_name<'a>(
    program: &LaidOutProgram,
    index: usize,
    activation: &Activation,
    units: impl Iterator<Item = &'a Unit>,
) -> String {
    let units: Vec<&Unit> = units.collect();
    let when = if activation.every_settle {
        "every settle".to_string()
    } else {
        let mut when = format!("{} events", activation.events.count_ones());
        if activation.clock_inputs {
            when.push_str(", clock inputs");
        }
        when
    };
    format!(
        "group {index} ({} units, {when}) writes {}",
        units.len(),
        written_names(program, units.into_iter())
    )
}

/// A short name of the state `units` write: the first objects, then how
/// many more.
pub(super) fn written_names<'a>(
    program: &LaidOutProgram,
    units: impl Iterator<Item = &'a Unit>,
) -> String {
    let written: BTreeSet<AbsoluteAddr> = units
        .flat_map(instructions)
        .flat_map(written_storage)
        .filter(|address| address.region == STABLE_REGION)
        .map(|address| address.absolute_addr())
        .collect();
    let mut names: Vec<String> = written
        .iter()
        .take(3)
        .map(|address| program.get_path(address))
        .collect();
    if written.len() > names.len() {
        names.push(format!("+{}", written.len() - names.len()));
    }
    names.join(", ")
}
