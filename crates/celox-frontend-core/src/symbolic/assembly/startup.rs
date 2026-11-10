//! Startup readiness: gating `always_comb` units on the initial processes.

use super::*;

/// always_comb starts after every initial and always procedure has reached its
/// first suspension or completion (IEEE 1800-2023 9.2.2.2). The unit runs once
/// `ready` ([`startup_ready_path`]) is set.
pub(super) fn after_initial_processes(
    unit: &ExecutionUnit<AbsoluteAddr>,
    ready: AbsoluteAddr,
) -> ExecutionUnit<AbsoluteAddr> {
    use celox_sir::{SIRBuilder, SIROffset};
    let mut b = SIRBuilder::new();
    let started = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Load(
        started,
        ready,
        SIROffset::Static(0),
        1,
    ));
    let execute = b.new_block();
    let done = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond: started,
        true_block: (execute, vec![]),
        false_block: (done, vec![]),
    });
    b.switch_to_block(execute);
    b.inline_execution_unit(unit);
    b.seal_block(SIRTerminator::Jump(done, vec![]));
    b.switch_to_block(done);
    b.flush_eu()
        .expect("process activation gate has instructions")
}

/// `ready = (|status_0) && (|status_1) && ...`: initial status is zero until
/// a process first suspends or completes; all control slots are ordinary
/// checkpointed state. Only this path reads the statuses, so their writes
/// mark one partition rather than every gated unit.
pub(super) fn startup_ready_path(
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    statuses: &[AbsoluteAddr],
    ready: AbsoluteAddr,
) -> Result<celox_slt::LogicPath<AbsoluteAddr>, ParserError> {
    let width = celox_design::PROCESS_STATUS_WIDTH;
    let mut expr = None;
    let mut sources = HashSet::default();
    for &status in statuses {
        let input = arena.alloc(celox_slt::SLTNode::Input {
            variable: status,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, width - 1),
        })?;
        let started = arena.alloc(celox_slt::SLTNode::Unary(celox_design::UnaryOp::Or, input))?;
        expr = Some(match expr {
            None => started,
            Some(previous) => arena.alloc(celox_slt::SLTNode::Binary(
                previous,
                celox_design::BinaryOp::And,
                started,
            ))?,
        });
        sources.insert(VarAtomBase::new(status, 0, width - 1));
    }
    let expr = expr.expect("a startup gate has statuses");
    Ok(celox_slt::LogicPath {
        target: celox_slt::LogicPathTarget::Var(VarAtomBase::new(ready, 0, 0)),
        transparent_copy: false,
        sources,
        previous_sources: HashSet::default(),
        address_sources: HashSet::default(),
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr,
    })
}

/// Declare the startup readiness bit read by gated `always_comb` units.
pub(super) fn declare_startup_ready(
    modules: &mut HashMap<ModuleId, SimModule>,
    root: ModuleId,
    var_id: SourceVarId,
) {
    let variables = &mut modules.get_mut(&root).expect("top module").variables;
    variables.insert(
        var_id,
        crate::symbolic::artifact::SymbolicVariable {
            path: vec!["$startup".to_string(), "ready".to_string()],
            kind: crate::VariableKind::Variable,
            signed: false,
            metadata: VariableMetadata {
                width: 1,
                is_4state: false,
                kind: DomainKind::Other,
                type_kind: celox_design::PortTypeKind::Bit,
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
            packed_dims: vec![1],
            unpacked_ranges: Vec::new(),
            source: None,
            module_affiliated: false,
        },
    );
}
