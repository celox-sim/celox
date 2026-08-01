use crate::HashMap;
pub use celox_design::PortTypeKind;
pub(crate) use celox_design::{
    AbsoluteAddrBase, DomainKind, InitialStateData, InitialStateWriteRun, InstanceId, ModuleId,
    RegionedAbsoluteAddrBase, RegionedVarAddrBase, RuntimeEventKind, RuntimeEventSite,
    RuntimeSchema, SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION,
};
#[cfg(test)]
pub(crate) use celox_design::{BinaryOp, UnaryOp};
pub use celox_frontend_veryl::{InstancePath, VariableInfo, VerylFrontendLookup};
#[cfg(test)]
pub(crate) use celox_sir::{BasicBlock, SIRValue, inline_single_predecessor_jumps};
pub(crate) use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    collect_exact_zero_registers,
};
use celox_testbench::TestbenchProgram;
use std::fmt;
use veryl_analyzer::ir::VarPath;

/// Source-independent identity of one elaborated state object.
pub type AbsoluteAddr = celox_design::StateAddr;
/// Source-independent state identity qualified by its storage region.
pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;

/// Error returned by [`Program::get_addr`] when a path-based variable lookup fails.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AddrLookupError {
    #[error("Instance not found: {path}")]
    InstanceNotFound { path: String },
    #[error("Variable not found: {path}")]
    VariableNotFound { path: String },
    #[error("Ambiguous variable path: {path} — multiple variables share this path")]
    AmbiguousPath { path: String },
}

pub type InitialMemoryWriteRun = InitialStateWriteRun;
pub type InitialMemoryData = InitialStateData;
pub type RuntimeErrorInfo<Addr = AbsoluteAddr> = celox_design::RuntimeErrorInfo<Addr>;

#[derive(Clone)]
pub struct Program {
    pub sir: SirProgram,
    pub design: celox_design::ElaboratedDesign<AbsoluteAddr>,
    pub frontend: VerylFrontendLookup,
    pub runtime_schema: RuntimeSchema<AbsoluteAddr>,
    pub layout_requirements: celox_state_layout::LayoutRequirements<AbsoluteAddr>,
    pub testbench: Option<TestbenchProgram<AbsoluteAddr>>,
}

/// A pre-layout compiler artifact whose SIR optimization pipeline has
/// completed successfully.
///
/// Construction is restricted to the compiler driver. Physical layout can
/// only be finalized from this phase, preventing unoptimized SIR from
/// accidentally entering a backend.
#[derive(Clone, Debug)]
pub struct OptimizedSir {
    program: Program,
}

impl OptimizedSir {
    pub(crate) fn new(program: Program) -> Self {
        Self { program }
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    pub fn into_program(self) -> Program {
        self.program
    }
}

impl std::ops::Deref for OptimizedSir {
    type Target = Program;

    fn deref(&self) -> &Self::Target {
        &self.program
    }
}

/// A [`Program`] whose physical state layout has been finalized.
///
/// Backend code generation accepts this artifact instead of a bare `Program`,
/// making it impossible to enter code generation before layout construction.
#[derive(Clone, Debug)]
pub struct LaidOutProgram {
    program: Program,
    layout: crate::backend::MemoryLayout,
}

impl LaidOutProgram {
    pub fn program(&self) -> &Program {
        &self.program
    }

    pub fn layout(&self) -> &crate::backend::MemoryLayout {
        &self.layout
    }

    pub(crate) fn program_mut(&mut self) -> &mut Program {
        &mut self.program
    }

    pub fn into_program(self) -> Program {
        self.program
    }

    pub fn into_parts(self) -> (Program, crate::backend::MemoryLayout) {
        (self.program, self.layout)
    }
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
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
        let mut program = self.program;
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
        crate::optimizer::coalescing::retain_final_identity_aliases(&mut program, four_state);
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
                crate::optimizer::coalescing::remove_final_identity_alias_stores(
                    &mut program,
                    &aliased,
                    four_state,
                );
            }
        }
        program.layout_requirements.clear();

        LaidOutProgram { program, layout }
    }
}

impl Program {
    pub(crate) fn state_address_for_source(
        &self,
        instance_id: InstanceId,
        var_id: veryl_analyzer::ir::VarId,
    ) -> Option<AbsoluteAddr> {
        self.frontend
            .state_address(&celox_frontend_veryl::AbsoluteAddr {
                instance_id,
                var_id,
            })
    }

