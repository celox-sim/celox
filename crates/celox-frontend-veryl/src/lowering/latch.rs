//! Persistent combinational activations for inferred latches.

use crate::{HashMap, SourceVarId, VariableKind};
use celox_design::{DomainKind, PortTypeKind, VarAtomBase, VariableMetadata};
use celox_sir::ExecutionUnit;
use celox_slt::{LogicPath, SLTNodeArena};

/// Retained values belong to a procedural activation, rather than a
/// combinational fixed point. Evaluate every output before publishing any of
/// them, and exclude written expressions from the implicit sensitivity list
/// (IEEE 1800-2023 9.2.2.2). Hidden snapshots also make repeated host evaluations
/// inert when the process's inputs have not changed.
pub(crate) fn lower_retained_comb_process(
    paths: &mut [LogicPath<SourceVarId>],
    arena: &SLTNodeArena<SourceVarId>,
    variables: &mut HashMap<SourceVarId, crate::symbolic::artifact::SymbolicVariable>,
    authored_reads: crate::HashSet<VarAtomBase<SourceVarId>>,
) -> bool {
    use celox_design::{BinaryOp, UnaryOp};
    use celox_sir::{BlockId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue};
    use celox_slt::{LogicPathTarget, SLTToSIRLowerer};

    let targets: Vec<_> = paths
        .iter()
        .map(|path| match path.target {
            LogicPathTarget::Var(target) => target,
            _ => unreachable!("authored combinational outputs are variable projections"),
        })
        .collect();
    let mut inputs = crate::HashSet::default();
    for path in paths.iter() {
        crate::symbolic::flattening::collect_inputs(path.expr, arena, &mut inputs);
    }
    if !targets.iter().any(|target| {
        inputs
            .iter()
            .any(|input| input.id == target.id && input.access.overlaps(&target.access))
    }) {
        return false;
    }
    let dependencies = crate::logic_tree::subtract_written_sensitivity(authored_reads, &targets);
    let mut sources: Vec<_> = dependencies.iter().copied().collect();
    sources.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
    let element_widths: HashMap<_, _> = variables
        .iter()
        .filter_map(|(&id, variable)| {
            let elements: usize = variable.metadata.array_dims.iter().product();
            (!variable.metadata.array_dims.is_empty())
                .then_some((id, variable.metadata.width / elements))
        })
        .collect();
    let source_four_state: HashMap<_, _> = variables
        .iter()
        .map(|(&id, var)| (id, var.metadata.is_4state))
        .collect();
    let mut next_id = variables.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let mut temp = |width, four_state| {
        let id = SourceVarId(next_id);
        next_id += 1;
        variables.insert(
            id,
            crate::symbolic::artifact::SymbolicVariable {
                path: vec![format!("__celox_comb_{id}")],
                kind: VariableKind::Variable,
                signed: false,
                metadata: VariableMetadata {
                    width,
                    is_4state: four_state,
                    kind: DomainKind::Other,
                    type_kind: PortTypeKind::Logic,
                    array_dims: Vec::new(),
                },
                packed_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                source: None,
                module_affiliated: false,
            },
        );
        id
    };
    let initialized = temp(1, false);
    let mut b = SIRBuilder::new();
    let init = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Load(
        init,
        initialized,
        SIROffset::Static(0),
        1,
    ));
    let mut run = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Unary(run, UnaryOp::LogicNot, init));
    let mut sensitivity_counters = Vec::new();
    for source in sources {
        let width = source.access.msb - source.access.lsb + 1;
        let saved = temp(width, source_four_state[&source.id]);
        let current = b.alloc_logic(width);
        let previous = b.alloc_logic(width);
        let offset = match element_widths.get(&source.id) {
            Some(&element_width)
                if width > element_width
                    && source.access.lsb.is_multiple_of(element_width)
                    && width.is_multiple_of(element_width) =>
            {
                SIROffset::PackedElements {
                    bit_offset: source.access.lsb,
                    element_width,
                }
            }
            _ => SIROffset::Static(source.access.lsb),
        };
        b.emit(SIRInstruction::Load(current, source.id, offset, width));
        b.emit(SIRInstruction::Load(
            previous,
            saved,
            SIROffset::Static(0),
            width,
        ));
        let changed = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(
            changed,
            current,
            BinaryOp::NeCase,
            previous,
        ));
        let next = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(next, run, BinaryOp::Or, changed));
        run = next;
        let bits = celox_design::EVENT_COUNTER_WIDTH;
        let counter = temp(bits, false);
        let seen = temp(bits, false);
        let current_count = b.alloc_bit(bits, false);
        let previous_count = b.alloc_bit(bits, false);
        b.emit(SIRInstruction::Load(
            current_count,
            counter,
            SIROffset::Static(0),
            bits,
        ));
        b.emit(SIRInstruction::Load(
            previous_count,
            seen,
            SIROffset::Static(0),
            bits,
        ));
        let changed = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(
            changed,
            current_count,
            BinaryOp::NeCase,
            previous_count,
        ));
        let next = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(next, run, BinaryOp::Or, changed));
        run = next;
        b.emit(SIRInstruction::Store(
            seen,
            SIROffset::Static(0),
            bits,
            current_count,
            Vec::new(),
            Vec::new(),
        ));
        sensitivity_counters.push((source, counter));
        b.emit(SIRInstruction::Store(
            saved,
            SIROffset::Static(0),
            width,
            current,
            Vec::new(),
            Vec::new(),
        ));
    }
    let one = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    b.emit(SIRInstruction::Store(
        initialized,
        SIROffset::Static(0),
        1,
        one,
        Vec::new(),
        Vec::new(),
    ));
    let execute = b.new_block();
    let done = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond: run,
        true_block: (execute, Vec::new()),
        false_block: (done, Vec::new()),
    });
    b.switch_to_block(execute);
    let lowerer = SLTToSIRLowerer::new(true).with_unpacked_input_types(arena, &element_widths);
    let mut cache = HashMap::default();
    let values: Vec<_> = paths
        .iter()
        .map(|path| lowerer.lower(&mut b, path.expr, arena, &mut cache))
        .collect();
    for (target, value) in targets.iter().zip(values) {
        let width = target.access.msb - target.access.lsb + 1;
        let offset = match element_widths.get(&target.id) {
            Some(&element_width)
                if width > element_width
                    && target.access.lsb.is_multiple_of(element_width)
                    && width.is_multiple_of(element_width) =>
            {
                SIROffset::PackedElements {
                    bit_offset: target.access.lsb,
                    element_width,
                }
            }
            _ => SIROffset::Static(target.access.lsb),
        };
        b.emit(SIRInstruction::Store(
            target.id,
            offset,
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    b.seal_block(SIRTerminator::Jump(done, Vec::new()));
    b.switch_to_block(done);
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    let unit = ExecutionUnit {
        blocks,
        register_map,
        entry_block_id: BlockId(0),
    };
    for (path, target) in paths.iter_mut().zip(targets) {
        path.target = LogicPathTarget::Process {
            process: initialized,
            after_initials: true,
            sensitivity_counters: sensitivity_counters.clone(),
            startup: None,
            target,
            unit: Box::new(unit.clone()),
        };
        path.sources = dependencies.clone();
        path.previous_sources.clear();
    }
    true
}
