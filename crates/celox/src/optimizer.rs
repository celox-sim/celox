use crate::ir::Program;

pub mod coalescing;

#[derive(Debug, Clone, Copy)]
pub struct PassOptions {
    pub max_inflight_loads: usize,
    pub four_state: bool,
}

impl Default for PassOptions {
    fn default() -> Self {
        Self {
            max_inflight_loads: 8,
            four_state: false,
        }
    }
}

pub trait ProgramPass {
    fn name(&self) -> &'static str;
    fn run(&self, program: &mut Program, options: &PassOptions);
}

#[derive(Default)]
pub struct PassManager {
    passes: Vec<Box<dyn ProgramPass>>,
}

impl PassManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pass<P>(&mut self, pass: P)
    where
        P: ProgramPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub fn run(&self, program: &mut Program, options: &PassOptions) {
        for pass in &self.passes {
            let _ = pass.name();
            pass.run(program, options);
        }
    }
}

/// Run only the tail-call splitting pass (no other optimizations).
/// Used when `optimize=false` to still avoid exceeding Cranelift's instruction limit.
pub fn split_if_needed(program: &mut Program, four_state: bool) {
    use coalescing::pass_tail_call_split;

    // split_with_threshold / split_multi_block_with_threshold do their own
    // cost checks internally, so we delegate directly without a redundant
    // estimate_units_cost call here.
    if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state) {
        program.eval_comb_plan = Some(crate::ir::EvalCombPlan::TailCallChunks(chunks));
    } else if let Some(plan) = pass_tail_call_split::split_if_needed_spilled(&program.eval_comb, four_state) {
        program.eval_comb_plan = Some(crate::ir::EvalCombPlan::MemorySpilled(plan));
    }
}

pub fn optimize(program: &mut Program, four_state: bool) {
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(
        program,
        &PassOptions {
            four_state,
            ..PassOptions::default()
        },
    );
}
