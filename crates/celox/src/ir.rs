use crate::HashMap;
pub use celox_design::PortTypeKind;
pub(crate) use celox_design::{
    AbsoluteAddrBase, BitAccess, InstanceId, ModuleId, RegionedAbsoluteAddrBase,
    RegionedVarAddrBase, RuntimeSchema, SPARSE_WORKING_REGION, STABLE_REGION, VarAtomBase,
    WORKING_REGION,
};
#[cfg(test)]
pub(crate) use celox_design::{BinaryOp, UnaryOp};
#[cfg(feature = "host-runtime")]
pub(crate) use celox_design::{
    InitialStateData, InitialStateWriteRun, RuntimeEventKind, RuntimeEventSite,
};
pub use celox_frontend_core::shared::{
    FrontendLookup, InstancePath, SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
#[cfg(all(
    feature = "host-runtime",
    any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )
))]
use celox_runtime::{
    DesignReflection, ReflectionScope, ReflectionScopeId, ReflectionSignal, ReflectionSignalId,
    SignalDirection,
};
#[cfg(test)]
pub(crate) use celox_sir::{BasicBlock, SIRValue, inline_single_predecessor_jumps};
pub(crate) use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    collect_exact_zero_registers,
};
use celox_testbench::TestbenchProgram;
use std::{fmt, ops::Deref};

/// Source-independent identity of one elaborated state object.
pub type AbsoluteAddr = celox_design::StateAddr;
/// Source-independent state identity qualified by its storage region.
pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;

/// Source-facing metadata for one flattened runtime state object.
///
/// Storage metadata remains canonical in [`RuntimeDesign::semantic`]; this
/// record only retains the hierarchy and source properties needed for lookup,
/// diagnostics, testbench integration, and reflection.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RuntimeVariable {
    pub address: AbsoluteAddr,
    pub source_id: SourceVarId,
    pub path: Vec<String>,
    pub var_kind: VariableKind,
    pub signed: bool,
    pub packed_dims: Vec<usize>,
}

/// One elaborated runtime instance with direct state-address indices.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RuntimeInstance {
    pub id: InstanceId,
    pub module_id: ModuleId,
    pub module_name: String,
    pub path: InstancePath,
    pub display_path: Vec<String>,
    state_addresses: Vec<AbsoluteAddr>,
    source_variables: HashMap<SourceVarId, AbsoluteAddr>,
    path_index: HashMap<Vec<String>, Option<AbsoluteAddr>>,
}

impl RuntimeInstance {
    pub fn state_addresses(&self) -> &[AbsoluteAddr] {
        &self.state_addresses
    }

    pub fn resolves_path_to(&self, path: &[String], address: AbsoluteAddr) -> bool {
        self.path_index.get(path) == Some(&Some(address))
    }
}

/// Canonical runtime design model after frontend scheduling.
///
/// The semantic state table, hierarchy, paths, and source-facing variable
/// properties are projected into this model once. The compiler can then drop
/// [`FrontendLookup`] instead of retaining it beside a duplicate state table.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RuntimeDesign {
    semantic: celox_design::ElaboratedDesign<AbsoluteAddr>,
    instances: HashMap<InstanceId, RuntimeInstance>,
    instance_ids: HashMap<InstancePath, InstanceId>,
    variables: HashMap<AbsoluteAddr, RuntimeVariable>,
}

impl std::ops::Deref for RuntimeDesign {
    type Target = celox_design::ElaboratedDesign<AbsoluteAddr>;

    fn deref(&self) -> &Self::Target {
        &self.semantic
    }
}

