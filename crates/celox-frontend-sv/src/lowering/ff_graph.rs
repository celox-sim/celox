//! Clocked destinations share a next-state DAG for their trigger. Blocking
//! writes update the symbolic store at their source position, and nonblocking
//! writes retain the value sampled there until the NBA region.
mod destinations;

use super::*;
use celox_frontend_core::symbolic::artifact::{EdgeEvaluation, NextStateForms};
use celox_slt::canon::Reinterner;
use celox_slt::next_state::Publication;
use comb::NextState;

#[derive(Default)]
pub(super) struct FfGraph {
    pub forms: NextStateForms,
    pub arena: SLTNodeArena<SourceVarId>,
    pub schedule: EdgeSchedule,
    active_targets: HashMap<TriggerSet<SourceVarId>, HashSet<SourceVarId>>,
    pub pending: HashMap<TriggerSet<SourceVarId>, NextState>,
}

impl FfGraph {
    pub fn try_add(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        trigger: &TriggerSet<SourceVarId>,
        body: &[sv::ir::Stmt],
        excluded: &HashSet<String>,
        active: &HashSet<SourceVarId>,
    ) -> Option<()> {
        if !register_updates(pm, body, excluded, &mut HashSet::default()) {
            return None;
        }
        let mut blocking = HashSet::default();
        let mut nba = HashSet::default();
        for stmt in body {
            stmt.walk(&mut |s| {
                if let sv::ir::Stmt::Assign {
                    lhs, nonblocking, ..
                } = s
                {
                    let id = pm.id(lhs.name()).expect("validated target");
                    if *nonblocking {
                        nba.insert(id);
                    } else {
                        blocking.insert(id);
                    }
                }
            });
        }
        if !blocking.is_disjoint(&nba) {
            return None;
        }
        // The symbolic store uses scratch variables for next values. Only
        // the expressions of original signals may escape this lowering.
        let checkpoint = pm.checkpoint();
        let first_temp = pm.next_id;
        let mut raw = SLTNodeArena::new();
        let form = comb::Comb::new(pm, &mut raw).lower_next_state(body, &blocking);
        pm.rollback(checkpoint);
        let form = form.ok()?;
        let unavailable_input = celox_slt::canon::reachable(&raw, form.nodes()).into_iter().any(|node| {
            matches!(raw.get(node), SLTNode::Input { variable, .. } if variable.0 >= first_temp.0 || active.contains(variable))
        });
        // Constant-index writes can appear as wide packed roots. Use the sparse
        // protocol only for element-addressed memory writes.
        if (form.ports.is_empty() && form.roots.is_empty())
            || unavailable_input
            || form
                .roots
                .iter()
                .any(|root| !pm.var(root.target).array_dims.is_empty())
        {
            return None;
        }
        let form = canonical_copy(&raw, &mut self.arena, &form).ok()?;
        self.active_targets
            .entry(trigger.clone())
            .or_default()
            .extend(blocking);
        let batch = self.pending.entry(trigger.clone()).or_default();
        batch.roots.extend(form.roots.iter().cloned());
        batch.ports.extend(form.ports.iter().cloned());
        self.forms.forms.push((trigger.clone(), form));
        Some(())
    }

