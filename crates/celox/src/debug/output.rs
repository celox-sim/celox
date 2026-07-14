use crate::HashMap;
/// Module for outputting SIR and SLT from Veryl source code
use crate::cfg_order::dominance_order;
use crate::ir::{
    ExecutionUnit, ModuleId, Program, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIRTerminator, SimModule,
};

use crate::debug::CompilationTrace;
use veryl_parser::resource_table;

impl CompilationTrace {
    /// Format pre-optimized SIR to string representation
    pub fn format_pre_optimized_sir(&self) -> Option<String> {
        self.pre_optimized_sir.as_ref().map(format_program)
    }

    /// Format post-optimized SIR to string representation
    pub fn format_post_optimized_sir(&self) -> Option<String> {
        self.post_optimized_sir.as_ref().map(format_program)
    }
    /// Format analyzer IR to string representation
    pub fn format_analyzer_ir(&self) -> Option<String> {
        self.analyzer_ir.clone()
    }

    /// Alias for format_post_optimized_sir
    pub fn format_program(&self) -> Option<String> {
        self.format_post_optimized_sir()
    }

    /// Format SLT to string representation
    pub fn format_slt(&self) -> Option<String> {
        self.sim_modules.as_ref().map(format_slt)
    }

    pub fn print(&self) {
        if let Some(slt) = self.format_slt() {
            println!("{}", slt);
        }
        if let Some(sir) = self.format_pre_optimized_sir() {
            println!("=== Pre-optimized SIR ===\n{}", sir);
        }
        if let Some(sir) = self.format_post_optimized_sir() {
            println!("=== Post-optimized SIR ===\n{}", sir);
        }
        if let Some(ir) = self.format_analyzer_ir() {
            println!("=== Analyzer IR ===\n{}", ir);
        }
        if let Some(clif) = &self.pre_optimized_clif {
            println!("=== Pre-optimized CLIF ===\n{}", clif);
        }
        if let Some(clif) = &self.post_optimized_clif {
            println!("=== Post-optimized CLIF ===\n{}", clif);
        }
        if let Some(native) = &self.native {
            println!("=== Native Machine Code ===\n{}", native);
        }
        if let Some(mir) = &self.mir {
            println!("=== MIR (Native Backend) ===\n{}", mir);
        }
    }
}