impl RuntimeDesign {
    fn from_projection(
        semantic: celox_design::ElaboratedDesign<AbsoluteAddr>,
        frontend: FrontendLookup,
    ) -> Result<Self, DesignProjectionError> {
        let expected_count = frontend
            .instance_module
            .values()
            .map(|module_id| frontend.module_variables[module_id].len())
            .sum::<usize>();
        if semantic.state_objects.len() != expected_count {
            return Err(DesignProjectionError::StateObjectCount {
                design: semantic.state_objects.len(),
                frontend: expected_count,
            });
        }

        let mut instances = HashMap::default();
        let mut variables = HashMap::default();
        for (path, &instance_id) in &frontend.instance_ids {
            let module_id = frontend.instance_module[&instance_id];
            let module_variables = &frontend.module_variables[&module_id];
            let display_path = frontend.instance_path_segments(path);
            let mut state_addresses = Vec::with_capacity(module_variables.len());
            let mut source_variables = HashMap::default();

            for info in module_variables.values() {
                let source_address = SourceAddr {
                    instance_id,
                    var_id: info.id,
                };
                let Some(address) = frontend.state_address(&source_address) else {
                    return Err(DesignProjectionError::MissingStateProjection { source_address });
                };
                let Some(metadata) = semantic.state_objects.get(&address) else {
                    return Err(DesignProjectionError::MissingStateObject { address });
                };
                if metadata != &info.metadata {
                    return Err(DesignProjectionError::MetadataMismatch { address });
                }

                state_addresses.push(address);
                source_variables.insert(info.id, address);
                variables.insert(
                    address,
                    RuntimeVariable {
                        address,
                        source_id: info.id,
                        path: info.path.clone(),
                        var_kind: info.var_kind,
                        signed: info.signed,
                        packed_dims: info.packed_dims.clone(),
                    },
                );
            }

            state_addresses.sort_unstable();
            let path_index = frontend.module_var_path_index[&module_id]
                .iter()
                .map(|(path, source_id)| {
                    (
                        path.clone(),
                        source_id.and_then(|source_id| source_variables.get(&source_id).copied()),
                    )
                })
                .collect();
            instances.insert(
                instance_id,
                RuntimeInstance {
                    id: instance_id,
                    module_id,
                    module_name: frontend
                        .module_names
                        .get(&module_id)
                        .cloned()
                        .unwrap_or_else(|| module_id.to_string()),
                    path: path.clone(),
                    display_path,
                    state_addresses,
                    source_variables,
                    path_index,
                },
            );
        }

        let design = Self {
            semantic,
            instances,
            instance_ids: frontend.instance_ids,
            variables,
        };
        design
            .validate()
            .map_err(|reason| DesignProjectionError::InvalidRuntimeDesign { reason })?;
        Ok(design)
    }

    pub fn semantic(&self) -> &celox_design::ElaboratedDesign<AbsoluteAddr> {
        &self.semantic
    }

    pub fn instances(&self) -> impl Iterator<Item = &RuntimeInstance> {
        self.instances.values()
    }

    pub fn instance(&self, id: InstanceId) -> Option<&RuntimeInstance> {
        self.instances.get(&id)
    }

    pub fn instance_at_path(&self, path: &InstancePath) -> Option<&RuntimeInstance> {
        self.instance_ids
            .get(path)
            .and_then(|instance_id| self.instances.get(instance_id))
    }

    pub fn root_instance(&self) -> Option<&RuntimeInstance> {
        self.instance_at_path(&InstancePath(Vec::new()))
    }

    pub fn variable(&self, address: &AbsoluteAddr) -> Option<&RuntimeVariable> {
        self.variables.get(address)
    }

    pub fn instance_variable(
        &self,
        instance_id: InstanceId,
        source_id: SourceVarId,
    ) -> Option<&RuntimeVariable> {
        let address = self
            .instances
            .get(&instance_id)?
            .source_variables
            .get(&source_id)?;
        self.variables.get(address)
    }

    pub fn variable_info(&self, address: &AbsoluteAddr) -> Option<VariableInfo> {
        let variable = self.variables.get(address)?;
        Some(VariableInfo {
            id: variable.source_id,
            path: variable.path.clone(),
            var_kind: variable.var_kind,
            signed: variable.signed,
            metadata: self.semantic.state_objects.get(address)?.clone(),
            packed_dims: variable.packed_dims.clone(),
        })
    }

