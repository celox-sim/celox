use std::{collections::BTreeSet, fmt};

use celox_design::{
    DomainKind, InitialStateValue, PortTypeKind, RegionedVarAddrBase, RuntimeErrorInfo,
    RuntimeEventSite, TriggerSet, VarAtomBase, VariableMetadata,
};
use celox_sir::{BasicBlock, ExecutionUnit};
use celox_slt::{
    CombObserver, FfAccessSummary, GlueBlockBase, LogicPath, NodeId, SLTNodeArena, SymbolicStore,
};
use veryl_analyzer::ir::{VarId, VarPath, Variable};
use veryl_analyzer::symbol::Affiliation;
use veryl_metadata::{ClockType, ResetType};
use veryl_parser::resource_table::StrId;

use crate::{
    BuildConfig, HashMap, ParserError, SourceVarId, VariableKind,
    veryl::lowering::{bitaccess, types::resolve_total_width},
};

type RegionedVarAddr = RegionedVarAddrBase<VarId>;
type GlueBlock = GlueBlockBase<VarId>;

/// Veryl-owned sidecar retained only until source identities and testbench AST
/// nodes have been projected into the source-neutral symbolic core.
pub struct VerylSymbolicRtl<'a> {
    pub symbolic: crate::symbolic::artifact::SymbolicRtl,
    pub module_ir: HashMap<celox_design::ModuleId, &'a veryl_analyzer::ir::Module>,
    pub source_id_maps: HashMap<celox_design::ModuleId, HashMap<VarId, SourceVarId>>,
}

#[derive(Clone)]
pub struct VerylSimModule {
    pub name: StrId,
    pub variables: HashMap<VarId, Variable>,
    pub ff_access_summaries: HashMap<TriggerSet<VarId>, FfAccessSummary<RegionedVarAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    /// Source instance declarations that explicitly have an array dimension.
    pub indexed_instance_names: crate::HashSet<StrId>,
    pub comb_blocks: Vec<LogicPath<VarId>>,
    pub comb_observers: Vec<CombObserver<VarId>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<VarId>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub initial_memory_values: Vec<InitialStateValue<VarId>>,
    pub comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    pub arena: SLTNodeArena<VarId>,
    pub store: SymbolicStore<VarId, NodeId>,
    /// Maps reset VarId to clock VarId, derived from FF declarations.
    pub reset_clock_map: HashMap<VarId, VarId>,
}

impl fmt::Debug for VerylSimModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimModule")
            .field("name", &self.name)
            .field("variables", &"<omitted>")
            .field("ff_access_summaries", &self.ff_access_summaries)
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("glue_blocks", &self.glue_blocks)
            .field("indexed_instance_names", &self.indexed_instance_names)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_boundaries", &self.comb_boundaries)
            .field("arena", &self.arena)
            .field("store", &self.store)
            .field("reset_clock_map", &self.reset_clock_map)
            .finish()
    }
}

impl VerylSimModule {
    pub fn find_var_id(&self, path: &VarPath) -> VarId {
        self.variables
            .iter()
            .find(|(_, variable)| &variable.path == path)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("Variable '{path}' not found in module"))
    }
}

fn variable_kind(kind: veryl_analyzer::ir::VarKind) -> VariableKind {
    use veryl_analyzer::ir::VarKind;
    match kind {
        VarKind::Param => VariableKind::Parameter,
        VarKind::Const => VariableKind::Constant,
        VarKind::Input => VariableKind::Input,
        VarKind::Output => VariableKind::Output,
        VarKind::Inout => VariableKind::Inout,
        VarKind::Variable => VariableKind::Variable,
        VarKind::Let => VariableKind::Let,
    }
}

