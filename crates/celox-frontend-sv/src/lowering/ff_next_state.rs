//! The next-state form of a module's `always_ff` processes, when a report
//! asks for it (`AnalysisOptions::ff_next_state_report`): lowered beside
//! a reference imperative lowering and checked independently of the executing schedule.
//!
//! Each process's form ([`comb::NextState`]) is copied through the
//! canonicalizing allocator ([`celox_slt::canon::intern`]) into one DAG of
//! the module, so equal expressions of all its processes are one node. A
//! form is taken only when it computes what the process's imperative units
//! do: both run on the same random states before the edge, the units on
//! the SIR interpreter and the form lowered to SIR, and every visible
//! variable must end equal. A form that differs is a fallback.
//!
//! The report line per module definition (`CELOX_FF_NEXT_STATE`, and a
//! `CELOX_FF_NEXT_STATE_MISMATCH` line per form that differs) gives:
//! - `processes`, `lowered`: the `always_ff` processes and the forms taken;
//! - `checked`: the forms the equivalence check ran on, and `unchecked`
//!   why it did not run on the others;
//! - `targets`, `root_bits`, `roots`: the next-state bit ranges;
//! - `ports`, `port_bits`: the memory write ports and the bits they write;
//! - `nodes`: the distinct nodes of the forms as lowered, `canonical` their
//!   canonical count, and `unique` the nodes of the canonical DAG;
//! - `same_op_bit_runs`: the 1-bit operations of the DAG that one wide
//!   operation would compute (see [`celox_slt::canon::same_op_bit_runs`]);
//! - `fallbacks`: why the other processes keep only their units.

use std::collections::BTreeMap;

use super::comb::{NextState, NextStateRoot, RamWritePort};
use super::ff_graph::{canonical_copy, next_state_unit};
use super::*;
use celox_sir::interp::{InterpError, InterpMachine, ResolvedAccess, StoreSnapshot, execute_unit};
use celox_slt::canon::{Canonicalizer, reachable, same_op_bit_runs};
use num_traits::{ToPrimitive, Zero};

/// The random states before the edge each form is checked on, besides the
/// all-zero and the all-one state.
const RANDOM_STATES: u64 = 14;

/// The imperative units of one `always_ff` process: its evaluation and the
/// commit of all its results.
pub(super) struct ImperativeUnits<'u> {
    pub eval: &'u ExecutionUnit<RegionedVarAddr>,
    pub apply: &'u ExecutionUnit<RegionedVarAddr>,
    /// The staged variables, as (variable, staging value, staging mask).
    pub staged: &'u [(SourceVarId, SourceVarId, SourceVarId)],
}

pub(super) struct NextStateReport {
    module: String,
    /// The file the report lines are appended to, if any.
    path: Option<std::path::PathBuf>,
    /// The lines of the forms that differ from their units.
    mismatches: Vec<String>,
    /// The arena each process's form is lowered in.
    raw: SLTNodeArena<SourceVarId>,
    /// The nodes of the forms taken, in `raw`.
    raw_roots: Vec<NodeId>,
    /// The canonical DAG of the forms taken.
    dag: SLTNodeArena<SourceVarId>,
    roots: Vec<NextStateRoot>,
    ports: Vec<RamWritePort>,
    /// The forms taken, in `dag`, with their processes' trigger sets.
    forms: Vec<(TriggerSet<SourceVarId>, NextState)>,
    processes: usize,
    lowered: usize,
    checked: usize,
    unchecked: BTreeMap<String, usize>,
    fallbacks: BTreeMap<String, usize>,
}

impl NextStateReport {
    pub(super) fn new(module: &str, path: Option<&std::path::Path>) -> Self {
        Self {
            module: module.to_string(),
            path: path.map(std::path::Path::to_path_buf),
            mismatches: Vec::new(),
            raw: SLTNodeArena::new(),
            raw_roots: Vec::new(),
            dag: SLTNodeArena::new(),
            roots: Vec::new(),
            ports: Vec::new(),
            forms: Vec::new(),
            processes: 0,
            lowered: 0,
            checked: 0,
            unchecked: BTreeMap::new(),
            fallbacks: BTreeMap::new(),
        }
    }

    fn fall_back(&mut self, reason: String) {
        // Reasons name the construct before any detail.
        let reason: String = reason.chars().take(80).collect();
        *self.fallbacks.entry(reason).or_default() += 1;
    }