/// Format Program to string representation
pub fn format_program(program: &Program) -> String {
    let mut output = String::new();

    output.push_str("=== Evaluation Flip-Flops (eval_apply_ffs) ===\n");
    for (addr, execution_units) in &program.eval_apply_ffs {
        output.push_str(&format!(
            "Trigger Group: {} ({})\n",
            program.get_path(addr),
            addr
        ));
        for (idx, eu) in execution_units.iter().enumerate() {
            output.push_str(&format!("  Execution Unit {}:\n", idx));
            output.push_str(&format!("    Entry Block: {}\n", eu.entry_block_id.0));
            output.push_str("    Registers:\n");
            let mut reg_ids: Vec<_> = eu.register_map.keys().collect();
            reg_ids.sort();
            for id in reg_ids {
                let ty = &eu.register_map[id];
                match ty {
                    crate::ir::RegisterType::Logic { width } => {
                        output.push_str(&format!("      r{}: logic<{}>\n", id.0, width));
                    }
                    crate::ir::RegisterType::Bit { width, signed } => {
                        let s = if *signed { "signed " } else { "" };
                        output.push_str(&format!("      r{}: {}bit<{}>\n", id.0, s, width));
                    }
                }
            }
            for block_id in sir_dominance_order(eu) {
                let block = &eu.blocks[&block_id];
                output.push_str(&format!("    b{}:\n", block.id.0));
                append_sir_block_params(&mut output, &block.params, "      ");
                for inst in &block.instructions {
                    output.push_str(&format!("      {}\n", format_instruction(inst, program)));
                }
                output.push_str(&format!("      {}\n", block.terminator));
            }
        }
    }

    output.push_str("\n=== Evaluation Flip-Flops (eval_only_ffs) ===\n");
    for (addr, execution_units) in &program.eval_only_ffs {
        output.push_str(&format!(
            "Trigger Group: {} ({})\n",
            program.get_path(addr),
            addr
        ));
        for (idx, eu) in execution_units.iter().enumerate() {
            output.push_str(&format!("  Execution Unit {}:\n", idx));
            output.push_str(&format!("    Entry Block: {}\n", eu.entry_block_id.0));
            output.push_str("    Registers:\n");
            let mut reg_ids: Vec<_> = eu.register_map.keys().collect();
            reg_ids.sort();
            for id in reg_ids {
                let ty = &eu.register_map[id];
                match ty {
                    crate::ir::RegisterType::Logic { width } => {
                        output.push_str(&format!("      r{}: logic<{}>\n", id.0, width));
                    }
                    crate::ir::RegisterType::Bit { width, signed } => {
                        let s = if *signed { "signed " } else { "" };
                        output.push_str(&format!("      r{}: {}bit<{}>\n", id.0, s, width));
                    }
                }
            }
            for block_id in sir_dominance_order(eu) {
                let block = &eu.blocks[&block_id];
                output.push_str(&format!("    b{}:\n", block.id.0));
                append_sir_block_params(&mut output, &block.params, "      ");
                for inst in &block.instructions {
                    output.push_str(&format!("      {}\n", format_instruction(inst, program)));
                }
                output.push_str(&format!("      {}\n", block.terminator));
            }
        }
    }

    output.push_str("\n=== Application Flip-Flops (apply_ffs) ===\n");
    for (addr, execution_units) in &program.apply_ffs {
        output.push_str(&format!(
            "Trigger Group: {} ({})\n",
            program.get_path(addr),
            addr
        ));
        for (idx, eu) in execution_units.iter().enumerate() {
            output.push_str(&format!("  Execution Unit {}:\n", idx));
            output.push_str(&format!("    Entry Block: {}\n", eu.entry_block_id.0));
            output.push_str("    Registers:\n");
            let mut reg_ids: Vec<_> = eu.register_map.keys().collect();
            reg_ids.sort();
            for id in reg_ids {
                let ty = &eu.register_map[id];
                match ty {
                    crate::ir::RegisterType::Logic { width } => {
                        output.push_str(&format!("      r{}: logic<{}>\n", id.0, width));
                    }
                    crate::ir::RegisterType::Bit { width, signed } => {
                        let s = if *signed { "signed " } else { "" };
                        output.push_str(&format!("      r{}: {}bit<{}>\n", id.0, s, width));
                    }
                }
            }
            for block_id in sir_dominance_order(eu) {
                let block = &eu.blocks[&block_id];
                output.push_str(&format!("    b{}:\n", block.id.0));
                append_sir_block_params(&mut output, &block.params, "      ");
                for inst in &block.instructions {
                    output.push_str(&format!("      {}\n", format_instruction(inst, program)));
                }
                output.push_str(&format!("      {}\n", block.terminator));
            }
        }
    }

    output.push_str("\n=== Evaluation Combinational Logic (eval_comb) ===\n");
    for (idx, eu) in program.eval_comb.iter().enumerate() {
        output.push_str(&format!("Execution Unit {}:\n", idx));
        output.push_str(&format!("  Entry Block: {}\n", eu.entry_block_id.0));
        output.push_str("  Registers:\n");
        let mut reg_ids: Vec<_> = eu.register_map.keys().collect();
        reg_ids.sort();
        for id in reg_ids {
            let ty = &eu.register_map[id];
            match ty {
                crate::ir::RegisterType::Logic { width } => {
                    output.push_str(&format!("    r{}: logic<{}>\n", id.0, width));
                }
                crate::ir::RegisterType::Bit { width, signed } => {
                    let s = if *signed { "signed " } else { "" };
                    output.push_str(&format!("    r{}: {}bit<{}>\n", id.0, s, width));
                }
            }
        }
        for block_id in sir_dominance_order(eu) {
            let block = &eu.blocks[&block_id];
            output.push_str(&format!("  b{}:\n", block.id.0));
            append_sir_block_params(&mut output, &block.params, "    ");
            for inst in &block.instructions {
                output.push_str(&format!("    {}\n", format_instruction(inst, program)));
            }
            output.push_str(&format!("    {}\n", block.terminator));
        }
    }

    output
}

fn append_sir_block_params(output: &mut String, params: &[RegisterId], indent: &str) {
    if params.is_empty() {
        return;
    }
    output.push_str(indent);
    output.push_str("params: [");
    for (index, param) in params.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        output.push_str(&format!("r{}", param.0));
    }
    output.push_str("]\n");
}