fn port_kind(kind: &veryl_analyzer::ir::TypeKind, config: &BuildConfig) -> PortTypeKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ClockNegedge => PortTypeKind::Clock,
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => PortTypeKind::ResetAsyncHigh,
            ResetType::AsyncLow => PortTypeKind::ResetAsyncLow,
            ResetType::SyncHigh => PortTypeKind::ResetSyncHigh,
            ResetType::SyncLow => PortTypeKind::ResetSyncLow,
        },
        TypeKind::ResetAsyncHigh => PortTypeKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => PortTypeKind::ResetAsyncLow,
        TypeKind::ResetSyncHigh => PortTypeKind::ResetSyncHigh,
        TypeKind::ResetSyncLow => PortTypeKind::ResetSyncLow,
        TypeKind::Logic => PortTypeKind::Logic,
        TypeKind::Bit => PortTypeKind::Bit,
        _ => PortTypeKind::Other,
    }
}

fn domain_kind(kind: &veryl_analyzer::ir::TypeKind, config: &BuildConfig) -> DomainKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock => match config.clock_type {
            ClockType::PosEdge => DomainKind::ClockPosedge,
            ClockType::NegEdge => DomainKind::ClockNegedge,
        },
        TypeKind::ClockPosedge => DomainKind::ClockPosedge,
        TypeKind::ClockNegedge => DomainKind::ClockNegedge,
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => DomainKind::ResetAsyncHigh,
            ResetType::AsyncLow => DomainKind::ResetAsyncLow,
            ResetType::SyncHigh | ResetType::SyncLow => DomainKind::Other,
        },
        TypeKind::ResetAsyncHigh => DomainKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => DomainKind::ResetAsyncLow,
        _ => DomainKind::Other,
    }
}