    pub(super) fn attempt(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        body: &[sv::ir::Stmt],
        trigger_set: &TriggerSet<SourceVarId>,
        units: ImperativeUnits<'_>,
    ) {
        self.processes += 1;
        // The form adds nothing to the module: its next-state variables and
        // temporaries go, and the module lowers on as if it had not been
        // tried.
        let checkpoint = pm.checkpoint();
        let first_temporary = pm.next_id;
        let taken = self.attempt_form(pm, body, units);
        pm.rollback(checkpoint);
        let Some((form, canonical, checked)) = taken else {
            return;
        };
        let reads_temporary = reachable(&self.dag, canonical.nodes())
            .into_iter()
            .any(|node| {
                matches!(self.dag.get(node), SLTNode::Input { variable, .. }
                    if variable.0 >= first_temporary.0)
            });
        if reads_temporary {
            self.fall_back("a form that reads a lowering temporary".to_string());
            return;
        }
        self.lowered += 1;
        self.checked += usize::from(checked);
        self.raw_roots.extend(form.nodes());
        self.roots.extend(canonical.roots.iter().cloned());
        self.ports.extend(canonical.ports.iter().cloned());
        self.forms.push((trigger_set.clone(), canonical));
    }

    /// The form of a process, its canonical copy and whether the check ran
    /// on it, or none when the form does not take the process.
    fn attempt_form(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        body: &[sv::ir::Stmt],
        units: ImperativeUnits<'_>,
    ) -> Option<(NextState, NextState, bool)> {
        pm.attempt_calls();
        let lowered = comb::Comb::new(pm, &mut self.raw).lower_next_state(body);
        pm.end_attempt_calls(false);
        let form = match lowered {
            Ok(form) => form,
            Err(error) => {
                self.fall_back(match error {
                    sv::AnalyzerError::Unsupported(reason) => reason,
                    error => format!("{error:?}"),
                });
                return None;
            }
        };
        let canonical = match canonical_copy(&self.raw, &mut self.dag, &form) {
            Ok(canonical) => canonical,
            Err(error) => {
                self.fall_back(format!("no canonical form: {error}"));
                return None;
            }
        };
        let checked = match check(pm, &self.dag, &canonical, &units, self.processes as u64) {
            Check::Equal => true,
            Check::Unchecked(reason) => {
                *self.unchecked.entry(reason).or_default() += 1;
                false
            }
            Check::Mismatch(detail) => {
                self.mismatches.push(format!(
                    "CELOX_FF_NEXT_STATE_MISMATCH module={} process={} {detail}",
                    self.module, self.processes
                ));
                self.fall_back("equivalence mismatch".to_string());
                return None;
            }
        };
        Some((form, canonical, checked))
    }

    /// Report the measures, if a report was asked for, and hand back the
    /// forms taken.
    pub(super) fn finish(
        mut self,
    ) -> (
        SLTNodeArena<SourceVarId>,
        celox_frontend_core::symbolic::artifact::NextStateForms,
    ) {
        if let Some(path) = self.path.clone() {
            self.print(&path);
        }
        (
            self.dag,
            celox_frontend_core::symbolic::artifact::NextStateForms {
                complete: Default::default(),
                forms: self.forms,
            },
        )
    }

    fn print(&mut self, path: &std::path::Path) {
        let dag_roots: Vec<NodeId> = NextState {
            roots: self.roots.clone(),
            ports: self.ports.clone(),
        }
        .nodes()
        .collect();
        let nodes = reachable(&self.raw, self.raw_roots.iter().copied()).len();
        let canonical = Canonicalizer::new(&self.raw).count(self.raw_roots.iter().copied());
        let unique = reachable(&self.dag, dag_roots.iter().copied()).len();
        let runs = same_op_bit_runs(&self.dag, dag_roots.iter().copied());
        let targets: HashSet<SourceVarId> = self.roots.iter().map(|root| root.target).collect();
        let width = |access: &BitAccess| access.msb - access.lsb + 1;
        let mut lines = std::mem::take(&mut self.mismatches);
        lines.push(format!(
            "CELOX_FF_NEXT_STATE module={} processes={} lowered={} checked={} targets={} root_bits={} roots={} ports={} port_bits={} nodes={nodes} canonical={canonical} unique={unique} same_op_bit_runs={runs} fallbacks={} unchecked={}",
            self.module,
            self.processes,
            self.lowered,
            self.checked,
            targets.len(),
            self.roots
                .iter()
                .map(|root| width(&root.access))
                .sum::<usize>(),
            self.roots.len(),
            self.ports.len(),
            self.ports
                .iter()
                .map(|port| width(&port.access))
                .sum::<usize>(),
            serde_json::to_string(&self.fallbacks).unwrap_or_default(),
            serde_json::to_string(&self.unchecked).unwrap_or_default(),
        ));
        // A diagnostic: a report that cannot be written changes nothing.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = file.write_all((lines.join("\n") + "\n").as_bytes());
        }
    }
}