    pub fn get_path(&self, address: &AbsoluteAddr) -> String {
        let Some(variable) = self.variables.get(address) else {
            return address.to_string();
        };
        let Some(instance) = self.instances.get(&address.instance_id) else {
            return address.to_string();
        };
        instance
            .display_path
            .iter()
            .chain(&variable.path)
            .cloned()
            .collect::<Vec<_>>()
            .join(".")
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.variables.len() != self.semantic.state_objects.len() {
            return Err(format!(
                "state variable count differs: design={} runtime={}",
                self.semantic.state_objects.len(),
                self.variables.len()
            ));
        }
        if self.instance_ids.len() != self.instances.len() {
            return Err(format!(
                "instance count differs: paths={} instances={}",
                self.instance_ids.len(),
                self.instances.len()
            ));
        }

        for (path, instance_id) in &self.instance_ids {
            let instance = self.instances.get(instance_id).ok_or_else(|| {
                format!("instance path {path:?} references missing {instance_id}")
            })?;
            if instance.path != *path || instance.id != *instance_id {
                return Err(format!("instance path index disagrees for {instance_id}"));
            }
        }

        for (instance_id, instance) in &self.instances {
            if instance.id != *instance_id {
                return Err(format!("instance map key disagrees for {instance_id}"));
            }
            if self.instance_ids.get(&instance.path) != Some(instance_id) {
                return Err(format!("missing path index for {instance_id}"));
            }
            if instance.state_addresses.len() != instance.source_variables.len() {
                return Err(format!(
                    "state/source variable count differs for {instance_id}"
                ));
            }
            if instance
                .state_addresses
                .windows(2)
                .any(|addresses| addresses[0] >= addresses[1])
            {
                return Err(format!(
                    "state addresses are not strictly sorted for {instance_id}"
                ));
            }

            for address in &instance.state_addresses {
                if address.instance_id != *instance_id {
                    return Err(format!(
                        "state address {address} belongs to another instance"
                    ));
                }
                if !self.semantic.state_objects.contains_key(address) {
                    return Err(format!("state address {address} has no semantic metadata"));
                }
                let variable = self
                    .variables
                    .get(address)
                    .ok_or_else(|| format!("state address {address} has no runtime variable"))?;
                if variable.address != *address
                    || instance.source_variables.get(&variable.source_id) != Some(address)
                {
                    return Err(format!("source index disagrees for {address}"));
                }
            }

            for (path, address) in &instance.path_index {
                let Some(address) = address else {
                    continue;
                };
                let variable = self
                    .variables
                    .get(address)
                    .ok_or_else(|| format!("path index references missing {address}"))?;
                if address.instance_id != *instance_id || variable.path != *path {
                    return Err(format!("path index disagrees for {address}"));
                }
            }
        }

        for address in self.variables.keys() {
            let instance = self
                .instances
                .get(&address.instance_id)
                .ok_or_else(|| format!("runtime variable {address} references missing instance"))?;
            if instance.state_addresses.binary_search(address).is_err() {
                return Err(format!(
                    "runtime variable {address} is not indexed by instance"
                ));
            }
        }

        Ok(())
    }

    pub(crate) fn take_initial_state(
        &mut self,
    ) -> Vec<celox_design::InitialStateValue<AbsoluteAddr>> {
        std::mem::take(&mut self.semantic.initial_state)
    }

    pub(crate) fn restore_initial_state(
        &mut self,
        initial_state: Vec<celox_design::InitialStateValue<AbsoluteAddr>>,
    ) {
        self.semantic.initial_state = initial_state;
    }
}

/// Error returned by [`RuntimeProgram::get_addr`] when a path-based variable lookup fails.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AddrLookupError {
    #[error("Instance not found: {path}")]
    InstanceNotFound { path: String },
    #[error("Variable not found: {path}")]
    VariableNotFound { path: String },
    #[error("Ambiguous variable path: {path} — multiple variables share this path")]
    AmbiguousPath { path: String },
}

/// Internal consistency failure while consuming the frontend projection into
/// the canonical runtime design.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum DesignProjectionError {
    #[error("state object count differs: design={design} frontend={frontend}")]
    StateObjectCount { design: usize, frontend: usize },
    #[error("missing state projection for {source_address}")]
    MissingStateProjection { source_address: SourceAddr },
    #[error("missing flattened state object {address}")]
    MissingStateObject { address: AbsoluteAddr },
    #[error("metadata differs for flattened state object {address}")]
    MetadataMismatch { address: AbsoluteAddr },
    #[error("invalid normalized runtime design: {reason}")]
    InvalidRuntimeDesign { reason: String },
}

#[cfg(feature = "host-runtime")]
pub type InitialMemoryWriteRun = InitialStateWriteRun;
#[cfg(feature = "host-runtime")]
pub type InitialMemoryData = InitialStateData;
pub type RuntimeErrorInfo<Addr = AbsoluteAddr> = celox_design::RuntimeErrorInfo<Addr>;

/// Source-independent metadata retained while a compiled design is executing.
///
/// Compiler-only SIR and layout requirements are deliberately absent. A
/// backend can therefore discard the compiler artifact after code generation.
#[derive(Clone)]
pub struct RuntimeProgram {
    pub design: RuntimeDesign,
    pub runtime_schema: RuntimeSchema<AbsoluteAddr>,
    pub testbench: Option<TestbenchProgram<AbsoluteAddr>>,
}

/// Lowered SIR whose backend-independent optimization pipeline has not run.
#[derive(Clone, Debug)]
pub struct UnoptimizedSir {
    pub sir: SirProgram,
    pub layout_requirements: celox_state_layout::LayoutRequirements<AbsoluteAddr>,
    pub runtime: RuntimeProgram,
}

impl UnoptimizedSir {
    pub(crate) fn new(sir: SirProgram, runtime: RuntimeProgram) -> Self {
        Self {
            sir,
            layout_requirements: Default::default(),
            runtime,
        }
    }

    pub(crate) fn into_optimized(self) -> OptimizedSir {
        OptimizedSir::new(self.sir, self.runtime, self.layout_requirements)
    }
}