    pub fn flush(
        &mut self,
        pm: &procedural::ProcModule<'_>,
        trigger: &TriggerSet<SourceVarId>,
        evaluations: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
        applications: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
        active: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
        nba: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    ) {
        if let Some(form) = self.pending.remove(trigger) {
            let segments = self.schedule.batches.entry(trigger.clone()).or_default();
            if let Some(unit) = evaluations.remove(trigger) {
                segments.push(EdgeBatch::Procedure(unit));
            }
            let mut targets: Vec<_> = form
                .roots
                .iter()
                .map(|root| VarAtomBase {
                    id: root.target,
                    access: root.access,
                })
                .collect();
            let mut commit = SIRBuilder::new();
            let memories = form.ports.iter().map(|port| port.array).collect();
            targets.extend(
                form.ports
                    .iter()
                    .map(|port| VarAtomBase::new(port.array, 0, pm.var(port.array).width - 1)),
            );
            ff::emit_sparse_memory_commits(&mut commit, &targets, &memories, pm);
            let commit = seal_builder(commit);
            segments.push(EdgeBatch::Graph(form));
            ff_processes::insert_or_merge_ff_unit(applications, trigger.clone(), commit.clone());
            let blocking = self.active_targets.remove(trigger).unwrap_or_default();
            if blocking.is_empty() {
                active
                    .entry(trigger.clone())
                    .or_insert_with(|| seal_builder(SIRBuilder::new()));
                ff_processes::insert_or_merge_ff_unit(nba, trigger.clone(), commit);
            } else {
                let (early, late): (Vec<_>, Vec<_>) =
                    targets.into_iter().partition(|t| blocking.contains(&t.id));
                for (phase, targets) in [(active, early), (nba, late)] {
                    let mut builder = SIRBuilder::new();
                    ff::emit_sparse_memory_commits(&mut builder, &targets, &memories, pm);
                    ff_processes::insert_or_merge_ff_unit(
                        phase,
                        trigger.clone(),
                        seal_builder(builder),
                    );
                }
            }
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct EdgeSchedule {
    batches: HashMap<TriggerSet<SourceVarId>, Vec<EdgeBatch>>,
}

type EdgeBatch = EdgeEvaluation<SourceVarId, RegionedVarAddr>;

impl EdgeSchedule {
    pub fn contains(&self, trigger: &TriggerSet<SourceVarId>) -> bool {
        self.batches.contains_key(trigger)
    }

    /// Keep symbolic batches and procedural barriers together until all of
    /// their instance inputs have producers in the design graph.
    pub fn finish(
        &self,
        module: &mut SimModule,
        wires: &HashSet<SourceVarId>,
    ) -> Result<(), String> {
        if self.batches.is_empty() {
            return Ok(());
        }
        let values = super::dataflow::optimize(module, wires)?;
        for (trigger, batches) in &self.batches {
            let mut plan = Vec::with_capacity(batches.len() + 1);
            for batch in batches {
                plan.push(match batch {
                    EdgeBatch::Graph(form) => {
                        EdgeBatch::Graph(form.map(|id| *id, |node| Ok::<_, String>(values[&node]))?)
                    }
                    EdgeBatch::Procedure(unit) => EdgeBatch::Procedure(unit.clone()),
                });
            }
            plan.extend(
                module
                    .eval_only_ff_blocks
                    .remove(trigger)
                    .map(EdgeBatch::Procedure),
            );
            module.next_state.evaluations.insert(trigger.clone(), plan);
        }
        Ok(())
    }
}

/// Each register has at most one write along an executed path. A second
/// NBA could produce an observable intermediate update even if the final
/// value equals the old value.
fn register_updates(
    pm: &procedural::ProcModule<'_>,
    body: &[sv::ir::Stmt],
    excluded: &HashSet<String>,
    written: &mut HashSet<SourceVarId>,
) -> bool {
    let calls = |expr| procedural::expr_calls(expr, &mut |_| true);
    body.iter().all(|stmt| match stmt {
        sv::ir::Stmt::Block { body, .. } => register_updates(pm, body, excluded, written),
        sv::ir::Stmt::If {
            condition,
            then_body,
            else_body,
        } => {
            let mut alternate = written.clone();
            if calls(condition)
                || !register_updates(pm, then_body, excluded, written)
                || !register_updates(pm, else_body, excluded, &mut alternate)
            {
                return false;
            }
            written.extend(alternate);
            true
        }
        sv::ir::Stmt::Case {
            selector,
            items,
            default,
            ..
        } => {
            if calls(selector)
                || items
                    .iter()
                    .flat_map(|item| &item.labels)
                    .any(|label| match label {
                        sv::ir::CaseLabel::Value(value) => calls(value),
                        sv::ir::CaseLabel::Range { low, high } => calls(low) || calls(high),
                    })
            {
                return false;
            }
            let before = written.clone();
            for branch in items
                .iter()
                .map(|item| item.body.as_slice())
                .chain(default.as_deref())
            {
                let mut writes = before.clone();
                if !register_updates(pm, branch, excluded, &mut writes) {
                    return false;
                }
                written.extend(writes);
            }
            true
        }
        sv::ir::Stmt::Assign {
            lhs,
            rhs,
            nonblocking,
        } => {
            let name = lhs.name();
            let Some(id) = pm.id(name) else { return false };
            let var = pm.var(id);
            let supported_target = match lhs {
                sv::ir::LValue::Ident(_) => var.array_dims.is_empty(),
                sv::ir::LValue::Select { .. } if !var.array_dims.is_empty() => {
                    var.array_dims.len() == 1 && !procedural::lvalue_calls(lhs, &mut |_| true)
                }
                sv::ir::LValue::Select { msb, lsb, .. } => [msb, lsb].into_iter().all(|bound| {
                    sv::typecheck::eval_const_expr_with_types(
                        bound,
                        pm.constants,
                        pm.parameter_types,
                    )
                    .is_some()
                }),
            };
            supported_target
                && (*nonblocking
                    || (var.array_dims.is_empty()
                        && pm.force_controls(id).is_none()
                        && !pm.force_holds.contains_key(&id)
                        && !pm.procedurally_forced.contains(&id)))
                && !var.hidden
                && var.float.is_none()
                && var.value_kind == celox_design::ValueKind::Bits
                && !excluded.contains(name)
                && !calls(rhs)
                && written.insert(id)
        }
        _ => false,
    })
}

/// `form`, lowered in `raw`, copied into the canonical DAG `dag`.
pub(super) fn canonical_copy(
    raw: &SLTNodeArena<SourceVarId>,
    dag: &mut SLTNodeArena<SourceVarId>,
    form: &NextState,
) -> Result<NextState, celox_slt::canon::ReinternError> {
    let mut copier = Reinterner::new(raw);
    form.map(|id| *id, |node| copier.copy(dag, node))
}

/// The SIR of `form`: every root and port computed from the state before
/// the edge, then the roots stored, then the ports in source order.
pub(super) fn next_state_unit(
    pm: &procedural::ProcModule<'_>,
    dag: &SLTNodeArena<SourceVarId>,
    form: &NextState,
    publication: Publication,
) -> Result<ExecutionUnit<RegionedVarAddr>, String> {
    let element_widths = pm
        .variables
        .iter()
        .filter_map(|(&id, v)| Some((id, unpacked_element_width(v)?)))
        .collect();
    celox_slt::next_state::lower_transition(
        dag,
        form,
        publication,
        pm.four_state,
        |var_id, region| RegionedVarAddrBase { region, var_id },
        &element_widths,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::{DeadStorePolicy, RegionedStateAddr, SPARSE_WORKING_REGION, StateAddr};

    fn schedule(
        source: &str,
        graph: bool,
        policy: DeadStorePolicy,
    ) -> celox_sir::SirProgram<StateAddr, RegionedStateAddr> {
        schedule_sources_with_options(
            &[(source, std::path::Path::new("ff.sv"))],
            "Top",
            &[],
            &[],
            &[],
            false,
            &celox_frontend_core::ParallelScheduleOptions {
                unobserved_nets: policy,
                ..Default::default()
            },
            &Default::default(),
            None,
            &mut Vec::new(),
            sv::AnalysisOptions {
                ff_next_state: graph,
                ..Default::default()
            },
        )
        .unwrap()
        .scheduled
        .sir
    }

    #[test]
    fn ram_banks_share_the_sampled_address_at_any_depth() {
        let source = r#"
            module Bank(input wire clk, en, d, input wire [AW-1:0] addr, output reg q);
                parameter AW = 3;
                reg words[0:(1<<AW)-1];
                always @(posedge clk) begin
                    if (en) words[addr] <= d;
                    q <= en ? d : words[addr];
                end
            endmodule
            module Top(input wire clk, select_addr, input wire [AW-1:0] a,b,
                       input wire [3:0] en,d, output wire [3:0] q, observed);
                parameter AW = 3;
                wire [AW-1:0] addr = select_addr ? a : b;
                assign observed = q * d;
                for (genvar i=0; i<4; i++) begin : bank
                    Bank #(.AW(AW)) mem(clk,en[i],d[i],addr,q[i]);
                end
            endmodule
        "#;
        let instructions = |width| {
            let source = source.replace("AW = 3", &format!("AW = {width}"));
            let sir = schedule(&source, true, DeadStorePolicy::Off);
            let units: Vec<_> = sir.eval_comb_apply_ffs.values().flatten().collect();
            assert_eq!(units.len(), 1);
            let ops: Vec<_> = units[0]
                .blocks
                .values()
                .flat_map(|b| &b.instructions)
                .collect();
            // The output still settles after publication, using the new q.
            let multiply = |op: &SIRInstruction<_>| {
                matches!(op, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
            };
            assert!(
                sir.eval_comb
                    .iter()
                    .flat_map(|u| u.blocks.values())
                    .flat_map(|b| &b.instructions)
                    .any(multiply)
            );
            assert!(!ops.iter().any(|op| multiply(op)));
            let writes: Vec<_> = ops
                .iter()
                .filter_map(|op| match op {
                    SIRInstruction::Store(target, SIROffset::Element { index, .. }, ..) => {
                        Some((target, index))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(writes.len(), 4);
            assert_eq!(
                writes
                    .iter()
                    .map(|(target, _)| target)
                    .collect::<HashSet<_>>()
                    .len(),
                4
            );
            assert!(writes.iter().all(|(_, index)| *index == writes[0].1));
            assert!(
                !ops.iter()
                    .any(|op| matches!(op, SIRInstruction::Commit(..)))
            );
            ops.len()
        };
        assert_eq!(instructions(3), instructions(9));
    }

    #[test]
    fn independent_samples_cross_a_procedure_on_the_same_clock() {
        let source = r#"
            module Top(input bit clk, other, input bit [7:0] a,b,
                       output bit [7:0] q0,q1,q2,q3,temp,p,watch,copy);
                always @(posedge clk) begin
                    copy = a+b;
                    q0 <= copy;
                    temp = a*b;
                    p <= temp;
                    temp = p;
                    q1 <= b+a;
                end
                always @(posedge clk) q2 <= a+b;
                always @(posedge clk) q3 <= b+a;
                always @(posedge other) watch <= p;
            endmodule
        "#;
        let sir = schedule(source, true, DeadStorePolicy::Off);
        let units = sir
            .eval_only_ffs
            .values()
            .find(|units| units.len() == 2)
            .unwrap();
        let operations = |unit: &ExecutionUnit<_>, op| {
            unit.blocks
                .values()
                .flat_map(|b| &b.instructions)
                .filter(|i| matches!(i, SIRInstruction::Binary(_,_,actual,_) if *actual == op))
                .count()
        };
        assert_eq!(operations(&units[0], BinaryOp::Mul), 1);
        assert_eq!(operations(&units[1], BinaryOp::Add), 1);
    }

    #[test]
    fn blocking_and_nonblocking_destinations_share_a_value_and_publish_in_their_regions() {
        let source = r#"
            module Top(input bit clk, en, input bit [7:0] d,
                       output bit [7:0] t,q,old,alt);
                always @(posedge clk) begin
                    old <= t;
                    t = d * d;
                    case (en)
                        1'b1: q <= t;
                        default: alt <= t;
                    endcase
                end
            endmodule
        "#;
        let mut trace = FrontendTrace::default();
        let out = schedule_sources_with_options(
            &[(source, std::path::Path::new("destinations.sv"))],
            "Top",
            &[],
            &[],
            &[],
            false,
            &Default::default(),
            &FrontendTraceOptions {
                sim_modules: true,
                ..Default::default()
            },
            Some(&mut trace),
            &mut Vec::new(),
            sv::AnalysisOptions {
                ff_next_state: true,
                ..Default::default()
            },
        )
        .unwrap();
        let modules = trace.sim_modules.unwrap();
        let top = modules.values().find(|m| m.name == "Top").unwrap();
        let (trigger, plan) = top.next_state.evaluations.iter().next().unwrap();
        let [EdgeEvaluation::Graph(form)] = &plan[..] else {
            panic!("expected a symbolic edge: {plan:?}")
        };
        let name = |id| top.variables[&id].path.join(".");
        let names: std::collections::BTreeSet<_> =
            form.roots.iter().map(|r| name(r.target)).collect();
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["alt", "old", "q", "t"]
        );
        let writes = |unit: &ExecutionUnit<RegionedVarAddr>| {
            unit.blocks
                .values()
                .flat_map(|b| &b.instructions)
                .filter_map(|i| match i {
                    SIRInstruction::Commit(_, dst, ..) if dst.region == STABLE_REGION => {
                        Some(name(dst.var_id))
                    }
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        let split = &top.split_commit_ff_blocks[trigger];
        assert_eq!(writes(&split.active), ["t"]);
        assert_eq!(writes(&split.nba), ["alt", "old", "q"]);
        let operations = |sir: &celox_sir::SirProgram<StateAddr, RegionedStateAddr>| {
            let mut staging_copies = 0;
            let mut multiplies = 0;
            for op in sir
                .eval_apply_ffs
                .values()
                .flatten()
                .flat_map(|u| u.blocks.values())
                .flat_map(|b| &b.instructions)
            {
                match op {
                    SIRInstruction::Commit(..) => staging_copies += 1,
                    SIRInstruction::Binary(_, _, BinaryOp::Mul, _) => multiplies += 1,
                    _ => {}
                }
            }
            (multiplies, staging_copies)
        };
        assert!(operations(&schedule(source, false, DeadStorePolicy::Off)).1 > 0);
        assert_eq!(operations(&out.scheduled.sir), (1, 0));
    }

    #[test]
    fn mixed_clock_publishes_a_sampled_memory_port_at_its_commit() {
        let source = r#"
            module Top(input bit clk, en, other, input bit [3:0] addr, input bit [7:0] d,
                       output bit [7:0] old, tmp, p, seen);
                always @(posedge other) seen <= old;
                bit [7:0] words[0:8];
                always @(posedge clk) begin
                    tmp = d*d;
                    p <= tmp;
                end
                always @(posedge clk) begin
                    if (en) words[addr] <= d;
                    old <= words[addr];
                end
            endmodule
        "#;
        let sir = schedule(source, true, DeadStorePolicy::Off);
        let stores = |units: &[ExecutionUnit<RegionedStateAddr>]| {
            units
                .iter()
                .flat_map(|u| u.blocks.values())
                .flat_map(|b| &b.instructions)
                .filter_map(|inst| match inst {
                    SIRInstruction::Store(addr, SIROffset::Element { .. }, ..) => Some(addr.region),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sir.eval_apply_ffs
                .values()
                .flat_map(|u| stores(u))
                .collect::<Vec<_>>(),
            [STABLE_REGION]
        );
        assert_eq!(
            sir.eval_only_ffs
                .values()
                .flat_map(|u| stores(u))
                .collect::<Vec<_>>(),
            [SPARSE_WORKING_REGION]
        );
        assert!(sir.eval_apply_ffs.values().flatten().flat_map(|u| u.blocks.values())
            .flat_map(|b| &b.instructions).all(|i| !matches!(i, SIRInstruction::Commit(src,..) if src.region == SPARSE_WORKING_REGION)));
    }

    #[test]
    fn shared_enable_preserves_the_disabled_registers() {
        let source =
            "module Top(input bit clk, en, input bit [31:0] data, output bit [31:0] a,b,c,d);
            always_ff @(posedge clk) if (en) a <= data;
            always_ff @(posedge clk) if (en) b <= a;
            always_ff @(posedge clk) if (en) c <= b;
            always_ff @(posedge clk) if (en) d <= c;
            endmodule";
        let sir = schedule(source, true, DeadStorePolicy::Off);
        let units = sir.eval_apply_ffs.values().flatten().collect::<Vec<_>>();
        let blocks: Vec<_> = units.iter().flat_map(|unit| unit.blocks.values()).collect();
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b.terminator, SIRTerminator::Branch { .. }))
                .count(),
            1
        );
        assert_eq!(
            blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Store(..)))
                .count(),
            4
        );
        assert!(
            !blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .any(|inst| matches!(inst, SIRInstruction::Commit(..) | SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn split_clock_branches_once_for_a_bank_alongside_an_unconditional_write() {
        let source =
            "module Top(input bit clk, clk2, en, input bit [31:0] data, output bit observed,
                         output bit [31:0] a,b,c, other);
            always_ff @(posedge clk) if (en) a <= data;
            always_ff @(posedge clk) if (en) b <= a;
            always_ff @(posedge clk) if (en) c <= b;
            always_ff @(posedge clk) other <= c;
            always_ff @(posedge clk2) observed <= ^other;
            endmodule";
        let sir = schedule(source, true, DeadStorePolicy::Off);
        let blocks = sir
            .eval_only_ffs
            .values()
            .flatten()
            .flat_map(|unit| unit.blocks.values())
            .collect::<Vec<_>>();
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b.terminator, SIRTerminator::Branch { .. }))
                .count(),
            1
        );
        assert!(
            !blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .any(|op| matches!(op, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn comb_driver_reaches_a_clocked_sink_in_a_testbench() {
        let source = r#"
            module Top(input bit clk, input bit [3:0] data, output bit [3:0] q);
                wire [7:0] padded = {data, 4'b0};
                always_ff @(posedge clk) q <= padded[3:0];
                initial begin #10; $display("%h", q); end
            endmodule
        "#;
        let loads = |graph| {
            let sir = schedule(source, graph, DeadStorePolicy::PreserveTopPorts);
            if !graph {
                assert!(sir.eval_comb_apply_ffs.is_empty());
            }
            sir.eval_apply_ffs
                .values()
                .flatten()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count()
        };
        assert!(loads(false) > 0);
        assert_eq!(loads(true), 0);
    }

    #[test]
    fn an_edge_reuses_an_existing_comb_sum_in_a_testbench() {
        let source = r#"
            module Top(input bit clk, input bit [7:0] a,b, output bit [7:0] q, y);
                wire [7:0] left = a ^ 8'h25;
                wire [7:0] right = b ^ 8'h87;
                wire [7:0] sum = left + right;
                assign y = sum;
                always_ff @(posedge clk) q <= right + left;
                initial begin #10; $display("%h", q); end
            endmodule
        "#;
        let edge_adds = |graph, policy| {
            let sir = schedule(source, graph, policy);
            if !graph {
                assert!(sir.eval_comb_apply_ffs.is_empty());
            }
            sir.eval_apply_ffs
                .values()
                .flatten()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Add, _)))
                .count()
        };
        assert_eq!(edge_adds(false, DeadStorePolicy::PreserveTopPorts), 1);
        assert_eq!(edge_adds(true, DeadStorePolicy::PreserveTopPorts), 0);
        assert_eq!(edge_adds(true, DeadStorePolicy::Off), 1);
    }

    #[test]
    fn shared_next_value_is_computed_once_before_publication() {
        let source = r#"
            module Top(input bit clk, input bit [7:0] a, b,
                       output bit [7:0] q, r, sum, delayed);
                assign sum = a + b;
                always_ff @(posedge clk) delayed <= sum;
                always_ff @(posedge clk) q <= a + b;
                always_ff @(posedge clk) r <= b + a;
            endmodule
        "#;
        let additions = |graph| {
            let sir = schedule(source, graph, DeadStorePolicy::Off);
            assert_eq!(!sir.eval_comb_apply_ffs.is_empty(), graph);
            let ticks = if graph {
                &sir.eval_comb_apply_ffs
            } else {
                &sir.eval_apply_ffs
            };
            ticks
                .values()
                .flatten()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Add, _)))
                .count()
        };
        assert_eq!(additions(false), 2);
        assert_eq!(additions(true), 1);
    }

    #[test]
    fn conditional_clock_reads_reuse_an_independently_observed_value() {
        use celox_design::BinaryOp;
        use celox_sir::SIRInstruction;

        let source = r#"
module Top(input bit src, a, b, ena, enb, input bit [7:0] d,
           output bit [7:0] qa, qb, output int changes);
    bit [7:0] x;
    always_ff @(posedge src) x <= d;
    wire [7:0] product = x * x;
    always_ff @(posedge a) qa <= ena ? product : 8'd0;
    always_ff @(posedge b) qb <= enb ? product : 8'd0;
    OBSERVER
endmodule"#;
        for observed in [false, true] {
            let watch = if observed {
                "initial forever begin @(product); changes++; end"
            } else {
                ""
            };
            let source = source.replace("OBSERVER", watch);
            let sir = schedule(&source, true, DeadStorePolicy::PreserveTopPorts);
            let products = sir
                .eval_apply_ffs
                .values()
                .flatten()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
                .count();
            assert_eq!(
                products,
                if observed { 0 } else { 2 },
                "observed={observed}"
            );
        }
    }
}