/// The outcome of an equivalence check.
enum Check {
    Equal,
    /// The check could not run, and why.
    Unchecked(String),
    Mismatch(String),
}

/// Whether `form` ends every visible variable where the imperative units
/// do, from the same states before the edge.
fn check(
    pm: &procedural::ProcModule<'_>,
    dag: &SLTNodeArena<SourceVarId>,
    form: &NextState,
    units: &ImperativeUnits<'_>,
    seed: u64,
) -> Check {
    let unit = match next_state_unit(pm, dag, form, STABLE_REGION) {
        Ok(unit) => unit,
        Err(reason) => return Check::Mismatch(format!("form not lowered: {reason}")),
    };
    let zeroed: HashSet<SourceVarId> = units
        .staged
        .iter()
        .flat_map(|&(_, value, mask)| [value, mask])
        .collect();
    let short = |error: InterpError| -> String {
        let text = error.to_string();
        text.chars().take(60).collect()
    };
    for state in 0..RANDOM_STATES + 2 {
        let machine = || CheckMachine::new(pm, &zeroed, seed, state);
        let mut imperative = machine();
        if let Err(error) = execute_unit(units.eval, &mut imperative, &[], pm.four_state)
            .and_then(|_| execute_unit(units.apply, &mut imperative, &[], pm.four_state))
        {
            return Check::Unchecked(format!("imperative units: {}", short(error)));
        }
        let mut next = machine();
        if let Err(error) = execute_unit(&unit, &mut next, &[], pm.four_state) {
            return Check::Mismatch(format!("form failed: {error}"));
        }
        let mut written: Vec<SourceVarId> = imperative
            .written
            .union(&next.written)
            .copied()
            .filter(|id| !pm.is_hidden(*id))
            .collect();
        written.sort_by_key(|id| id.0);
        for id in written {
            let left = imperative.stable_value(id);
            let right = next.stable_value(id);
            if left != right {
                let differ = (&left.0 ^ &right.0) | (&left.1 ^ &right.1);
                let bit = differ.trailing_zeros().unwrap_or(0);
                let bit_of = |plane: &BigUint| (plane >> bit) & BigUint::from(1u8);
                return Check::Mismatch(format!(
                    "state={state} variable={} bit={bit} imperative={}/{} form={}/{}",
                    pm.var(id).path.join("."),
                    bit_of(&left.0),
                    bit_of(&left.1),
                    bit_of(&right.0),
                    bit_of(&right.1),
                ));
            }
        }
    }
    Check::Equal
}

/// The state of a module for one equivalence check: each variable of each
/// region as a (value, unknown-mask) pair of its logical bits, created on
/// first access. A variable's value before the edge follows from the seed
/// and the state number alone, so two machines of one state agree on it.
struct CheckMachine<'p> {
    variables: &'p HashMap<SourceVarId, SvVariable>,
    /// Variables that start as zero: the staging of staged writes.
    zeroed: &'p HashSet<SourceVarId>,
    seed: u64,
    state: u64,
    values: HashMap<(u32, SourceVarId), (BigUint, BigUint)>,
    /// The variables whose stable value the unit wrote.
    written: HashSet<SourceVarId>,
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn ones(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - 1u8
}

impl<'p> CheckMachine<'p> {
    fn new(
        pm: &'p procedural::ProcModule<'_>,
        zeroed: &'p HashSet<SourceVarId>,
        seed: u64,
        state: u64,
    ) -> Self {
        Self {
            variables: &*pm.variables,
            zeroed,
            seed,
            state,
            values: HashMap::default(),
            written: HashSet::default(),
        }
    }

    fn width(&self, id: SourceVarId) -> Result<usize, InterpError> {
        self.variables
            .get(&id)
            .map(|variable| variable.width)
            .ok_or_else(|| InterpError::Machine(format!("unknown variable {}", id.0)))
    }

    /// The value of `id` before the edge: zero in state 0, all ones in
    /// state 1, random bits otherwise; never unknown.
    fn initial(&self, id: SourceVarId) -> Result<(BigUint, BigUint), InterpError> {
        let width = self.width(id)?;
        let value = if self.zeroed.contains(&id) || self.state == 0 {
            BigUint::zero()
        } else if self.state == 1 {
            ones(width)
        } else {
            let mut random =
                self.seed.wrapping_mul(0x1000_0000_01b3) ^ (self.state << 40) ^ id.0 as u64;
            let words: Vec<u64> = (0..width.div_ceil(64))
                .map(|_| splitmix(&mut random))
                .collect();
            BigUint::from_slice(
                &words
                    .iter()
                    .flat_map(|word| [*word as u32, (*word >> 32) as u32])
                    .collect::<Vec<_>>(),
            ) & ones(width)
        };
        Ok((value, BigUint::zero()))
    }

