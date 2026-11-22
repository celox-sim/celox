use crate::logic_tree::node::SLTNodeArena;
use std::collections::BTreeSet;

use super::ExternalModule;
use crate::{
    BuildConfig, HashMap, HashSet, LoweringPhase, ModuleInitialMemoryValue, ParserError,
    RegionedVarAddr, SimModule,
    bitaccess::{
        eval_var_select, eval_var_select_with_geometry, is_static_access, select_geometry,
    },
    bitslicer::BitSlicer,
    ff::FfParser,
    logic_tree::{
        CombEffectCollector, SymbolicStore, apply_assignment_destination, coerce_node_width,
        collect_and_advance_expression, collect_written_expression,
        eval_assignment_expression_effectful, eval_expression, expression_contains_runtime_effect,
        get_width, parse_comb_with_loop_recovery, subtract_written_sensitivity,
    },
    loop_provenance::{LoopProvenance, LoopRecoveryCandidate},
    registry::get_port_type,
    resolve_total_width,
};
use celox_design::{
    BitAccess, InitialStateData as InitialMemoryData,
    InitialStateWriteRun as InitialMemoryWriteRun, ModuleId, RuntimeEventSite,
    SPARSE_WORKING_REGION, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
};
use celox_sir::{BlockId, ExecutionUnit, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator};
use celox_slt::{
    CombObserver, FfAccessSummary, LogicPath, LogicPathTarget, NodeId, RangeStore, SLTNode,
    SLTNodeArenaEditError,
};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, Component, Declaration, Expression, Factor,
    InstDeclaration, Module, Statement, SystemFunctionInput, SystemFunctionKind,
    SystemFunctionOutput, VarId,
};
use veryl_analyzer::value::Value;
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::StrId;

mod external_ports;
mod ff_parts;
mod initial_memory;
mod ports;
pub use external_ports::*;
use ff_parts::*;
pub(crate) use initial_memory::*;
use ports::*;

pub struct ModuleParser<'a> {
    module: &'a Module,
    inst_ids: &'a [ModuleId],
    inst_idx: usize,
    slicer: BitSlicer,
    store: SymbolicStore<VarId>,
    comb_blocks: Vec<LogicPath<VarId>>,
    comb_process_ranges: Vec<crate::artifact::CombProcess>,
    allow_inferred_latches: bool,
    four_state: bool,
    comb_observers: Vec<CombObserver<VarId>>,
    comb_runtime_event_sites: Vec<RuntimeEventSite>,
    comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    instances: HashMap<StrId, Vec<ModuleId>>,
    port_refs: Vec<crate::artifact::PortRef>,
    last_variable: VarId,
    initial_memory_values: Vec<ModuleInitialMemoryValue>,
    ff_parser: FfParser<'a>,
    arena: SLTNodeArena<VarId>,
    reset_clock_map: HashMap<VarId, VarId>,
    loop_candidates: Vec<LoopRecoveryCandidate>,
    external_modules: &'a HashMap<ModuleId, ExternalModule>,
    external_output_targets: Vec<(VarId, BitAccess)>,
    native_outputs: Vec<VarAtomBase<VarId>>,
    /// Lanes requested for partitioned execution.
    parallel_lanes: u32,
}

static EMPTY_EXTERNAL_MODULES: std::sync::LazyLock<HashMap<ModuleId, ExternalModule>> =
    std::sync::LazyLock::new(HashMap::default);