impl Deref for UnoptimizedSir {
    type Target = RuntimeProgram;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

/// A pre-layout compiler artifact whose SIR optimization pipeline has
/// completed successfully.
///
/// Construction is restricted to the compiler driver. Physical layout can
/// only be finalized from this phase, preventing unoptimized SIR from
/// accidentally entering a backend.
#[derive(Clone, Debug)]
pub struct OptimizedSir {
    pub sir: SirProgram,
    pub layout_requirements: celox_state_layout::LayoutRequirements<AbsoluteAddr>,
    pub(crate) runtime: RuntimeProgram,
}

impl OptimizedSir {
    pub(crate) fn new(
        sir: SirProgram,
        runtime: RuntimeProgram,
        layout_requirements: celox_state_layout::LayoutRequirements<AbsoluteAddr>,
    ) -> Self {
        Self {
            sir,
            layout_requirements,
            runtime,
        }
    }

    #[cfg(all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )
    ))]
    pub(crate) fn into_runtime(self) -> RuntimeProgram {
        self.runtime
    }
}

impl Deref for OptimizedSir {
    type Target = RuntimeProgram;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

/// Optimized SIR whose physical state layout has been finalized.
///
/// Backend code generation accepts this artifact instead of a bare SIR value,
/// making it impossible to enter code generation before layout construction.
#[derive(Clone, Debug)]
pub struct LaidOutProgram {
    pub sir: SirProgram,
    pub(crate) runtime: RuntimeProgram,
    layout: crate::backend::MemoryLayout,
}

impl LaidOutProgram {
    pub fn layout(&self) -> &crate::backend::MemoryLayout {
        &self.layout
    }

    pub fn runtime(&self) -> &RuntimeProgram {
        &self.runtime
    }

    pub fn into_runtime(self) -> RuntimeProgram {
        self.runtime
    }
}

impl Deref for LaidOutProgram {
    type Target = RuntimeProgram;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl fmt::Debug for RuntimeProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeProgram")
            .field("num_events", &self.design.events.len())
            .finish_non_exhaustive()
    }
}

impl OptimizedSir {
    /// Finalize the physical state layout and consume the optimized program.
    pub fn into_laid_out(self, four_state: bool) -> LaidOutProgram {
        self.into_laid_out_with_mode(
            four_state,
            crate::backend::memory_layout::MemoryLayoutMode::Packed,
        )
    }

    pub fn into_laid_out_with_mode(
        self,
        four_state: bool,
        mode: crate::backend::memory_layout::MemoryLayoutMode,
    ) -> LaidOutProgram {
        let mut program = self;
        if !program.runtime_schema.comb_observers.is_empty()
            && !program.layout_requirements.is_empty()
        {
            let observed_written: crate::HashSet<AbsoluteAddr> = program
                .runtime_schema
                .comb_observers
                .iter()
                .flat_map(|observer| observer.written_inputs.iter().copied())
                .collect();
            program
                .layout_requirements
                .state_aliases_mut()
                .retain(|alias_addr, _| !observed_written.contains(alias_addr));
            program
                .layout_requirements
                .state_aliases_mut()
                .retain(|alias_addr, _| {
                    !comb_capture_enable_needs_unaliased_old_value(
                        &program.sir.eval_comb,
                        *alias_addr,
                    )
                });
        }
        crate::optimizer::sir::retain_final_identity_aliases(&mut program, four_state);
        let layout = crate::backend::MemoryLayout::build(&program, four_state, mode);

        // Remove identity Stores for aliases validated by the layout
        if !program.layout_requirements.is_empty() {
            let aliased: crate::HashMap<AbsoluteAddr, AbsoluteAddr> = program
                .layout_requirements
                .state_aliases()
                .iter()
                .filter(|(alias_addr, canonical_addr)| {
                    layout
                        .offsets
                        .get(alias_addr)
                        .zip(layout.offsets.get(canonical_addr))
                        .is_some_and(|(a, c)| a == c)
                })
                .map(|(&alias, &canonical)| (alias, canonical))
                .collect();
            if !aliased.is_empty() {
                crate::optimizer::sir::remove_final_identity_alias_stores(
                    &mut program,
                    &aliased,
                    four_state,
                );
            }
        }
        rebuild_rtl_writes(&mut program);
        program.layout_requirements.clear();
        let OptimizedSir {
            sir,
            runtime,
            layout_requirements,
        } = program;
        debug_assert!(layout_requirements.is_empty());
        LaidOutProgram {
            sir,
            runtime,
            layout,
        }
    }
}