    fn cell(
        &mut self,
        region: u32,
        id: SourceVarId,
    ) -> Result<&mut (BigUint, BigUint), InterpError> {
        if !self.values.contains_key(&(region, id)) {
            let value = if region == STABLE_REGION {
                self.initial(id)?
            } else {
                // Every other region starts as a copy of the stable one.
                self.cell(STABLE_REGION, id)?.clone()
            };
            self.values.insert((region, id), value);
        }
        Ok(self.values.get_mut(&(region, id)).expect("inserted above"))
    }

    fn stable_value(&mut self, id: SourceVarId) -> (BigUint, BigUint) {
        self.cell(STABLE_REGION, id)
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    /// The logical bit offset of an access, or `None` when it is outside
    /// every representable position.
    fn offset(access: ResolvedAccess<'_>) -> Option<usize> {
        let dynamic = |slot: usize| -> Option<usize> {
            access.dynamics[slot].and_then(|value| value.payload.to_usize())
        };
        match access.offset {
            SIROffset::Static(offset) => Some(*offset),
            SIROffset::Dynamic(_) => dynamic(0),
            SIROffset::Element {
                element_width,
                bit_offset,
                dynamic_bit_offset,
                ..
            } => {
                let base = dynamic(0)?.checked_mul(*element_width)?;
                let within = match dynamic_bit_offset {
                    Some(_) => dynamic(1)?,
                    None => 0,
                };
                base.checked_add(*bit_offset)?.checked_add(within)
            }
            SIROffset::ElementRun { element_width, .. } => dynamic(0)?.checked_mul(*element_width),
            SIROffset::PackedElements { bit_offset, .. } => Some(*bit_offset),
        }
    }
}

impl InterpMachine<RegionedVarAddr> for CheckMachine<'_> {
    fn load(
        &mut self,
        addr: &RegionedVarAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        let Some(offset) = Self::offset(access) else {
            return Ok(SIRValue::new(0u8));
        };
        let (value, mask) = self.cell(addr.region, addr.var_id)?;
        Ok(SIRValue::new_four_state(
            (&*value >> offset) & ones(bits),
            (&*mask >> offset) & ones(bits),
        ))
    }

    fn store(
        &mut self,
        addr: &RegionedVarAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        stored: &SIRValue,
    ) -> Result<(), InterpError> {
        let width = self.width(addr.var_id)?;
        let Some(offset) = Self::offset(access) else {
            return Ok(());
        };
        if offset >= width {
            return Ok(());
        }
        let kept = ones(width) ^ ((ones(bits) << offset) & ones(width));
        let (value, mask) = self.cell(addr.region, addr.var_id)?;
        *value = (&*value & &kept) | (((&stored.payload & ones(bits)) << offset) & ones(width));
        *mask = (&*mask & &kept) | (((&stored.mask & ones(bits)) << offset) & ones(width));
        if addr.region == STABLE_REGION {
            self.written.insert(addr.var_id);
        }
        Ok(())
    }

    fn commit(
        &mut self,
        src: &RegionedVarAddr,
        dst: &RegionedVarAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        let value = self.load(src, access, bits)?;
        self.store(dst, access, bits, &value)
    }

