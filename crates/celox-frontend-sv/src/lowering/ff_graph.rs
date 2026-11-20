//! Graph-first lowering of packed register updates. Processes with one
//! nonblocking write per destination along a path share their trigger's next-state DAG.
//! A procedural evaluation ends a batch because it may change stable state.
use super::*;
use celox_frontend_core::symbolic::artifact::NextStateForms;
use celox_sir::RegisterId;
use celox_slt::{SLTToSIRLowerer, canon::Reinterner};
use comb::NextState;

#[derive(Default)]
pub(super) struct FfGraph {
    pub forms: NextStateForms,
    pub arena: SLTNodeArena<SourceVarId>,
    pub schedule: EdgeSchedule,
    pub pending: HashMap<TriggerSet<SourceVarId>, NextState>,
}

impl FfGraph {
    pub fn try_add(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        trigger: &TriggerSet<SourceVarId>,
        body: &[sv::ir::Stmt],
        excluded: &HashSet<String>,
    ) -> Option<()> {
        if !register_updates(pm, body, excluded, &mut HashSet::default()) {
            return None;
        }
        // The symbolic store uses scratch variables for next values. Only
        // the expressions of original signals may escape this lowering.
        let checkpoint = pm.checkpoint();
        let first_temp = pm.next_id;
        let mut raw = SLTNodeArena::new();
        let form = comb::Comb::new(pm, &mut raw).lower_next_state(body);
        pm.rollback(checkpoint);
        let form = form.ok()?;
        let reads_temporary = celox_slt::canon::reachable(&raw, form.nodes()).into_iter().any(|node| {
            matches!(raw.get(node), SLTNode::Input { variable, .. } if variable.0 >= first_temp.0)
        });
        if !form.ports.is_empty() || form.roots.is_empty() || reads_temporary {
            return None;
        }
        let form = canonical_copy(&raw, &mut self.arena, &form).ok()?;
        self.pending
            .entry(trigger.clone())
            .or_default()
            .roots
            .extend(form.roots.iter().cloned());
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
                segments.push(EdgeBatch::Procedural(unit));
            }
            let targets: Vec<_> = form
                .roots
                .iter()
                .map(|root| VarAtomBase {
                    id: root.target,
                    access: root.access,
                })
                .collect();
            let mut commit = SIRBuilder::new();
            emit_ff_commits(&mut commit, &targets, pm);
            let commit = seal_builder(commit);
            segments.push(EdgeBatch::Registers(form));
            ff_processes::insert_or_merge_ff_unit(applications, trigger.clone(), commit.clone());
            // All the writes in this batch publish in the NBA phase.
            active
                .entry(trigger.clone())
                .or_insert_with(|| seal_builder(SIRBuilder::new()));
            ff_processes::insert_or_merge_ff_unit(nba, trigger.clone(), commit);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct EdgeSchedule {
    batches: HashMap<TriggerSet<SourceVarId>, Vec<EdgeBatch>>,
}

#[derive(Clone)]
enum EdgeBatch {
    Procedural(ExecutionUnit<RegionedVarAddr>),
    Registers(NextState),
}

impl EdgeSchedule {
    pub fn contains(&self, trigger: &TriggerSet<SourceVarId>) -> bool {
        self.batches.contains_key(trigger)
    }