fn is_4state(kind: &veryl_analyzer::ir::TypeKind) -> bool {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock
        | TypeKind::ClockPosedge
        | TypeKind::ClockNegedge
        | TypeKind::Reset
        | TypeKind::ResetAsyncHigh
        | TypeKind::ResetAsyncLow
        | TypeKind::ResetSyncHigh
        | TypeKind::ResetSyncLow
        | TypeKind::Logic => true,
        TypeKind::Struct(value) => value
            .members
            .iter()
            .any(|member| is_4state(&member.r#type.kind)),
        TypeKind::Union(value) => value
            .members
            .iter()
            .any(|member| is_4state(&member.r#type.kind)),
        _ => false,
    }
}

fn map_trigger(
    trigger: &TriggerSet<VarId>,
    ids: &HashMap<VarId, SourceVarId>,
) -> TriggerSet<SourceVarId> {
    TriggerSet {
        clock: ids[&trigger.clock],
        resets: trigger.resets.iter().map(|id| ids[id]).collect(),
    }
}

fn map_regioned(
    address: &RegionedVarAddr,
    ids: &HashMap<VarId, SourceVarId>,
) -> crate::symbolic::artifact::SymbolicRegionedAddr {
    crate::symbolic::artifact::SymbolicRegionedAddr {
        region: address.region,
        var_id: ids[&address.var_id],
    }
}

fn map_execution_unit(
    unit: &ExecutionUnit<RegionedVarAddr>,
    ids: &HashMap<VarId, SourceVarId>,
) -> ExecutionUnit<crate::symbolic::artifact::SymbolicRegionedAddr> {
    ExecutionUnit {
        entry_block_id: unit.entry_block_id,
        blocks: unit
            .blocks
            .iter()
            .map(|(&id, block)| {
                (
                    id,
                    BasicBlock {
                        id: block.id,
                        params: block.params.clone(),
                        instructions: block
                            .instructions
                            .iter()
                            .map(|instruction| instruction.map_addr(|addr| map_regioned(addr, ids)))
                            .collect(),
                        terminator: block.terminator.clone(),
                    },
                )
            })
            .collect(),
        register_map: unit.register_map.clone(),
    }
}

fn map_observer(
    observer: &CombObserver<VarId>,
    source_arena: &SLTNodeArena<VarId>,
    target_arena: &mut SLTNodeArena<SourceVarId>,
    cache: &mut HashMap<NodeId, NodeId>,
    ids: &HashMap<VarId, SourceVarId>,
) -> Result<CombObserver<SourceVarId>, celox_slt::SLTNodeFactsError> {
    let map_id = |id: &VarId| ids[id];
    let mut map_node = |node| {
        source_arena
            .get(node)
            .map_addr(node, source_arena, target_arena, cache, &map_id)
    };
    let map_atoms = |atoms: &[VarAtomBase<VarId>]| {
        atoms
            .iter()
            .map(|atom| VarAtomBase::new(ids[&atom.id], atom.access.lsb, atom.access.msb))
            .collect()
    };
    Ok(CombObserver {
        site_id: observer.site_id,
        activation_group: observer.activation_group,
        guard: observer.guard.map(&mut map_node).transpose()?,
        args: observer
            .args
            .iter()
            .copied()
            .map(&mut map_node)
            .collect::<Result<_, _>>()?,
        loop_runner: observer.loop_runner.map(&mut map_node).transpose()?,
        sensitivity: map_atoms(&observer.sensitivity),
        local_inputs: observer
            .local_inputs
            .iter()
            .map(|(id, node)| Ok((ids[id], map_node(*node)?)))
            .collect::<Result<_, celox_slt::SLTNodeFactsError>>()?,
        observed_inputs: map_atoms(&observer.observed_inputs),
        position_inputs: map_atoms(&observer.position_inputs),
        preceding_writes: map_atoms(&observer.preceding_writes),
        written_before: map_atoms(&observer.written_before),
        written_input_atoms: map_atoms(&observer.written_input_atoms),
        written_inputs: observer.written_inputs.iter().map(|id| ids[id]).collect(),
        captured_in_loop: observer.captured_in_loop,
    })
}

/// Project analyzer-owned variable identities into the neutral symbolic domain.
pub(crate) fn project_id_map(
    ir: &veryl_analyzer::ir::Module,
) -> Result<HashMap<VarId, SourceVarId>, ParserError> {
    let mut source_variables = ir.variables.keys().copied().collect::<Vec<_>>();
    source_variables.sort_unstable();
    source_variables
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let source_id = SourceVarId(u32::try_from(index).map_err(|_| {
                ParserError::illegal_context(
                    "frontend source identity projection",
                    "module has more than u32::MAX variables",
                    None,
                )
            })?);
            Ok((id, source_id))
        })
        .collect()
}

pub(crate) fn project_module_with_ids(
    module: &VerylSimModule,
    ir: &veryl_analyzer::ir::Module,
    config: &BuildConfig,
    ids: HashMap<VarId, SourceVarId>,
    child_ids: &HashMap<celox_design::ModuleId, HashMap<VarId, SourceVarId>>,
) -> Result<
    (
        crate::symbolic::artifact::SimModule,
        HashMap<VarId, SourceVarId>,
    ),
    ParserError,
> {
    let mut source_variables = ir.variables.iter().collect::<Vec<_>>();
    source_variables.sort_unstable_by_key(|(id, _)| **id);

    let variables = source_variables
        .into_iter()
        .map(|(id, variable)| {
            let (dimensions, _, _) = bitaccess::get_dimensions_and_strides(ir, *id)?;
            let packed_dims = dimensions
                .into_iter()
                .skip(variable.r#type.array.iter().count())
                .collect();
            Ok((
                ids[id],
                crate::symbolic::artifact::SymbolicVariable {
                    path: variable
                        .path
                        .0
                        .iter()
                        .map(|part| {
                            veryl_parser::resource_table::get_str_value(*part).unwrap_or_default()
                        })
                        .collect(),
                    kind: variable_kind(variable.kind),
                    signed: variable.r#type.signed,
                    metadata: VariableMetadata {
                        width: resolve_total_width(ir, variable)?,
                        is_4state: is_4state(&variable.r#type.kind),
                        kind: domain_kind(&variable.r#type.kind, config),
                        type_kind: port_kind(&variable.r#type.kind, config),
                        array_dims: variable
                            .r#type
                            .array
                            .iter()
                            .filter_map(|value| *value)
                            .collect(),
                    },
                    packed_dims,
                    source: Some(celox_frontend_core::SourceLocation {
                        path: variable.token.beg.source.to_string(),
                        text: variable.token.beg.source.get_text(),
                        span: (&variable.token).into(),
                    }),
                    module_affiliated: variable.affiliation == Affiliation::Module,
                },
            ))
        })
        .collect::<Result<HashMap<_, _>, ParserError>>()?;

    let mut arena = SLTNodeArena::new();
    let mut cache = HashMap::default();
    let map_id = |id: &VarId| ids[id];
    let comb_blocks = module
        .comb_blocks
        .iter()
        .map(|path| path.map_addr(&module.arena, &mut arena, &mut cache, &map_id))
        .collect::<Result<_, _>>()?;
    let comb_observers = module
        .comb_observers
        .iter()
        .map(|observer| map_observer(observer, &module.arena, &mut arena, &mut cache, &ids))
        .collect::<Result<_, _>>()?;

    let map_units = |units: &HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>| {
        units
            .iter()
            .map(|(trigger, unit)| (map_trigger(trigger, &ids), map_execution_unit(unit, &ids)))
            .collect()
    };

    Ok((
        crate::symbolic::artifact::SimModule {
            name: veryl_parser::resource_table::get_str_value(module.name).unwrap_or_default(),
            variables,
            ff_access_summaries: module
                .ff_access_summaries
                .iter()
                .map(|(trigger, summary)| {
                    (
                        map_trigger(trigger, &ids),
                        FfAccessSummary {
                            reads: summary
                                .reads
                                .iter()
                                .map(|atom| {
                                    VarAtomBase::new(
                                        map_regioned(&atom.id, &ids),
                                        atom.access.lsb,
                                        atom.access.msb,
                                    )
                                })
                                .collect(),
                            writes: summary
                                .writes
                                .iter()
                                .map(|atom| {
                                    VarAtomBase::new(
                                        map_regioned(&atom.id, &ids),
                                        atom.access.lsb,
                                        atom.access.msb,
                                    )
                                })
                                .collect(),
                            dynamic_writes: summary
                                .dynamic_writes
                                .iter()
                                .map(|address| map_regioned(address, &ids))
                                .collect(),
                        },
                    )
                })
                .collect(),
            eval_only_ff_blocks: map_units(&module.eval_only_ff_blocks),
            apply_ff_blocks: map_units(&module.apply_ff_blocks),
            eval_apply_ff_blocks: map_units(&module.eval_apply_ff_blocks),
            glue_blocks: module
                .glue_blocks
                .iter()
                .map(|(name, blocks)| {
                    let name =
                        veryl_parser::resource_table::get_str_value(*name).unwrap_or_default();
                    let blocks = blocks
                        .iter()
                        .map(|block| {
                            let child = child_ids.get(&block.module_id).unwrap_or(&ids);
                            crate::symbolic::remap::glue_block(block, &ids, child)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok((name, blocks))
                })
                .collect::<Result<_, celox_slt::SLTNodeFactsError>>()?,
            indexed_instance_names: module
                .indexed_instance_names
                .iter()
                .map(|name| veryl_parser::resource_table::get_str_value(*name).unwrap_or_default())
                .collect(),
            comb_blocks,
            comb_observers,
            runtime_errors: module
                .runtime_errors
                .iter()
                .map(|(&code, info)| {
                    (
                        code,
                        RuntimeErrorInfo {
                            message: info.message.clone(),
                            signals: info.signals.iter().map(|id| ids[id]).collect(),
                        },
                    )
                })
                .collect(),
            runtime_event_sites: module.runtime_event_sites.clone(),
            initial_memory_values: module
                .initial_memory_values
                .iter()
                .map(|value| InitialStateValue {
                    address: ids[&value.address],
                    data: value.data.clone(),
                })
                .collect(),
            comb_boundaries: module
                .comb_boundaries
                .iter()
                .map(|(id, boundaries)| (ids[id], boundaries.clone()))
                .collect(),
            arena,
            reset_clock_map: module
                .reset_clock_map
                .iter()
                .map(|(reset, clock)| (ids[reset], ids[clock]))
                .collect(),
        },
        ids,
    ))
}