    fn notify_triggers(
        &mut self,
        _addr: &RegionedVarAddr,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        _triggers: &[celox_sir::TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn notify_trigger_only_store(
        &mut self,
        _addr: &RegionedVarAddr,
        _triggers: &[celox_sir::TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn capture_store_range(
        &mut self,
        _addr: &RegionedVarAddr,
        _access: ResolvedAccess<'_>,
        _bits: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        Ok(StoreSnapshot::default())
    }

    fn enable_comb_captures(
        &mut self,
        _addr: &RegionedVarAddr,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        _before: &StoreSnapshot,
        _sites: &[u32],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn emit_runtime_event(&mut self, site_id: u32, _args: &[SIRValue]) -> Result<(), InterpError> {
        Err(InterpError::UnsupportedOperation(format!(
            "runtime event {site_id}"
        )))
    }

    fn emit_comb_capture_event(
        &mut self,
        site_id: u32,
        _args: &[SIRValue],
        _fatal_error_code: Option<i64>,
        _consume_enabled: bool,
    ) -> Result<(), InterpError> {
        Err(InterpError::UnsupportedOperation(format!(
            "comb capture event {site_id}"
        )))
    }

    fn enable_comb_capture_if_changed(
        &mut self,
        _old: &SIRValue,
        _new: &SIRValue,
        _sites: &[u32],
    ) -> Result<(), InterpError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FrontendTraceOptions, schedule_sources_with_options, sv};
    use std::sync::Arc;

    /// The scheduled SIR of `source`, or its error, and the report lines
    /// when `report` names a file.
    fn schedule(
        source: &str,
        four_state: bool,
        report: Option<&std::path::Path>,
    ) -> (String, Vec<String>) {
        if let Some(path) = report {
            let _ = std::fs::remove_file(path);
        }
        let scheduled = schedule_sources_with_options(
            &[(source, std::path::Path::new("next_state.sv"))],
            "Top",
            &[],
            &[],
            &[],
            four_state,
            &celox_frontend_core::ParallelScheduleOptions::default(),
            &FrontendTraceOptions::default(),
            None,
            &mut Vec::new(),
            sv::AnalysisOptions {
                ff_next_state_report: report.map(Arc::from),
                ..sv::AnalysisOptions::default()
            },
        );
        let sir = match scheduled {
            Ok(output) => format!("{:?}", output.scheduled.sir),
            Err(error) => format!("error {error:?}"),
        };
        let lines = report
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|report| report.lines().map(str::to_string).collect())
            .unwrap_or_default();
        if let Some(path) = report {
            let _ = std::fs::remove_file(path);
        }
        (sir, lines)
    }

    /// The report lines of scheduling `source`. Reporting changes nothing:
    /// the SIR is the one of scheduling it without the report.
    fn report(name: &str, source: &str, four_state: bool) -> Vec<String> {
        let path = std::env::temp_dir().join(format!(
            "celox-next-state-{}-{name}.txt",
            std::process::id()
        ));
        let (reported, lines) = schedule(source, four_state, Some(&path));
        let (plain, _) = schedule(source, four_state, None);
        assert!(!reported.starts_with("error "), "{reported}");
        assert_eq!(reported, plain, "the report changed the lowering of {name}");
        lines
    }

    /// A memory written at a run-time index is a write port, a call in a
    /// nonblocking assignment writes its own variables blocking, a bit
    /// written after the whole target splits the earlier value, and the
    /// form computes what the process's units do; a process with a system
    /// task keeps its units alone.
    #[test]
    fn forms_match_their_units() {
        let source = "module Top(input logic clk, input logic we, input logic [3:0] wa,
                         input logic [7:0] wd, input logic [7:0] a,
                         output logic [7:0] q, output logic [7:0] r,
                         output logic [1:0] s);
              logic [7:0] mem [16];
              function automatic logic [7:0] twice(input logic [7:0] x);
                  return x * 2 + 1;
              endfunction
              always_ff @(posedge clk) begin
                  if (we) mem[wa] <= wd;
                  q <= mem[a[3:0]];
                  r <= twice(a);
                  s <= 2'b10;
                  if (a[0]) s[0] <= 1'b1;
              end
              always_ff @(posedge clk) if (we) $display(\"write %d\", wd);
            endmodule";
        for four_state in [false, true] {
            let lines = report("forms", source, four_state);
            assert!(
                lines.iter().all(|line| !line.contains("MISMATCH")),
                "{lines:?}"
            );
            let top = lines
                .iter()
                .find(|line| line.contains("module=Top "))
                .unwrap_or_else(|| panic!("{lines:?}"));
            for field in [
                "processes=2 ",
                "lowered=1 ",
                "checked=1 ",
                "ports=1 ",
                "port_bits=8 ",
                "same_op_bit_runs=0 ",
                "runtime events in the next-state form",
            ] {
                assert!(top.contains(field), "{field} in {top}");
            }
        }
    }

    /// Each specialization of a module reports under its parameter values.
    #[test]
    fn reports_name_specializations() {
        let source = "module Reg #(parameter W = 4)(input logic clk, input logic [W-1:0] d,
                                           output logic [W-1:0] q);
              always_ff @(posedge clk) q <= d;
            endmodule
            module Top(input logic clk, input logic [7:0] d, output logic [3:0] a,
                       output logic [7:0] b);
              Reg #(.W(4)) narrow(.clk(clk), .d(d[3:0]), .q(a));
              Reg #(.W(8)) wide(.clk(clk), .d(d), .q(b));
            endmodule";
        let lines = report("specializations", source, false);
        for label in ["module=Reg#(W=4) ", "module=Reg#(W=8) "] {
            assert!(
                lines.iter().any(|line| line.contains(label)),
                "{label} in {lines:?}"
            );
        }
    }
}
