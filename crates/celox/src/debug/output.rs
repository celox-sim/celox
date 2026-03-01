use crate::HashMap;
/// Module for outputting SIR and SLT from Veryl source code
use crate::ir::{ModuleId, Program, RegionedAbsoluteAddr, SIRInstruction, SimModule};

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
            let mut blocks: Vec<_> = eu.blocks.values().collect();
            blocks.sort_unstable_by_key(|e| e.id.0);
            for block in &blocks {
                output.push_str(&format!("    b{}:\n", block.id.0));
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
            let mut blocks: Vec<_> = eu.blocks.values().collect();
            blocks.sort_unstable_by_key(|e| e.id.0);
            for block in &blocks {
                output.push_str(&format!("    b{}:\n", block.id.0));
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
            let mut blocks: Vec<_> = eu.blocks.values().collect();
            blocks.sort_unstable_by_key(|e| e.id.0);
            for block in &blocks {
                output.push_str(&format!("    b{}:\n", block.id.0));
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
        let mut blocks: Vec<_> = eu.blocks.values().collect();
        blocks.sort_unstable_by_key(|e| e.id.0);
        for block in &blocks {
            output.push_str(&format!("  b{}:\n", block.id.0));
            for inst in &block.instructions {
                output.push_str(&format!("    {}\n", format_instruction(inst, program)));
            }
            output.push_str(&format!("    {}\n", block.terminator));
        }
    }

    output
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
        SIRInstruction::Store(addr, offset, bits, src, _) => {
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
    }
}

/// Format SLT (Simulation Logic Tree) to string representation
pub fn format_slt(sim_modules: &HashMap<ModuleId, SimModule>) -> String {
    let mut output = String::new();

    output.push_str("=== Simulation Logic Tree (SLT) ===\n\n");

    for (_id, sim_module) in sim_modules {
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
