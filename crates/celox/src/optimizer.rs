use crate::ir::Program;
pub use celox_sir_opt::{OptLevel, OptimizeOptions, PassOptions, SirPass};

pub mod coalescing;

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

pub fn optimize(program: &mut Program, four_state: bool, optimize_options: &OptimizeOptions) {
    optimize_impl(program, four_state, optimize_options, false);
}

pub(crate) fn optimize_preserving_element_storage(
    program: &mut Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, true);
}

fn optimize_impl(
    program: &mut Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(
        program,
        &PassOptions {
            four_state,
            optimize_options: optimize_options.clone(),
            preserve_element_storage_layout,
            ..PassOptions::default()
        },
    );
}