impl<'a> ModuleParser<'a> {
    pub fn parse(
        module: &'a Module,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(
            module,
            Vec::new(),
            config,
            inst_ids,
            &EMPTY_EXTERNAL_MODULES,
        )?;
        parser.parse_inner()
    }

    pub fn parse_with_loop_provenance(
        module: &'a Module,
        loop_provenance: &LoopProvenance,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(
            module,
            loop_provenance.candidates_for_module(module),
            config,
            inst_ids,
            &EMPTY_EXTERNAL_MODULES,
        )?;
        parser.parse_inner()
    }

    pub fn parse_with_loop_provenance_and_external_modules(
        module: &'a Module,
        loop_provenance: &LoopProvenance,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
        external_modules: &'a HashMap<ModuleId, ExternalModule>,
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(
            module,
            loop_provenance.candidates_for_module(module),
            config,
            inst_ids,
            external_modules,
        )?;
        parser.parse_inner()
    }

    fn new(
        module: &'a Module,
        loop_candidates: Vec<LoopRecoveryCandidate>,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
        external_modules: &'a HashMap<ModuleId, ExternalModule>,
    ) -> Result<Self, ParserError> {
        // Reads of constants and parameters that the analyzer did not fold
        // (dynamic array indices, selects of struct members) use ordinary
        // state loads. Retain the declared values in the initial state instead
        // of leaving zero/X.
        let mut initial_memory_values = Vec::new();
        for (&id, var) in &module.variables {
            if !matches!(
                var.kind,
                veryl_analyzer::ir::VarKind::Const | veryl_analyzer::ir::VarKind::Param
            ) || (var.r#type.array.dims() == 0 && var.value.is_empty())
            {
                continue;
            }
            let elements = var.r#type.total_array().ok_or_else(|| {
                ParserError::illegal_context(
                    "constant array",
                    "unresolved array shape",
                    Some(&var.token),
                )
            })?;
            let width = resolve_total_width(module, var)?;
            let element_width = width / elements.max(1);
            if element_width == 0 {
                continue;
            }
            let mut writes = Vec::new();
            for index in 0..elements {
                let value = var
                    .value
                    .get(index)
                    .or_else(|| (var.value.len() == 1).then(|| &var.value[0]))
                    .ok_or_else(|| {
                        ParserError::illegal_context(
                            "constant array",
                            "missing initial element",
                            Some(&var.token),
                        )
                    })?;
                let element_mask = (BigUint::from(1u8) << element_width) - 1u8;
                let mask = value.mask_xz().into_owned() & &element_mask;
                let payload = (value.payload().into_owned() ^ &mask) & &element_mask;
                let (payload, mask) = if var.r#type.is_2state() {
                    (payload & (element_mask ^ &mask), BigUint::from(0u8))
                } else {
                    (payload, mask)
                };
                // Backends copy whole bytes of a run, so give each run its
                // full width rather than the minimal `to_bytes_le` encoding.
                let element_bytes = element_width.div_ceil(8);
                let mut value_bytes = payload.to_bytes_le();
                value_bytes.resize(element_bytes, 0);
                let mut mask_bytes = mask.to_bytes_le();
                mask_bytes.resize(element_bytes, 0);
                writes.push(InitialMemoryWriteRun {
                    bit_offset: index * element_width,
                    bit_width: element_width,
                    value_bytes,
                    mask_bytes,
                });
            }
            initial_memory_values.push(ModuleInitialMemoryValue {
                address: id,
                data: InitialMemoryData::Writes(writes),
            });
        }
        Ok(Self {
            module,
            inst_ids,
            inst_idx: 0,
            slicer: BitSlicer::new(module)?,
            store: SymbolicStore::default(),
            comb_blocks: Vec::new(),
            comb_process_ranges: Vec::new(),
            allow_inferred_latches: config.allow_inferred_latches,
            four_state: config.four_state,
            comb_observers: Vec::new(),
            comb_runtime_event_sites: Vec::new(),
            comb_boundaries: HashMap::default(),
            instances: Default::default(),
            port_refs: Vec::new(),
            last_variable: module.variables.keys().copied().max().unwrap_or_default(),
            initial_memory_values,
            ff_parser: FfParser::new(module, config.clone()),
            parallel_lanes: config.parallel_lanes,
            arena: {
                let mut arena = SLTNodeArena::new();
                arena.reset_type = config.reset_type;
                arena
            },
            reset_clock_map: HashMap::default(),
            loop_candidates,
            external_modules,
            external_output_targets: Vec::new(),
            native_outputs: Vec::new(),
        })
    }

    fn parse_comb_declaration(
        &mut self,
        decl: &veryl_analyzer::ir::CombDeclaration,
    ) -> Result<(), ParserError> {
        let arena_start = self.arena.len();
        let site_offset = self.comb_runtime_event_sites.len() as u32;
        let (mut paths, store, boundaries, mut observers, sites) = parse_comb_with_loop_recovery(
            self.module,
            decl,
            &mut self.arena,
            &self.loop_candidates,
            site_offset,
        )?;
        for observer in &mut observers {
            observer.site_id += site_offset;
            observer.activation_group = site_offset;
        }
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, arena_start..arena_end, site_offset)?;
        self.store.extend(store);
        // Veryl's IR uses CombDeclaration for both continuous declarations
        // and procedural blocks. Continuous assignments retain their `assign`
        // (or `connect`/`let`) declaration token; procedural assignment tokens
        // start at their destination. Only procedural blocks may infer state.
        let continuous = decl.statements.iter().all(|statement| {
            matches!(statement, Statement::Assign(assign) if matches!(
                veryl_parser::resource_table::get_str_value(assign.token.beg.text).as_deref(),
                Some("assign" | "connect" | "let")
            ))
        });
        if self.allow_inferred_latches && !continuous {
            let mut proven_combinational =
                crate::logic_tree::proves_combinational(&paths, &self.arena);
            let proven_binary_combinational = !proven_combinational
                && crate::logic_tree::proves_binary_combinational(&paths, &self.arena);
            // A two-state selector has no value outside complete binary
            // labels, so the unmatched branch never runs.
            if proven_binary_combinational && !self.four_state {
                proven_combinational =
                    crate::logic_tree::complete_binary_cases(&mut paths, &mut self.arena);
            }
            self.comb_process_ranges.push(crate::artifact::CombProcess {
                paths: self.comb_blocks.len()..self.comb_blocks.len() + paths.len(),
                reads: crate::logic_tree::sensitivity::implicit_reads(
                    self.module,
                    &decl.statements,
                )?,
                token: decl
                    .statements
                    .iter()
                    .filter_map(|statement| match statement {
                        Statement::Assign(x) => Some(x.token),
                        Statement::If(x) => Some(x.token),
                        Statement::IfReset(x) => Some(x.token),
                        Statement::Case(x) => Some(x.token),
                        Statement::For(x) => Some(x.token),
                        _ => None,
                    })
                    .reduce(|mut range, token| {
                        if token.beg.pos < range.beg.pos {
                            range.beg = token.beg;
                        }
                        if token.end.pos + token.end.length > range.end.pos + range.end.length {
                            range.end = token.end;
                        }
                        range
                    })
                    .unwrap_or(self.module.token),
                proven_combinational,
                proven_binary_combinational,
            });
        }
        self.comb_blocks.extend(paths);
        self.comb_observers.extend(observers);
        self.comb_runtime_event_sites.extend(sites);
        for (id, bounds) in boundaries {
            self.comb_boundaries.entry(id).or_default().extend(bounds);
        }
        Ok(())
    }

    fn attach_connection_effects(
        &mut self,
        arena_start: usize,
        mut effects: CombEffectCollector,
        written_accesses: &HashMap<VarId, Vec<BitAccess>>,
        process_sensitivity: HashSet<VarAtomBase<VarId>>,
        preserved_address_sources: HashSet<VarAtomBase<VarId>>,
    ) -> Result<Vec<u32>, ParserError> {
        if effects.observers.is_empty() && effects.sites.is_empty() {
            return Ok(Vec::new());
        }

        let written_atoms: Vec<_> = written_accesses
            .iter()
            .flat_map(|(&id, accesses)| {
                accesses
                    .iter()
                    .map(move |access| VarAtomBase::new(id, access.lsb, access.msb))
            })
            .collect();
        let mut process_sensitivity =
            subtract_written_sensitivity(process_sensitivity, &written_atoms);
        process_sensitivity.extend(preserved_address_sources);
        let process_sensitivity: Vec<_> = process_sensitivity.into_iter().collect();
        for observer in &mut effects.observers {
            observer.sensitivity = process_sensitivity.clone();
            observer.written_input_atoms = observer
                .observed_inputs
                .iter()
                .chain(observer.position_inputs.iter())
                .copied()
                .filter(|atom| {
                    written_atoms.iter().any(|written| {
                        written.id == atom.id && written.access.overlaps(&atom.access)
                    })
                })
                .collect();
            observer.written_inputs = observer
                .written_input_atoms
                .iter()
                .map(|atom| atom.id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
        }

        let site_offset = self.comb_runtime_event_sites.len();
        for observer in &mut effects.observers {
            observer.site_id += site_offset as u32;
            observer.activation_group = site_offset as u32;
        }
        let site_ids = effects
            .observers
            .iter()
            .map(|observer| observer.site_id)
            .collect();
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, arena_start..arena_end, site_offset as u32)?;
        self.comb_observers.extend(effects.observers);
        self.comb_runtime_event_sites.extend(effects.sites);
        Ok(site_ids)
    }

    fn add_instance(&mut self, name: StrId, module: ModuleId) -> usize {
        let children = self.instances.entry(name).or_default();
        let index = children.len();
        children.push(module);
        index
    }

    fn bind_port(
        &mut self,
        name: StrId,
        index: usize,
        variable: VarId,
        width: usize,
        signed: bool,
    ) -> VarId {
        self.last_variable.inc();
        let operand = self.last_variable;
        self.port_refs.push(crate::artifact::PortRef {
            operand,
            name,
            index,
            variable,
            width,
            signed,
        });
        operand
    }

    fn parse_inst_declaration(
        &mut self,
        decl: &InstDeclaration,
        module_id: ModuleId,
    ) -> Result<(), ParserError> {
        if let Component::SystemVerilog(system_verilog) = &*decl.component {
            return self.parse_external_inst_declaration(decl, system_verilog.name, module_id);
        }

        let child_module = match &*decl.component {
            Component::Module(m) => m,
            _ => unreachable!(),
        };

        // 1. Inputs (Parent -> Child)
        let mut input_ports = Vec::new();
        let mut output_ports = Vec::new();
        let index = self.add_instance(decl.name, module_id);

        // Parent variables are implicit inputs until a connection expression
        // writes them, so instance parsing only needs sparse, touched entries.
        let parent_store = SymbolicStore::default();

        for input in &decl.inputs {
            let child_port_id = input.id;
            let ty = get_port_type(child_module, &child_port_id)?;
            let width = ty.width();
            let operand = self.bind_port(
                decl.name,
                index,
                child_port_id,
                width,
                child_module.variables[&child_port_id].r#type.signed,
            );
            let expression = &input.expr;
            if width == 0 {
                return Err(ParserError::illegal_context(
                    "input port connection",
                    "child input port has zero width",
                    Some(&expression.token_range()),
                ));
            }

            let mut written_accesses = HashMap::default();
            let mut connection_store = parent_store.clone();
            let mut output_address_sources = HashMap::default();
            collect_written_expression(self.module, expression, &mut written_accesses)?;
            collect_parent_output_address_sources(
                self.module,
                &connection_store,
                expression,
                &mut self.arena,
                &mut output_address_sources,
            )?;
            let ((expr_node, expr_sources), _bounds) = eval_assignment_expression_effectful(
                self.module,
                &mut connection_store,
                &crate::float::converted(
                    expression,
                    &child_module
                        .variables
                        .get(&child_port_id)
                        .expect("resolved child input")
                        .r#type,
                ),
                &mut self.arena,
                width,
            )?;

            if expression_contains_runtime_effect(self.module, expression) {
                let arena_start = self.arena.len();
                let mut effects = CombEffectCollector::with_capture_namespace(
                    self.comb_runtime_event_sites.len() as u32,
                );
                let mut effect_store = SymbolicStore::default();
                for (&id, variable) in &self.module.variables {
                    effect_store.insert(
                        id,
                        RangeStore::new(None, resolve_total_width(self.module, variable)?),
                    );
                }
                let _ = collect_and_advance_expression(
                    self.module,
                    &mut effect_store,
                    expression,
                    &mut self.arena,
                    &mut effects,
                )?;

                let mut process_sensitivity = std::mem::take(&mut effects.sensitivity);
                process_sensitivity.extend(expr_sources.iter().copied());
                for (&id, accesses) in &written_accesses {
                    let Some(range_store) = connection_store.get(&id) else {
                        continue;
                    };
                    for &access in accesses {
                        for (value, _) in range_store.get_parts_ref(access).map_err(|error| {
                            ParserError::illegal_context(
                                "instance input function output",
                                error.to_string(),
                                Some(&expression.token_range()),
                            )
                        })? {
                            if let Some((_, sources)) = value {
                                process_sensitivity.extend(sources);
                            }
                        }
                    }
                }
                let preserved_address_sources = output_address_sources
                    .values()
                    .flat_map(|sources| sources.iter().copied())
                    .collect();
                let _ = self.attach_connection_effects(
                    arena_start,
                    effects,
                    &written_accesses,
                    process_sensitivity,
                    preserved_address_sources,
                )?;
            }

            let path = LogicPath {
                transparent_copy: true,
                target: LogicPathTarget::Var(VarAtomBase::new(operand, 0, width - 1)),
                expr: expr_node,
                sources: collect_port_sources(expr_node, &self.arena),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
            };

            input_ports.push(path);
            output_ports.extend(parent_effect_paths(
                &parent_store,
                &connection_store,
                &written_accesses,
                &output_address_sources,
                &mut self.arena,
            )?);
        }

        // 2. Outputs (Child -> Parent)
        for output in &decl.outputs {
            // The analyzer includes deliberately unconnected child outputs
            // with an empty destination list. They do not drive a parent variable;
            // width coverage applies only to connected destinations.
            if output.dst.is_empty() {
                continue;
            }
            let child_port_id = output.id;
            let output_port_start = output_ports.len();
            let ty = get_port_type(child_module, &child_port_id)?;
            let width = ty.width();
            let operand = self.bind_port(
                decl.name,
                index,
                child_port_id,
                width,
                child_module.variables[&child_port_id].r#type.signed,
            );
            if width == 0 {
                return Err(ParserError::illegal_context(
                    "output port connection",
                    "child output port has zero width",
                    output.dst.first().map(|destination| &destination.token),
                ));
            }
            let child_port = child_module.variables.get(&child_port_id).ok_or_else(|| {
                ParserError::illegal_context(
                    "output port connection",
                    "child output variable is absent from the semantic module",
                    output.dst.first().map(|destination| &destination.token),
                )
            })?;
            let rhs_node = self.arena.alloc(SLTNode::Input {
                variable: operand,
                signed: child_port.r#type.signed,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            })?;
            // LHS: output.dst (AssignDestination).
            let mut current_offset = 0usize;
            let mut destination_arena = SLTNodeArena::<VarId>::with_context_from(&self.arena);
            let mut destination_store = SymbolicStore::default();
            let preview_rhs_node = destination_arena.alloc(SLTNode::Input {
                variable: operand,
                signed: child_port.r#type.signed,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            })?;
            let mut destination_written_accesses = HashMap::default();
            let mut destination_address_sources = HashMap::default();
            let composed_output_accesses = Vec::new();
            let output_effect_arena_start = self.arena.len();
            let mut output_effects = CombEffectCollector::with_capture_namespace(
                self.comb_runtime_event_sites.len() as u32,
            );
            let mut output_effect_store = SymbolicStore::default();
            for (&id, variable) in &self.module.variables {
                output_effect_store.insert(
                    id,
                    RangeStore::new(None, resolve_total_width(self.module, variable)?),
                );
            }
            // A packed concat destination is MSB-first, so its last destination
            // receives the low bits. An unpacked-array slice has one destination
            // per child element in low-to-high order instead.
            let child_elements = child_port.r#type.total_array().unwrap_or(1);
            let element_wise = output.dst.len() > 1 && child_elements == output.dst.len();
            let destination_order: Vec<_> = if element_wise {
                (0..output.dst.len()).collect()
            } else {
                (0..output.dst.len()).rev().collect()
            };
            for destination_index in destination_order {
                let dst = &output.dst[destination_index];
                check_output_destination(dst)?;
                for address in dst.index.expressions().chain(dst.select.0.iter()) {
                    let address_sources = collect_and_advance_expression(
                        self.module,
                        &mut output_effect_store,
                        address,
                        &mut self.arena,
                        &mut output_effects,
                    )?;
                    output_effects.sensitivity.extend(address_sources);
                    collect_written_expression(
                        self.module,
                        address,
                        &mut destination_written_accesses,
                    )?;
                    collect_parent_output_address_sources(
                        self.module,
                        &destination_store,
                        address,
                        &mut destination_arena,
                        &mut destination_address_sources,
                    )?;
                }
                let geometry = select_geometry(self.module, dst.id, &dst.index, &dst.select)?;
                let prefix_access =
                    eval_var_select_with_geometry(&dst.index, &dst.select, &geometry)?;
                let conversion = if output.dst.len() == 1 && child_elements == 1 {
                    crate::float::conversion(&child_port.r#type, &dst.comptime.r#type)
                } else {
                    None
                };
                let part_width = if conversion.is_some() {
                    width
                } else {
                    geometry.selected_width
                };

                // Extract this part from rhs_node
                let slice_end = current_offset.checked_add(part_width).ok_or_else(|| {
                    ParserError::illegal_context(
                        "output port destination",
                        "concatenated destination width overflows usize",
                        Some(&dst.token),
                    )
                })?;
                if part_width == 0 || slice_end > width {
                    return Err(ParserError::illegal_context(
                        "output port destination",
                        format!(
                            "destination slice {current_offset}..{slice_end} does not fit output width {width}"
                        ),
                        Some(&dst.token),
                    ));
                }
                let slice_access = BitAccess::new(current_offset, slice_end - 1);

                let rhs_part = if slice_access.lsb == 0
                    && slice_access.msb == get_width(rhs_node, &self.arena) - 1
                {
                    rhs_node
                } else {
                    self.arena.alloc(SLTNode::Slice {
                        expr: rhs_node,
                        access: slice_access,
                    })?
                };
                let preview_rhs_part = if slice_access.lsb == 0
                    && slice_access.msb == get_width(preview_rhs_node, &destination_arena) - 1
                {
                    preview_rhs_node
                } else {
                    destination_arena.alloc(SLTNode::Slice {
                        expr: preview_rhs_node,
                        access: slice_access,
                    })?
                };
                let rhs_part = if let Some(op) = conversion {
                    self.arena.alloc(SLTNode::Unary(op, rhs_part))?
                } else {
                    rhs_part
                };
                let preview_rhs_part = if let Some(op) = conversion {
                    destination_arena.alloc(SLTNode::Unary(op, preview_rhs_part))?
                } else {
                    preview_rhs_part
                };
                let preview_rhs_sources: HashSet<_> = std::iter::once(VarAtomBase::new(
                    operand,
                    slice_access.lsb,
                    slice_access.msb,
                ))
                .collect();

                let mut sources = HashSet::default();
                sources.insert(VarAtomBase::new(operand, 0, width - 1));
                let (expr, access, previous_sources, address_sources) = (
                    rhs_part,
                    prefix_access,
                    HashSet::default(),
                    HashSet::default(),
                );

                let path = LogicPath {
                    transparent_copy: true,
                    target: LogicPathTarget::Var(VarAtomBase::new(dst.id, access.lsb, access.msb)),
                    sources,
                    previous_sources,
                    address_sources,
                    local_inputs: Vec::new(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr,
                };
                output_ports.push(path);

                {
                    let (next_store, _) = apply_assignment_destination(
                        self.module,
                        destination_store,
                        HashMap::default(),
                        dst,
                        preview_rhs_part,
                        preview_rhs_sources,
                        child_port.r#type.is_2state(),
                        &mut destination_arena,
                    )?;
                    destination_store = next_store;
                }

                // Runtime effects in a later destination execute after this
                // child slice has been written. Model that statement position
                // with a parent read: scheduling orders the observer after the
                // corresponding port assignment, and capture then sees the actual
                // child-driven value without introducing a child VarId into
                // the parent module arena.
                let effect_preview = self.arena.alloc(SLTNode::Input {
                    variable: dst.id,
                    signed: self.module.variables[&dst.id].r#type.signed,
                    index: vec![],
                    access,
                })?;
                let effect_sources =
                    std::iter::once(VarAtomBase::new(dst.id, access.lsb, access.msb)).collect();
                let parent_width =
                    resolve_total_width(self.module, &self.module.variables[&dst.id])?;
                output_effect_store
                    .entry(dst.id)
                    .or_insert_with(|| RangeStore::new(None, parent_width))
                    .update(access, Some((effect_preview, effect_sources)))
                    .map_err(|error| {
                        ParserError::illegal_context(
                            "output connection effect preview",
                            error.to_string(),
                            Some(&dst.token),
                        )
                    })?;

                current_offset = slice_end;
            }
            let process_sensitivity = std::mem::take(&mut output_effects.sensitivity);
            let preserved_address_sources = destination_address_sources
                .values()
                .flat_map(|sources| sources.iter().copied())
                .collect();
            let output_effect_site_ids = self.attach_connection_effects(
                output_effect_arena_start,
                output_effects,
                &destination_written_accesses,
                process_sensitivity,
                preserved_address_sources,
            )?;
            for path in &mut output_ports[output_port_start..] {
                path.comb_capture_enable_sites
                    .extend(output_effect_site_ids.iter().copied());
                path.comb_capture_enable_always = !output_effect_site_ids.is_empty();
            }
            let mut remaining_written_accesses = destination_written_accesses.clone();
            subtract_composed_accesses(&mut remaining_written_accesses, &composed_output_accesses);
            let effects = parent_effect_paths(
                &parent_store,
                &destination_store,
                &remaining_written_accesses,
                &destination_address_sources,
                &mut destination_arena,
            )?;
            let mut cache = HashMap::default();
            for effect in effects {
                output_ports.push(effect.map_addr(
                    &destination_arena,
                    &mut self.arena,
                    &mut cache,
                    &|id| *id,
                )?);
            }
            if current_offset != width {
                return Err(ParserError::illegal_context(
                    "output port destination",
                    format!(
                        "concatenated destinations cover {current_offset} bits, but child output has width {width}"
                    ),
                    output.dst.first().map(|dst| &dst.token),
                ));
            }
        }

        self.native_outputs.extend(
            output_ports
                .iter()
                .filter_map(|path| path.target.var().copied()),
        );
        self.comb_blocks.extend(input_ports);
        self.comb_blocks.extend(output_ports);
        Ok(())
    }

    fn parse_external_inst_declaration(
        &mut self,
        decl: &InstDeclaration,
        external_name: StrId,
        module_id: ModuleId,
    ) -> Result<(), ParserError> {
        let child = self.external_modules.get(&module_id).ok_or_else(|| {
            ParserError::MissingExternalModule {
                name: external_name.to_string(),
                source_location: None,
            }
        })?;
        let Component::SystemVerilog(system_verilog) = decl.component.as_ref() else {
            unreachable!();
        };
        let interface_ports = child
            .port_order
            .iter()
            .any(|id| child.sim_module.variables[id].path.len() > 1);
        if !interface_ports && child.port_order.len() != system_verilog.connects.len() {
            return Err(ParserError::illegal_context(
                "external module port connections",
                format!(
                    "module \"{external_name}\" has {} ports but the instance connects {}",
                    child.port_order.len(),
                    system_verilog.connects.len()
                ),
                None,
            ));
        }

        let mut input_ports = Vec::new();
        let mut output_ports = Vec::new();
        let index = self.add_instance(decl.name, module_id);
        let (mut uses_named_associations, mut formal_names) = external_port_formal_names(decl)?;
        if interface_ports {
            let mut groups = Vec::new();
            for id in &child.port_order {
                let group = child.sim_module.variables[id].path[0].clone();
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
            let mut expanded = Vec::new();
            let mut index = 0;
            for (group_index, formal) in formal_names.iter().enumerate() {
                let formal = if uses_named_associations {
                    formal.as_str()
                } else {
                    groups.get(group_index).ok_or_else(|| {
                        ParserError::illegal_context(
                            "external module interface connections",
                            "too many interface port associations",
                            Some(&decl.token),
                        )
                    })?
                };
                let Some(first) = system_verilog.connects.get(index) else {
                    return Err(ParserError::illegal_context(
                        "external module interface connections",
                        "missing interface connection",
                        Some(&decl.token),
                    ));
                };
                let is_interface = child.port_order.iter().any(|id| {
                    let path = &child.sim_module.variables[id].path;
                    path.len() > 1 && path[0] == formal
                });
                if is_interface {
                    let token = first.token;
                    while let Some(connection) = system_verilog.connects.get(index) {
                        if connection.token.beg.pos != token.beg.pos {
                            break;
                        }
                        let member = connection
                            .path
                            .0
                            .iter()
                            .skip(1)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("$");
                        if member.is_empty() {
                            return Err(ParserError::illegal_context(
                                "external module interface connections",
                                "interface port needs an interface instance",
                                Some(&connection.token),
                            ));
                        }
                        expanded.push(format!("{formal}${member}"));
                        index += 1;
                    }
                } else {
                    expanded.push(formal.to_string());
                    index += 1;
                }
            }
            formal_names = expanded;
            uses_named_associations = true;
        }
        if formal_names.len() != system_verilog.connects.len() {
            return Err(ParserError::illegal_context(
                "external module port connections",
                "could not preserve the named SystemVerilog port associations",
                Some(&decl.token),
            ));
        }
        let connections: HashMap<_, _> = if uses_named_associations {
            formal_names
                .into_iter()
                .zip(&system_verilog.connects)
                .collect()
        } else {
            HashMap::default()
        };

        for (port_index, child_port_id) in child.port_order.iter().enumerate() {
            let child_var = child
                .sim_module
                .variables
                .get(child_port_id)
                .ok_or_else(|| {
                    ParserError::illegal_context(
                        "external module port connections",
                        format!("port {child_port_id} is absent from module \"{external_name}\""),
                        None,
                    )
                })?;
            let parent_dst = if uses_named_associations {
                let formal_name = child_var.path.join("$");
                connections.get(formal_name.as_str()).ok_or_else(|| {
                    ParserError::illegal_context(
                        "external module port connections",
                        format!(
                            "port \"{formal_name}\" is not connected on module \"{external_name}\""
                        ),
                        Some(&decl.token),
                    )
                })?
            } else {
                &system_verilog.connects[port_index]
            };
            let child_width = child_var.metadata.width;
            if child_width == 0 {
                return Err(ParserError::illegal_context(
                    "external module port connections",
                    "child port has zero width",
                    Some(&parent_dst.token),
                ));
            }
            if child_var.kind == crate::VariableKind::Output {
                check_output_destination(parent_dst)?;
            }
            let parent_var = &self.module.variables[&parent_dst.id];
            let parent_access = eval_var_select(
                self.module,
                parent_dst.id,
                &parent_dst.index,
                &parent_dst.select,
            )?;
            let parent_width = parent_access.msb - parent_access.lsb + 1;
            let parent_expr_signed = parent_var.r#type.signed
                && parent_dst.select.is_empty()
                && parent_dst.select.1.is_none();

            let operand = self.bind_port(
                decl.name,
                index,
                VarId::from_raw(child_port_id.0),
                child_width,
                child_var.signed,
            );
            match child_var.kind {
                crate::VariableKind::Input => {
                    let parent_node = self.arena.alloc(SLTNode::Input {
                        variable: parent_dst.id,
                        signed: parent_expr_signed,
                        index: Vec::new(),
                        access: parent_access,
                    })?;
                    let mut expr = coerce_node_width(
                        &mut self.arena,
                        parent_node,
                        Some(child_width),
                        parent_expr_signed,
                    )?;
                    if !child_var.metadata.is_4state {
                        expr = self
                            .arena
                            .alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                    }
                    let mut sources = HashSet::default();
                    sources.insert(VarAtomBase::new(
                        parent_dst.id,
                        parent_access.lsb,
                        parent_access.msb,
                    ));
                    input_ports.push(LogicPath {
                        transparent_copy: true,
                        target: LogicPathTarget::Var(VarAtomBase::new(operand, 0, child_width - 1)),
                        expr,
                        sources,
                        previous_sources: HashSet::default(),
                        address_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    });
                }
                crate::VariableKind::Output => {
                    if parent_var.kind == veryl_analyzer::ir::VarKind::Input {
                        return Err(ParserError::illegal_context(
                            "external module output connections",
                            format!("child output cannot drive input port {}", parent_var.path),
                            Some(&parent_dst.token),
                        ));
                    }
                    if self
                        .external_output_targets
                        .iter()
                        .any(|(id, access)| *id == parent_dst.id && access.overlaps(&parent_access))
                    {
                        return Err(ParserError::illegal_context(
                            "external module output connections",
                            format!(
                                "multiple output ports drive overlapping target {}",
                                parent_var.path
                            ),
                            Some(&parent_dst.token),
                        ));
                    }
                    self.external_output_targets
                        .push((parent_dst.id, parent_access));
                    let child_node = self.arena.alloc(SLTNode::Input {
                        variable: operand,
                        signed: child_var.signed,
                        index: Vec::new(),
                        access: BitAccess::new(0, child_width - 1),
                    })?;
                    let mut expr = coerce_node_width(
                        &mut self.arena,
                        child_node,
                        Some(parent_width),
                        child_var.signed,
                    )?;
                    if parent_var.r#type.is_2state() {
                        expr = self
                            .arena
                            .alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                    }
                    let mut sources = HashSet::default();
                    sources.insert(VarAtomBase::new(operand, 0, child_width - 1));
                    output_ports.push(LogicPath {
                        transparent_copy: true,
                        target: LogicPathTarget::Var(VarAtomBase::new(
                            parent_dst.id,
                            parent_access.lsb,
                            parent_access.msb,
                        )),
                        expr,
                        sources,
                        previous_sources: HashSet::default(),
                        address_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    });
                }
                crate::VariableKind::Inout => {
                    return Err(ParserError::unsupported(
                        64,
                        LoweringPhase::SimulatorParser,
                        "external inout port",
                        child_var.path.join("."),
                        Some(&parent_dst.token),
                    ));
                }
                _ => {
                    return Err(ParserError::illegal_context(
                        "external module port connections",
                        format!(
                            "port {} has no input/output direction",
                            child_var.path.join(".")
                        ),
                        Some(&parent_dst.token),
                    ));
                }
            }
        }

        self.comb_blocks.extend(input_ports);
        self.comb_blocks.extend(output_ports);
        Ok(())
    }

    fn parse_initial_declaration(
        &mut self,
        decl: &veryl_analyzer::ir::InitialDeclaration,
    ) -> Result<(), ParserError> {
        let mut context = veryl_analyzer::Context::default();
        context.variables = self.module.variables.clone();
        for stmt in &decl.statements {
            self.parse_initial_statement(stmt, &mut context)?;
        }
        Ok(())
    }

    fn parse_initial_statement(
        &mut self,
        stmt: &Statement,
        context: &mut veryl_analyzer::Context,
    ) -> Result<(), ParserError> {
        visit_initial_statement(stmt, context, &mut |filename, output, context| {
            if let SystemFunctionOutput::Local(destinations) = output {
                let value = self.parse_readmem_file(filename, destinations, 16, context)?;
                self.initial_memory_values.push(value);
            }
            // Hierarchical destinations are resolved after instance elaboration.
            Ok(())
        })
    }

    fn parse_readmem_file(
        &self,
        filename_arg: &SystemFunctionInput,
        output: &[AssignDestination],
        radix: u32,
        context: &mut veryl_analyzer::Context,
    ) -> Result<ModuleInitialMemoryValue, ParserError> {
        // The file is read when a simulator instance starts (IEEE 1800-2023
        // 21.4), not when the design is compiled.
        let (address, load) = readmem_load(self.module, filename_arg, output, radix, |dst| {
            dst.index.eval_value(context)
        })?;
        Ok(ModuleInitialMemoryValue {
            address,
            data: InitialMemoryData::MemoryFile(load),
        })
    }

    /// Assignments in distinct clocked declarations sample the same old state.
    /// Lower their scalar expressions in the arena also used by wire drivers.
    fn scalar_event(
        &mut self,
        decls: &[&veryl_analyzer::ir::FfDeclaration],
    ) -> Result<Option<celox_slt::next_state::NextStateForm<VarId>>, ParserError> {
        use veryl_analyzer::ir::{Op, TypeKind};
        fn scalar(module: &Module, id: &VarId) -> bool {
            module.variables.get(id).is_some_and(|v| {
                matches!(v.r#type.kind, TypeKind::Bit | TypeKind::Logic)
                    && v.r#type.total_array().unwrap_or(1) == 1
                    && !v.r#type.signed
            })
        }
        fn pure(module: &Module, expr: &Expression) -> bool {
            match expr {
                Expression::Term(term) => match &**term {
                    Factor::Variable(id, index, select, _) => {
                        scalar(module, id)
                            && index.indices.is_empty()
                            && select.0.is_empty()
                            && select.1.is_none()
                    }
                    Factor::Value(v) => {
                        matches!(v.value, veryl_analyzer::ir::ValueVariant::Numeric(_))
                            && crate::float::precision(&v.r#type).is_none()
                    }
                    _ => false,
                },
                Expression::Binary(
                    a,
                    Op::Add | Op::Sub | Op::Mul | Op::BitAnd | Op::BitOr | Op::BitXor,
                    b,
                    _,
                ) => pure(module, a) && pure(module, b),
                _ => false,
            }
        }
        let mut targets = HashSet::default();
        let mut assignments = Vec::new();
        for decl in decls {
            let [Statement::Assign(assign)] = decl.statements.as_slice() else {
                return Ok(None);
            };
            let [dst] = assign.dst.as_slice() else {
                return Ok(None);
            };
            if decl.reset.is_some()
                || !scalar(self.module, &dst.id)
                || !dst.index.indices.is_empty()
                || !dst.select.0.is_empty()
                || dst.select.1.is_some()
                || !targets.insert(dst.id)
                || !pure(self.module, &assign.expr)
            {
                return Ok(None);
            }
            assignments.push((dst.id, &assign.expr));
        }
        let mut form = celox_slt::next_state::NextStateForm::default();
        for (id, expr) in assignments {
            let width = resolve_total_width(self.module, &self.module.variables[&id])?;
            let ((value, _), _) = eval_expression(
                self.module,
                &SymbolicStore::default(),
                expr,
                &mut self.arena,
                Some(width),
            )?;
            let value = coerce_node_width(
                &mut self.arena,
                value,
                Some(width),
                crate::context_width::expression_signed(expr),
            )?;
            form.roots.push(celox_slt::next_state::ValueRoot {
                target: id,
                value,
                access: BitAccess::new(0, width - 1),
            });
        }
        Ok(Some(form))
    }

    fn parse_inner(mut self) -> Result<SimModule, ParserError> {
        crate::value_type_check::check_module(self.module)?;
        let mut ff_groups: HashMap<TriggerSet<VarId>, Vec<&veryl_analyzer::ir::FfDeclaration>> =
            HashMap::default();

        // 1. Parse all declarations
        for decl in self.module.declarations.iter() {
            match decl {
                Declaration::Ff(ff_decl) => {
                    let trigger_set = self.ff_parser.detect_trigger_set(ff_decl);
                    ff_groups.entry(trigger_set).or_default().push(ff_decl);
                    // Build reset -> clock mapping
                    if let Some(reset) = &ff_decl.reset {
                        self.reset_clock_map.insert(reset.id, ff_decl.clock.id);
                    }
                }
                Declaration::Comb(comb_decl) => {
                    self.parse_comb_declaration(comb_decl)?;
                }
                Declaration::Inst(inst_decl) => {
                    let mid = self.inst_ids[self.inst_idx];
                    self.inst_idx += 1;
                    self.parse_inst_declaration(inst_decl, mid)?;
                }
                Declaration::Initial(init_decl) => {
                    self.parse_initial_declaration(init_decl)?;
                }
                _ => {}
            }
        }

        // Sample once per source trigger. Assembly orders publications across instances.
        let mut event_bodies = HashMap::default();
        let mut ff_access_summaries = HashMap::default();

        let mut event_graphs = HashMap::default();
        for (trigger_set, decls) in &ff_groups {
            if !self.four_state
                && let Some(form) = self.scalar_event(decls)?
            {
                let mut publication = SIRBuilder::new();
                for root in &form.roots {
                    let target = root.target;
                    publication.emit(SIRInstruction::Commit(
                        RegionedVarAddr {
                            region: WORKING_REGION,
                            var_id: target,
                        },
                        RegionedVarAddr {
                            region: STABLE_REGION,
                            var_id: target,
                        },
                        SIROffset::Static(root.access.lsb),
                        root.access.msb - root.access.lsb + 1,
                        Vec::new(),
                    ));
                }
                publication.seal_block(SIRTerminator::Return);
                let (blocks, register_map, _) = publication.drain();
                event_bodies.insert(
                    trigger_set.clone(),
                    crate::symbolic::artifact::EventBody {
                        sample: None,
                        publish: ExecutionUnit {
                            blocks,
                            register_map,
                            entry_block_id: BlockId(0),
                        },
                    },
                );
                event_graphs.insert(trigger_set.clone(), form);
                continue;
            }
            let mut builder = SIRBuilder::new();
            let ff_group = self.ff_parser.parse_ff_group(decls, &mut builder)?;
            let targets = ff_group.targets;
            let sources = ff_group.sources;
            let dynamic_write_vars = ff_group.dynamic_write_vars;
            ff_access_summaries.insert(
                trigger_set.clone(),
                FfAccessSummary {
                    reads: sources,
                    writes: targets.clone(),
                    dynamic_writes: dynamic_write_vars
                        .iter()
                        .copied()
                        .map(|var_id| RegionedVarAddr {
                            region: WORKING_REGION,
                            var_id,
                        })
                        .collect(),
                },
            );
            let mut commits = build_ff_region_copies_skipping(
                &targets,
                WORKING_REGION,
                STABLE_REGION,
                &dynamic_write_vars,
            );
            commits.extend(build_sparse_ff_commits(&targets, &dynamic_write_vars));

            builder.seal_block(SIRTerminator::Return);
            let (bbs, regs, _) = builder.drain();
            let mut eval_only_eu = ExecutionUnit {
                blocks: bbs,
                entry_block_id: BlockId(0),
                register_map: regs,
            };

            rewrite_dynamic_ff_stores_to_sparse(&mut eval_only_eu, &dynamic_write_vars);

            let seeds = build_ff_region_copies_skipping(
                &targets,
                STABLE_REGION,
                WORKING_REGION,
                &dynamic_write_vars,
            );
            if let Some(entry) = eval_only_eu.blocks.get_mut(&BlockId(0)) {
                let mut s = seeds;
                s.append(&mut entry.instructions);
                entry.instructions = s;
            }

            // --- apply: minimal EU containing only commit instructions ---
            let mut apply_builder = SIRBuilder::new();
            for commit in &commits {
                apply_builder.emit(commit.clone());
            }
            apply_builder.seal_block(SIRTerminator::Return);
            let (apply_bbs, apply_regs, _) = apply_builder.drain();
            let apply_eu = ExecutionUnit {
                blocks: apply_bbs,
                entry_block_id: BlockId(0),
                register_map: apply_regs,
            };

            event_bodies.insert(
                trigger_set.clone(),
                crate::symbolic::artifact::EventBody {
                    sample: Some(eval_only_eu),
                    publish: apply_eu,
                },
            );
        }

        // Lane-partitioned builds also lower every trigger group as several
        // contiguous parts. Each FF declaration samples pre-edge state and
        // stages only its own targets, so parts are independent until they
        // publish. The sequential units above stay unchanged.
        let mut parallel_ff_parts = HashMap::default();
        if self.parallel_lanes > 1 {
            const PARTS_PER_LANE: usize = 4;
            let part_limit = PARTS_PER_LANE.saturating_mul(self.parallel_lanes as usize);
            for (trigger_set, decls) in &ff_groups {
                let part_count = decls.len().min(part_limit);
                if part_count < 2 {
                    continue;
                }
                let mut parts = Vec::with_capacity(part_count);
                for part in 0..part_count {
                    let begin = decls.len() * part / part_count;
                    let end = decls.len() * (part + 1) / part_count;
                    let mut builder = SIRBuilder::new();
                    let ff_group = self
                        .ff_parser
                        .parse_ff_group(&decls[begin..end], &mut builder)?;
                    parts.push(build_ff_part(builder, &ff_group));
                }
                parallel_ff_parts.insert(trigger_set.clone(), parts);
            }
        }

        for (external_id, external_access) in &self.external_output_targets {
            let overlaps_comb = self
                .comb_blocks
                .iter()
                .filter(|path| !path.transparent_copy)
                .any(|path| {
                    path.target.var().is_some_and(|target| {
                        target.id == *external_id && target.access.overlaps(external_access)
                    })
                });
            let overlaps_ff = ff_access_summaries.values().any(|summary| {
                summary.writes.iter().any(|target| {
                    target.id.var_id == *external_id && target.access.overlaps(external_access)
                }) || summary
                    .dynamic_writes
                    .iter()
                    .any(|target| target.var_id == *external_id)
            });
            let overlaps_child_output = self
                .native_outputs
                .iter()
                .any(|target| target.id == *external_id && target.access.overlaps(external_access));
            let overlaps_graph = event_graphs.values().any(|form| {
                form.roots.iter().any(|root| {
                    root.target == *external_id && root.access.overlaps(external_access)
                })
            });
            if overlaps_comb || overlaps_ff || overlaps_graph || overlaps_child_output {
                let variable = &self.module.variables[external_id];
                let driver = if overlaps_child_output {
                    "child output driver"
                } else {
                    "local driver"
                };
                return Err(ParserError::illegal_context(
                    "external module output connections",
                    format!("external output overlaps {driver} for {}", variable.path,),
                    None,
                ));
            }
        }

        // Keep both boundary sources:
        // - BitSlicer: assignment destination-based split points
        // - parse_comb: expression/read-driven split points
        let mut comb_boundaries = self.slicer.boundaries().clone();
        for (id, bounds) in self.comb_boundaries {
            comb_boundaries.entry(id).or_default().extend(bounds);
        }
        let ff_site_count = self.ff_parser.runtime_event_sites().len() as u32;
        for observer in &mut self.comb_observers {
            observer.site_id += ff_site_count;
            observer.activation_group += ff_site_count;
        }
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, 0..arena_end, ff_site_count)?;
        let mut runtime_event_sites = self.ff_parser.runtime_event_sites().clone();
        runtime_event_sites.extend(self.comb_runtime_event_sites);
        let mut variable_widths = HashMap::default();
        let mut variable_signedness = HashMap::default();
        for (id, variable) in &self.module.variables {
            variable_widths.insert(*id, resolve_total_width(self.module, variable)?);
            variable_signedness.insert(*id, variable.r#type.signed);
        }
        for port in &self.port_refs {
            variable_widths.insert(port.operand, port.width);
            variable_signedness.insert(port.operand, port.signed);
        }
        celox_slt::verify_symbolic_roots(
            &self.arena,
            &self.comb_blocks,
            &self.comb_observers,
            &variable_widths,
            &variable_signedness,
        )
        .map_err(|error| ParserError::SltVerify {
            phase: "after module symbolic lowering",
            error,
        })?;
        Ok(SimModule {
            variables: self.module.variables.clone(),
            name: self.module.name,
            instances: self.instances,
            port_refs: self.port_refs,
            indexed_instance_names: HashSet::default(),
            ff_access_summaries,
            event_graphs,
            event_bodies,
            parallel_ff_parts,
            comb_blocks: self.comb_blocks,
            comb_process_ranges: self.comb_process_ranges,
            comb_observers: self.comb_observers,
            runtime_errors: self.ff_parser.runtime_errors().clone(),
            runtime_event_sites,
            memory_file_bases: self.ff_parser.memory_file_bases().to_vec(),
            // The loads of `always_comb` blocks run at time zero, after the
            // `initial` blocks (IEEE 1800-2023 9.2.2.2).
            initial_memory_values: {
                let mut values = self.initial_memory_values;
                values.append(&mut self.arena.memory_loads);
                values
            },
            comb_boundaries,
            arena: self.arena.into_inner(),
            store: self.store,
            reset_clock_map: self.reset_clock_map,
        })
    }
}

#[cfg(test)]
mod tests;
