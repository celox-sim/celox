//! A clock move whose entire combinational cone is wiring needs only these
//! stores, not another evaluation of the datapath. Keep the original stores
//! (including trigger bookkeeping) and leave the full comb function unchanged.

use crate::HashMap;
use crate::ir::{
    AbsoluteAddr, BlockId, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr, RegisterId,
    SIRInstruction, SIROffset, SIRTerminator,
};
use celox_sir::BasicBlock;

pub(crate) const EXPORT_PREFIX: &str = "clock_alias_update[";

pub(crate) fn extract(
    program: &LaidOutProgram,
    clock: AbsoluteAddr,
) -> Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>> {
    let events = &program.design.events;
    if events.reset_clocks.contains_key(&clock)
        || !events.combinationally_read.contains(&clock)
        || !program
            .runtime()
            .runtime_schema
            .persistent_comb_state
            .is_empty()
        || !program.runtime().runtime_schema.comb_observers.is_empty()
    {
        return None;
    }
    let is_alias =
        |address: RegionedAbsoluteAddr| events.canonical(address.absolute_addr()) == clock;
    let mut result = Vec::new();
    for unit in &program.sir.eval_comb {
        let cfg = celox_sir::cfg::SirCfg::analyze_forward(unit).ok()?;
        // A copy must execute exactly once on every successful evaluation.
        // Exclude error exits rather than hoisting through them. Other logic
        // may contain loops; only the copied stores must be outside cycles.
        if unit
            .blocks
            .values()
            .any(|block| matches!(block.terminator, SIRTerminator::Error(_)))
        {
            return None;
        }
        let exits: Vec<_> = unit
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Return))
            .map(|block| cfg.index[&block.id])
            .collect();
        let mut loads = HashMap::default();
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                if matches!(
                    instruction,
                    SIRInstruction::ExternCall { .. }
                        | SIRInstruction::RuntimeEvent { .. }
                        | SIRInstruction::CombCaptureEvent { .. }
                        | SIRInstruction::CombCaptureEnableIfChanged { .. }
                ) {
                    return None;
                }
                if let Some(read) = instruction.memory_read() {
                    if is_alias(*read.addr) {
                        // Only direct root loads: alias chains and working-state
                        // loads require a more general dependency proof.
                        match instruction {
                            SIRInstruction::Load(reg, addr, SIROffset::Static(0), 1)
                                if addr.region == 0 && addr.absolute_addr() == clock =>
                            {
                                loads.insert(*reg, instruction.clone());
                            }
                            _ => return None,
                        }
                    }
                }
            }
        }
        let mut stores = Vec::new();
        for id in &cfg.block_ids {
            let block = &unit.blocks[id];
            let mut used = false;
            block
                .terminator
                .for_each_use(|reg| used |= loads.contains_key(&reg));
            if used {
                return None;
            }
            for instruction in &block.instructions {
                let mut uses_clock = false;
                instruction.for_each_use(|reg| uses_clock |= loads.contains_key(&reg));
                let writes_alias = instruction
                    .memory_write()
                    .is_some_and(|write| is_alias(*write.addr));
                if !uses_clock && !writes_alias {
                    continue;
                }
                match instruction {
                    SIRInstruction::Store(addr, SIROffset::Static(0), 1, source, _, captures)
                        if addr.region == 0
                            && addr.absolute_addr() != clock
                            && is_alias(*addr)
                            && loads.contains_key(source)
                            && captures.is_empty()
                            && !cfg.sccs[cfg.scc_for_block[cfg.index[id]]].cyclic
                            && exits
                                .iter()
                                .all(|&exit| cfg.dominators.dominates(cfg.index[id], exit)) =>
                    {
                        stores.push(instruction.clone());
                    }
                    _ => return None,
                }
            }
        }
        if stores.is_empty() {
            continue;
        }
        let mut registers: Vec<RegisterId> = loads.keys().copied().collect();
        registers.sort_unstable();
        let register_map = registers
            .iter()
            .map(|reg| (*reg, unit.register_map[reg].clone()))
            .collect();
        let mut instructions: Vec<_> = registers.iter().map(|reg| loads[reg].clone()).collect();
        instructions.extend(stores);
        let id = BlockId(0);
        result.push(ExecutionUnit {
            entry_block_id: id,
            blocks: [(
                id,
                BasicBlock {
                    id,
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        });
    }
    // No exports for unused clocks: their ordinary topology already avoids settling.
    (!result.is_empty()).then_some(result)
}