    pub(crate) fn from_scheduled(
        scheduled: celox_frontend_veryl::ScheduledRtl,
    ) -> (Self, celox_frontend_veryl::VerylTestbenchSource) {
        (
            Self {
                sir: scheduled.sir,
                design: scheduled.design,
                frontend: scheduled.frontend_lookup,
                runtime_schema: scheduled.runtime_schema,
                layout_requirements: Default::default(),
                testbench: None,
            },
            scheduled.testbench_source,
        )
    }

    pub fn get_addr(
        &self,
        instance_path: &[(&str, usize)],
        var_path: &[&str],
    ) -> Result<AbsoluteAddr, AddrLookupError> {
        let mut instance_path_str_id = Vec::new();
        for path in instance_path {
            let id = veryl_parser::resource_table::insert_str(path.0);
            instance_path_str_id.push((id, path.1));
        }
        let instance_id = *self
            .frontend
            .instance_ids
            .get(&InstancePath(instance_path_str_id))
            .ok_or_else(|| AddrLookupError::InstanceNotFound {
                path: instance_path
                    .iter()
                    .map(|(s, i)| format!("{}[{}]", s, i))
                    .collect::<Vec<_>>()
                    .join("."),
            })?;
        let module_id = self.frontend.instance_module[&instance_id];
        let mut var_path_str_id = Vec::new();
        for path in var_path {
            let id = veryl_parser::resource_table::insert_str(path);
            var_path_str_id.push(id);
        }

        let target_path = VarPath(var_path_str_id);
        let path_str = var_path.join(".");
        let entry = self.frontend.module_var_path_index[&module_id]
            .get(&target_path)
            .ok_or_else(|| AddrLookupError::VariableNotFound {
                path: path_str.clone(),
            })?;
        let var_id = entry.ok_or_else(|| AddrLookupError::AmbiguousPath { path: path_str })?;
        let source_addr = celox_frontend_veryl::AbsoluteAddr {
            instance_id,
            var_id,
        };
        self.frontend
            .state_address(&source_addr)
            .ok_or_else(|| AddrLookupError::VariableNotFound {
                path: var_path.join("."),
            })
    }

    pub fn get_path(&self, addr: &AbsoluteAddr) -> String {
        self.frontend.get_state_path(addr)
    }

    pub fn get_variable_info(&self, addr: &AbsoluteAddr) -> Option<&VariableInfo> {
        let source = self.frontend.source_address(addr)?;
        let module_id = self.frontend.instance_module.get(&source.instance_id)?;
        let module_vars = self.frontend.module_variables.get(module_id)?;
        module_vars.get(&source.var_id)
    }

    pub fn num_events(&self) -> usize {
        self.design.events.len()
    }

    /// Verify the temporary migration projection from frontend lookup tables
    /// into the source-independent elaborated design. This can be removed once
    /// the frontend tables are consumed rather than retained beside `design`.
    pub(crate) fn verify_design_projection(&self) -> Result<(), String> {
        let expected_count = self
            .frontend
            .instance_module
            .values()
            .map(|module_id| self.frontend.module_variables[module_id].len())
            .sum::<usize>();
        if self.design.state_objects.len() != expected_count {
            return Err(format!(
                "state object count differs: design={} frontend={expected_count}",
                self.design.state_objects.len()
            ));
        }

        for (&instance_id, module_id) in &self.frontend.instance_module {
            for info in self.frontend.module_variables[module_id].values() {
                let source_address = celox_frontend_veryl::AbsoluteAddr {
                    instance_id,
                    var_id: info.id,
                };
                let Some(address) = self.frontend.state_address(&source_address) else {
                    return Err(format!("missing state projection for {source_address}"));
                };
                let Some(metadata) = self.design.state_objects.get(&address) else {
                    return Err(format!("missing flattened state object {address}"));
                };
                if metadata != &info.metadata {
                    return Err(format!(
                        "metadata differs for flattened state object {address}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Collect the set of `AbsoluteAddr` values that are accessed in the working
    /// region (region != STABLE). These are the only variables that need working
    /// region space.
    pub fn collect_working_region_addrs(&self) -> std::collections::HashSet<AbsoluteAddr> {
        let mut addrs = std::collections::HashSet::new();

        let scan_units =
            |units: &HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
             addrs: &mut std::collections::HashSet<AbsoluteAddr>| {
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

    pub fn collect_sparse_working_region_addrs(&self) -> std::collections::HashSet<AbsoluteAddr> {
        let mut addrs = std::collections::HashSet::new();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalArrayLayout {
    pub element_width: usize,
    pub element_count: usize,
    pub element_stride: usize,
    pub plane_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalRef {
    pub offset: usize,
    pub width: usize,
    pub is_4state: bool,
    pub array_layout: Option<SignalArrayLayout>,
}
pub use celox_frontend_veryl::SimModule;

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