fn rebuild_rtl_writes(program: &mut OptimizedSir) {
    let mut rtl_writes = crate::HashSet::default();
    for unit in program
        .sir
        .eval_comb
        .iter()
        .chain(program.sir.eval_apply_ffs.values().flatten())
        .chain(program.sir.eval_comb_apply_ffs.values().flatten())
        .chain(program.sir.eval_only_ffs.values().flatten())
        .chain(program.sir.apply_ffs.values().flatten())
    {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                let (address, offset, width) = match instruction {
                    SIRInstruction::Store(address, offset, width, ..)
                    | SIRInstruction::Commit(_, address, offset, width, _) => {
                        (address.absolute_addr(), offset, *width)
                    }
                    _ => continue,
                };
                let access = offset
                    .constant_bit_offset()
                    .and_then(|lsb| {
                        width
                            .checked_sub(1)
                            .and_then(|tail| lsb.checked_add(tail))
                            .map(|msb| BitAccess::new(lsb, msb))
                    })
                    .or_else(|| {
                        program
                            .runtime
                            .design
                            .state_objects
                            .get(&address)
                            .and_then(|object| object.width.checked_sub(1))
                            .map(|msb| BitAccess::new(0, msb))
                    });
                if let Some(access) = access {
                    rtl_writes.insert(VarAtomBase {
                        id: address,
                        access,
                    });
                }
            }
        }
    }
    program.runtime.runtime_schema.rtl_writes = rtl_writes;
}