    /// Emit edge evaluations only once their combinational producers are in
    /// the module graph. A procedural batch forms an ordering boundary.
    pub fn lower(
        &self,
        pm: &procedural::ProcModule<'_>,
        module: &mut SimModule,
        wires: &HashSet<SourceVarId>,
    ) -> Result<(), String> {
        if self.batches.is_empty() {
            return Ok(());
        }
        let values = super::dataflow::optimize(module, wires)?;
        for (trigger, batches) in &self.batches {
            let tail = module.eval_only_ff_blocks.remove(trigger);
            let mut evals = Vec::new();
            for batch in batches {
                let eval = match batch {
                    EdgeBatch::Procedural(unit) => unit.clone(),
                    EdgeBatch::Registers(form) => {
                        let form = form.map(|id| *id, |node| Ok::<_, String>(values[&node]))?;
                        next_state_unit(pm, &module.arena, &form, WORKING_REGION)?
                    }
                };
                evals.push(eval);
            }
            evals.extend(tail);
            let mut eval = if evals.len() == 1 {
                evals.pop().unwrap()
            } else {
                merge_sir_eus(&evals).0
            };
            ff::prune_unreachable_blocks(&mut eval);
            let direct = if let [EdgeBatch::Registers(form)] = batches.as_slice()
                && module.next_state.complete.contains(trigger)
            {
                let form = form.map(|id| *id, |node| Ok::<_, String>(values[&node]))?;
                next_state_unit(pm, &module.arena, &form, STABLE_REGION)?
            } else {
                let mut combined =
                    celox_sir::merge_sir_eu_refs(&[&eval, &module.apply_ff_blocks[trigger]]).0;
                ff::prune_unreachable_blocks(&mut combined);
                combined
            };
            module.eval_only_ff_blocks.insert(trigger.clone(), eval);
            module.eval_apply_ff_blocks.insert(trigger.clone(), direct);
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
            nonblocking: true,
        } => {
            let name = lhs.name();
            let Some(id) = pm.id(name) else { return false };
            let var = pm.var(id);
            let static_target = match lhs {
                sv::ir::LValue::Ident(_) => true,
                sv::ir::LValue::Select { msb, lsb, .. } => [msb, lsb].into_iter().all(|bound| {
                    sv::typecheck::eval_const_expr_with_types(
                        bound,
                        pm.constants,
                        pm.parameter_types,
                    )
                    .is_some()
                }),
            };
            static_target
                && !var.hidden
                && var.array_dims.is_empty()
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
    output_region: u32,
) -> Result<ExecutionUnit<RegionedVarAddr>, String> {
    let mut mapped = SLTNodeArena::<RegionedVarAddr>::new();
    let mut cache = HashMap::default();
    let stable = |var_id| RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    };
    let map = |id: &SourceVarId| stable(*id);
    let mut mapped_nodes = Vec::new();
    for node in form.nodes() {
        mapped_nodes.push(
            dag.get(node)
                .map_addr(node, dag, &mut mapped, &mut cache, &map)
                .map_err(|error| error.to_string())?,
        );
    }
    let element_widths: HashMap<RegionedVarAddr, usize> = pm
        .variables
        .iter()
        .filter_map(|(&id, variable)| Some((stable(id), unpacked_element_width(variable)?)))
        .collect();
    let mut lowerer =
        SLTToSIRLowerer::new(pm.four_state).with_unpacked_input_types(&mapped, &element_widths);
    let mut b = SIRBuilder::<RegionedVarAddr>::new();
    let mut lowered = HashMap::default();
    // A common enable is one decision for the entire bank. Sample active
    // data only on the enabled edge. A split evaluation must still leave
    // the old values in its working slots for the later commit.
    let enabled = (!pm.four_state && form.ports.is_empty())
        .then(|| {
            let mut enable = None;
            let mut active = Vec::with_capacity(mapped_nodes.len());
            for (root, node) in form.roots.iter().zip(&mapped_nodes) {
                let SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } = *mapped.get(*node)
                else {
                    return None;
                };
                let holds = |node| {
                    matches!(mapped.get(node), SLTNode::Input {variable, access, index, ..}
                if *variable == stable(root.target) && *access == root.access && index.is_empty())
                };
                let (polarity, data) = if holds(else_expr) {
                    (true, then_expr)
                } else if holds(then_expr) {
                    (false, else_expr)
                } else {
                    return None;
                };
                if enable
                    .replace((cond, polarity))
                    .is_some_and(|old| old != (cond, polarity))
                {
                    return None;
                }
                active.push(data);
            }
            Some((enable?, active))
        })
        .flatten();
    let mut join = None;
    if let Some(((condition, polarity), active)) = enabled {
        let cond = lowerer.lower(&mut b, condition, &mapped, &mut lowered);
        let taken = b.new_block();
        let done = b.new_block();
        let hold = if output_region == WORKING_REGION {
            b.new_block()
        } else {
            done
        };
        let (yes, no) = if polarity {
            (taken, hold)
        } else {
            (hold, taken)
        };
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (yes, Vec::new()),
            false_block: (no, Vec::new()),
        });
        if hold != done {
            b.switch_to_block(hold);
            let targets: Vec<_> = form
                .roots
                .iter()
                .map(|root| VarAtomBase {
                    id: root.target,
                    access: root.access,
                })
                .collect();
            emit_ff_seeds(&mut b, &targets);
            b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        }
        b.switch_to_block(taken);
        mapped_nodes = active;
        join = Some(done);
    }
    let registers: Vec<RegisterId> = mapped_nodes
        .into_iter()
        .map(|node| lowerer.lower(&mut b, node, &mapped, &mut lowered))
        .collect();
    let (root_registers, port_registers) = registers.split_at(form.roots.len());
    for (root, &value) in form.roots.iter().zip(root_registers) {
        let width = root.access.msb - root.access.lsb + 1;
        if b.register(&value).width() != width {
            return Err(format!("root of {width} bits has a value of another width"));
        }
        b.emit(SIRInstruction::Store(
            RegionedVarAddrBase {
                region: output_region,
                var_id: root.target,
            },
            SIROffset::Static(root.access.lsb),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    for (port, registers) in form.ports.iter().zip(port_registers.chunks(3)) {
        let &[index, value, guard] = registers else {
            unreachable!("a port computes three nodes");
        };
        let write = b.new_block();
        let next = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond: guard,
            true_block: (write, Vec::new()),
            false_block: (next, Vec::new()),
        });
        b.switch_to_block(write);
        b.emit(SIRInstruction::Store(
            RegionedVarAddrBase {
                region: output_region,
                var_id: port.array,
            },
            SIROffset::Element {
                index,
                element_width: port.element_width,
                bit_offset: port.access.lsb,
                dynamic_bit_offset: None,
            },
            port.access.msb - port.access.lsb + 1,
            value,
            Vec::new(),
            Vec::new(),
        ));
        b.seal_block(SIRTerminator::Jump(next, Vec::new()));
        b.switch_to_block(next);
    }
    if let Some(done) = join {
        b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        b.switch_to_block(done);
    }
    Ok(seal_builder(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::{DeadStorePolicy, RegionedStateAddr, StateAddr};

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
            assert!(sir.eval_comb_apply_ffs.is_empty());
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
            assert!(sir.eval_comb_apply_ffs.is_empty());
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
}