fn sir_dominance_order<A>(eu: &ExecutionUnit<A>) -> Vec<crate::ir::BlockId> {
    dominance_order(
        eu.entry_block_id,
        eu.blocks.keys().copied(),
        |block_id| match &eu.blocks[&block_id].terminator {
            SIRTerminator::Jump(target, _) => vec![*target],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        },
    )
}

fn format_regioned_addr(addr: &RegionedAbsoluteAddr, program: &Program) -> String {
    format!(
        "{} (region={})",
        program.get_path(&addr.absolute_addr()),
        addr.region
    )
}

fn format_instruction(inst: &SIRInstruction<RegionedAbsoluteAddr>, program: &Program) -> String {
    match inst {
        SIRInstruction::Imm(rd, value) => format!("r{} = {}", rd.0, value),
        SIRInstruction::Binary(rd, rs1, op, rs2) => {
            format!("r{} = r{} {} r{}", rd.0, rs1.0, op, rs2.0)
        }
        SIRInstruction::Unary(rd, op, rs) => format!("r{} = {} r{}", rd.0, op, rs.0),
        SIRInstruction::Load(rd, addr, offset, bits) => {
            format!(
                "r{} = Load(addr={}, offset={}, bits={})",
                rd.0,
                format_regioned_addr(addr, program),
                offset,
                bits
            )
        }
        SIRInstruction::Store(addr, offset, bits, src, _, _) => {
            format!(
                "Store(addr={}, offset={}, bits={}, src_reg = {})",
                format_regioned_addr(addr, program),
                offset,
                bits,
                src.0
            )
        }
        SIRInstruction::Commit(src, dst, offset, bits, _) => {
            format!(
                "Commit(src={}, dst={}, offset={}, bits={})",
                format_regioned_addr(src, program),
                format_regioned_addr(dst, program),
                offset,
                bits
            )
        }
        SIRInstruction::Concat(rd, rs) => {
            let rs_str = rs
                .iter()
                .map(|r| format!("r{}", r.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("r{} = Concat({})", rd.0, rs_str)
        }
        SIRInstruction::Slice(dst, src, offset, width) => {
            format!(
                "r{} = Slice(r{}, offset={}, width={})",
                dst.0, src.0, offset, width
            )
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            format!(
                "r{} = Mux(cond=r{}, then=r{}, else=r{})",
                dst.0, cond.0, then_val.0, else_val.0
            )
        }
        SIRInstruction::RuntimeEvent { site_id, args } => {
            let args = args
                .iter()
                .map(|r| format!("r{}", r.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!("RuntimeEvent(site={}, args=[{}])", site_id, args)
        }
        SIRInstruction::CombCaptureEvent {
            site_id,
            args,
            fatal_error_code,
            consume_enabled,
        } => {
            let args = args
                .iter()
                .map(|r| format!("r{}", r.0))
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(code) = fatal_error_code {
                format!(
                    "CombCaptureEvent(site={}, args=[{}], fatal_error={})",
                    site_id, args, code
                )
            } else if *consume_enabled {
                format!(
                    "CombCaptureEvent(site={}, args=[{}], consume_enabled=true)",
                    site_id, args
                )
            } else {
                format!("CombCaptureEvent(site={}, args=[{}])", site_id, args)
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            format!(
                "CombCaptureEnableIfChanged(old=r{}, new=r{}, sites={:?})",
                old.0, new.0, sites
            )
        }
    }
}

/// Format SLT (Simulation Logic Tree) to string representation
pub fn format_slt(sim_modules: &HashMap<ModuleId, SimModule>) -> String {
    let mut output = String::new();

    output.push_str("=== Simulation Logic Tree (SLT) ===\n\n");

    for sim_module in sim_modules.values() {
        output.push_str(&format!(
            "Module: {}\n",
            resource_table::get_str_value(sim_module.name).unwrap()
        ));
        output.push_str("Combinational Logic Blocks:\n");

        for (idx, logic_path) in sim_module.comb_blocks.iter().enumerate() {
            output.push_str(&format!("Path {}:\n", idx));
            output.push_str(&format!("Target: {}\n", logic_path.target));
            output.push_str(&format!(
                "Sources: {}\n",
                logic_path
                    .sources
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(",")
            ));
            output.push_str(&format!(
                "Expression: \n{}\n",
                sim_module.arena.display(logic_path.expr)
            ));
        }
        output.push('\n');
    }

    output
}