impl RuntimeProgram {
    #[cfg(all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )
    ))]
    pub(crate) fn build_design_reflection(
        &self,
        layout: &crate::backend::MemoryLayout,
    ) -> DesignReflection {
        struct ScopeSource {
            instance_id: InstanceId,
            name: String,
            full_name: String,
            parent_name: Option<String>,
            module_name: String,
        }

        let root_name = self
            .design
            .root_instance()
            .expect("top-level instance exists")
            .module_name
            .clone();

        let mut scope_sources = self
            .design
            .instances()
            .map(|instance| {
                let segments = &instance.display_path;
                let name = segments
                    .last()
                    .cloned()
                    .unwrap_or_else(|| root_name.clone());
                let full_name = if segments.is_empty() {
                    root_name.clone()
                } else {
                    format!("{root_name}.{}", segments.join("."))
                };
                let parent_name = (!segments.is_empty()).then(|| {
                    if segments.len() == 1 {
                        root_name.clone()
                    } else {
                        format!("{root_name}.{}", segments[..segments.len() - 1].join("."))
                    }
                });
                ScopeSource {
                    instance_id: instance.id,
                    name,
                    full_name,
                    parent_name,
                    module_name: instance.module_name.clone(),
                }
            })
            .collect::<Vec<_>>();
        scope_sources.sort_by(|left, right| left.full_name.cmp(&right.full_name));

        let scope_ids = scope_sources
            .iter()
            .enumerate()
            .map(|(index, scope)| {
                (
                    scope.full_name.clone(),
                    ReflectionScopeId(u32::try_from(index).expect("scope count exceeds u32")),
                )
            })
            .collect::<HashMap<_, _>>();
        let instance_scopes = scope_sources
            .iter()
            .enumerate()
            .map(|(index, scope)| {
                (
                    scope.instance_id,
                    ReflectionScopeId(u32::try_from(index).expect("scope count exceeds u32")),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut scopes = scope_sources
            .iter()
            .map(|scope| ReflectionScope {
                name: scope.name.clone(),
                full_name: scope.full_name.clone(),
                module_name: scope.module_name.clone(),
                parent: scope.parent_name.as_ref().map(|parent| scope_ids[parent]),
                children: Vec::new(),
                signals: Vec::new(),
            })
            .collect::<Vec<_>>();
        let child_parents = scopes
            .iter()
            .enumerate()
            .filter_map(|(index, scope)| {
                scope.parent.map(|parent| {
                    (
                        parent,
                        ReflectionScopeId(u32::try_from(index).expect("scope count exceeds u32")),
                    )
                })
            })
            .collect::<Vec<_>>();
        for (parent, child) in child_parents {
            scopes[parent.0 as usize].children.push(child);
        }

        let mut signals = Vec::new();
        for scope in &scope_sources {
            let instance = self.design.instance(scope.instance_id).unwrap();
            for state_address in instance.state_addresses() {
                let variable = self.design.variable(state_address).unwrap();
                if matches!(
                    variable.var_kind,
                    VariableKind::Parameter | VariableKind::Constant
                ) {
                    continue;
                }
                if instance.path_index.get(&variable.path) != Some(&Some(*state_address)) {
                    continue;
                }
                let metadata = &self.design.state_objects[state_address];
                let name = variable.path.join(".");
                let array_layout =
                    layout
                        .unpacked_arrays
                        .get(state_address)
                        .map(|array| SignalArrayLayout {
                            element_width: array.element_width,
                            element_count: array.element_count,
                            element_stride: array.element_stride,
                            plane_size: array.plane_size,
                        });
                let direction = match variable.var_kind {
                    VariableKind::Input => SignalDirection::Input,
                    VariableKind::Output => SignalDirection::Output,
                    VariableKind::Inout => SignalDirection::Inout,
                    _ => SignalDirection::Internal,
                };
                signals.push(ReflectionSignal {
                    full_name: format!("{}.{}", scope.full_name, name),
                    name,
                    parent: instance_scopes[&scope.instance_id],
                    state_address: *state_address,
                    signal: SignalRef {
                        offset: layout.offsets[state_address],
                        width: layout.widths[state_address],
                        is_4state: layout.is_4states[state_address],
                        array_layout,
                    },
                    direction,
                    domain_kind: metadata.kind,
                    signed: variable.signed,
                    packed_dims: variable.packed_dims.clone(),
                    unpacked_dims: metadata.array_dims.clone(),
                    type_kind: metadata.type_kind,
                });
            }
        }
        signals.sort_by(|left, right| left.full_name.cmp(&right.full_name));
        for (index, signal) in signals.iter().enumerate() {
            scopes[signal.parent.0 as usize]
                .signals
                .push(ReflectionSignalId(
                    u32::try_from(index).expect("signal count exceeds u32"),
                ));
        }
        let reflection = DesignReflection::new(scopes, signals);
        debug_assert!(reflection.validate().is_ok());
        reflection
    }

    pub(crate) fn from_scheduled(
        scheduled: celox_frontend_core::ScheduledRtl,
    ) -> Result<(SirProgram, Self), DesignProjectionError> {
        let design = RuntimeDesign::from_projection(scheduled.design, scheduled.frontend_lookup)?;
        Ok((
            scheduled.sir,
            Self {
                design,
                runtime_schema: scheduled.runtime_schema,
                testbench: None,
            },
        ))
    }

    pub fn get_addr(
        &self,
        instance_path: &[(&str, usize)],
        var_path: &[&str],
    ) -> Result<AbsoluteAddr, AddrLookupError> {
        let instance_path: Vec<(String, usize)> = instance_path
            .iter()
            .map(|(name, index)| ((*name).to_string(), *index))
            .collect();
        let instance = self
            .design
            .instance_at_path(&InstancePath(instance_path.clone()))
            .ok_or_else(|| AddrLookupError::InstanceNotFound {
                path: instance_path
                    .iter()
                    .map(|(s, i)| format!("{}[{}]", s, i))
                    .collect::<Vec<_>>()
                    .join("."),
            })?;
        let target_path = var_path
            .iter()
            .map(|segment| (*segment).to_string())
            .collect::<Vec<_>>();
        let path_str = var_path.join(".");
        let entry = instance.path_index.get(&target_path).ok_or_else(|| {
            AddrLookupError::VariableNotFound {
                path: path_str.clone(),
            }
        })?;
        entry
            .as_ref()
            .copied()
            .ok_or(AddrLookupError::AmbiguousPath { path: path_str })
    }

    pub fn get_path(&self, addr: &AbsoluteAddr) -> String {
        self.design.get_path(addr)
    }

    pub fn get_variable_info(&self, addr: &AbsoluteAddr) -> Option<VariableInfo> {
        self.design.variable_info(addr)
    }

    pub fn num_events(&self) -> usize {
        self.design.events.len()
    }
}

impl OptimizedSir {
    /// Collect the set of `AbsoluteAddr` values that are accessed in the working
    /// region (region != STABLE). These are the only variables that need working
    /// region space.
    pub fn collect_working_region_addrs(&self) -> crate::HashSet<AbsoluteAddr> {
        let mut addrs = crate::HashSet::default();

        let scan_units =
            |units: &HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
             addrs: &mut crate::HashSet<AbsoluteAddr>| {
                for eu_list in units.values() {
                    for eu in eu_list {
                        for block in eu.blocks.values() {
                            for inst in &block.instructions {
                                match inst {
                                    SIRInstruction::Store(addr, _, _, _, _, _)
                                        if addr.region == WORKING_REGION =>
                                    {
                                        addrs.insert(addr.absolute_addr());
                                    }
                                    SIRInstruction::Commit(src, dst, _, _, _) => {
                                        if src.region == WORKING_REGION {
                                            addrs.insert(src.absolute_addr());
                                        }
                                        if dst.region == WORKING_REGION {
                                            addrs.insert(dst.absolute_addr());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            };

        scan_units(&self.sir.eval_apply_ffs, &mut addrs);
        scan_units(&self.sir.eval_comb_apply_ffs, &mut addrs);
        scan_units(&self.sir.eval_only_ffs, &mut addrs);
        scan_units(&self.sir.apply_ffs, &mut addrs);

        addrs
    }

    pub fn collect_sparse_working_region_addrs(&self) -> crate::HashSet<AbsoluteAddr> {
        let mut addrs = crate::HashSet::default();
        for units in self
            .sir
            .eval_apply_ffs
            .values()
            .chain(self.sir.eval_comb_apply_ffs.values())
            .chain(self.sir.eval_only_ffs.values())
        {
            for eu in units {
                for block in eu.blocks.values() {
                    for inst in &block.instructions {
                        if let SIRInstruction::Store(addr, _, _, _, _, _) = inst
                            && addr.region == SPARSE_WORKING_REGION
                        {
                            addrs.insert(addr.absolute_addr());
                        }
                    }
                }
            }
        }
        addrs
    }
}

fn comb_capture_enable_needs_unaliased_old_value(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    alias_addr: AbsoluteAddr,
) -> bool {
    for eu in units {
        for block in eu.blocks.values() {
            let mut last_store = None;
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Store(addr, _, _, _, _, comb_capture_sites) => {
                        let abs = addr.absolute_addr();
                        if abs == alias_addr && !comb_capture_sites.is_empty() {
                            return true;
                        }
                        last_store = Some(abs);
                    }
                    SIRInstruction::CombCaptureEnableIfChanged { sites, .. } => {
                        if !sites.is_empty() && last_store == Some(alias_addr) {
                            return true;
                        }
                        last_store = None;
                    }
                    _ => {
                        last_store = None;
                    }
                }
            }
        }
    }
    false
}

pub(crate) mod verify {
    pub(crate) use celox_sir::verify::*;
}
pub use celox_slt::{GlueAddrBase, GlueBlockBase};

pub use celox_frontend_core::TraceSimModule as SimModule;
#[cfg(all(
    feature = "host-runtime",
    any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )
))]
pub(crate) use celox_runtime::SignalArrayLayout;
pub use celox_runtime::SignalRef;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_zero_analysis_collapses_repeated_concat_dependencies() {
        let zero = RegisterId(0);
        let wide_zero = RegisterId(1);
        let sliced_zero = RegisterId(2);
        let nonzero = RegisterId(3);
        let mixed = RegisterId(4);
        let eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                        SIRInstruction::Concat(wide_zero, vec![zero; 4096]),
                        SIRInstruction::Slice(sliced_zero, wide_zero, 0, 64),
                        SIRInstruction::Imm(nonzero, SIRValue::new(1u8)),
                        SIRInstruction::Concat(mixed, vec![zero, nonzero]),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: HashMap::default(),
        };

        let zeros = collect_exact_zero_registers(&eu, [sliced_zero, mixed]);
        assert!(zeros.contains(&zero));
        assert!(zeros.contains(&wide_zero));
        assert!(zeros.contains(&sliced_zero));
        assert!(!zeros.contains(&nonzero));
        assert!(!zeros.contains(&mixed));
    }

    #[test]
    fn test_sirvalue_display() {
        let val = SIRValue::new(42u64);
        let display = format!("{}", val);
        assert!(display.contains("SIRValue"));
        assert!(display.contains("0x2a")); // 42 in hex
    }

    #[test]
    fn test_absoluteaddr_display() {
        let addr = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: celox_design::StateObjectId(0),
        };
        let display = format!("{}", addr);
        assert!(display.contains("AbsoluteAddr"));
        assert!(display.contains("inst0"));
        assert!(display.contains("state0"));
    }

    #[test]
    fn test_glueaddr_display() {
        let parent_addr =
            celox_frontend_veryl::GlueAddr::Parent(veryl_analyzer::ir::VarId::default());
        let parent_display = format!("{}", parent_addr);
        assert!(parent_display.contains("GlueAddr::Parent"));
        assert!(parent_display.contains("var0"));

        let child_addr =
            celox_frontend_veryl::GlueAddr::Child(veryl_analyzer::ir::VarId::default());
        let child_display = format!("{}", child_addr);
        assert!(child_display.contains("GlueAddr::Child"));
        assert!(child_display.contains("var0"));
    }

    #[test]
    fn test_instanceid_display() {
        let id = InstanceId(42);
        let display = format!("{}", id);
        assert_eq!(display, "inst42");
    }

    #[test]
    fn test_binaryop_display() {
        assert_eq!(format!("{}", BinaryOp::Add), "Add");
        assert_eq!(format!("{}", BinaryOp::Sub), "Sub");
        assert_eq!(format!("{}", BinaryOp::Mul), "Mul");
        assert_eq!(format!("{}", BinaryOp::Xor), "Xor");
    }

    #[test]
    fn test_unaryop_display() {
        assert_eq!(format!("{}", UnaryOp::Minus), "Minus");
        assert_eq!(format!("{}", UnaryOp::LogicNot), "LogicNot");
        assert_eq!(format!("{}", UnaryOp::BitNot), "BitNot");
        assert_eq!(format!("{}", UnaryOp::PopCount), "PopCount");
        assert_eq!(
            format!("{}", UnaryOp::CountLeadingZeros),
            "CountLeadingZeros"
        );
        assert_eq!(
            format!("{}", UnaryOp::CountTrailingZeros),
            "CountTrailingZeros"
        );
    }

    #[test]
    fn bit_count_result_width_represents_operand_width() {
        for (operand_width, expected) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 2),
            (8, 4),
            (usize::MAX, usize::BITS as usize),
        ] {
            for op in [
                UnaryOp::PopCount,
                UnaryOp::CountLeadingZeros,
                UnaryOp::CountTrailingZeros,
            ] {
                assert_eq!(op.result_width(operand_width), expected, "{op}");
            }
        }
    }

    #[test]
    fn bit_count_unary_ops_roundtrip_through_serde() {
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            let encoded = serde_json::to_string(&op).unwrap();
            let decoded: UnaryOp = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, op);
        }
    }

    #[test]
    fn test_sirinstruction_display() {
        // Test Imm instruction
        let imm: SIRInstruction<i32> = SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u64));
        let imm_display = format!("{}", imm);
        assert!(imm_display.contains("r0"));
        assert!(imm_display.contains("SIRValue"));

        // Test Binary instruction
        let binary: SIRInstruction<i32> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Add, RegisterId(2));
        let binary_display = format!("{}", binary);
        assert!(binary_display.contains("r0"));
        assert!(binary_display.contains("r1"));
        assert!(binary_display.contains("r2"));
        assert!(binary_display.contains("Add"));

        // Test Unary instruction
        let unary: SIRInstruction<i32> =
            SIRInstruction::Unary(RegisterId(0), UnaryOp::Minus, RegisterId(1));
        let unary_display = format!("{}", unary);
        assert!(unary_display.contains("r0"));
        assert!(unary_display.contains("r1"));
        assert!(unary_display.contains("Minus"));
    }

    #[test]
    fn test_sirterminator_display() {
        // Test Jump
        let jump = SIRTerminator::Jump(BlockId(1), vec![RegisterId(0), RegisterId(1)]);
        let jump_display = format!("{}", jump);
        assert!(jump_display.contains("Jump"));
        assert!(jump_display.contains("b1"));

        // Test Return
        let ret = SIRTerminator::Return;
        let ret_display = format!("{}", ret);
        assert_eq!(ret_display, "Return");

        // Test Branch
        let branch = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![]),
            false_block: (BlockId(2), vec![]),
        };
        let branch_display = format!("{}", branch);
        assert!(branch_display.contains("Branch"));
        assert!(branch_display.contains("b1"));
        assert!(branch_display.contains("b2"));
    }

    #[test]
    fn test_basicblock_display() {
        let _block: BasicBlock<i32> = BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(42u64)),
                SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Add, RegisterId(2)),
            ],
            terminator: SIRTerminator::Return,
        };

        let block_display = format!("{}", _block);
        assert!(block_display.contains("b0:"));
        assert!(block_display.contains("params:"));
        assert!(block_display.contains("r0"));
        assert!(block_display.contains("r1"));
        assert!(block_display.contains("Add"));
        assert!(block_display.contains("Return"));
    }

    #[test]
    fn single_predecessor_inlining_rewrites_dominated_parameter_uses() {
        let mut eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![RegisterId(1)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: (0..3)
                .map(|register| {
                    (
                        RegisterId(register),
                        RegisterType::Bit {
                            width: 8,
                            signed: false,
                        },
                    )
                })
                .collect(),
        };
        eu.verify_result().unwrap();

        assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
        eu.verify_result().unwrap();
        assert_eq!(eu.blocks.len(), 1);
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::Unary(
                RegisterId(2),
                UnaryOp::Ident,
                RegisterId(0)
            )]
        ));
    }

    #[test]
    fn single_predecessor_inlining_handles_deep_linear_cfg() {
        const BLOCK_COUNT: usize = 20_000;

        let mut eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: (0..BLOCK_COUNT)
                .map(|index| {
                    let id = BlockId(index);
                    let terminator = if index + 1 == BLOCK_COUNT {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(BlockId(index + 1), Vec::new())
                    };
                    (
                        id,
                        BasicBlock {
                            id,
                            params: Vec::new(),
                            instructions: Vec::new(),
                            terminator,
                        },
                    )
                })
                .collect(),
            register_map: crate::HashMap::default(),
        };
        eu.verify_result().unwrap();

        assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
        assert_eq!(eu.blocks.len(), 1);
        assert_eq!(eu.blocks[&BlockId(0)].terminator, SIRTerminator::Return);
        eu.verify_result().unwrap();
    }
}
