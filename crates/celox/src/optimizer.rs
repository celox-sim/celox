use crate::ir::Program;

pub mod coalescing;

#[derive(Debug, Clone, Copy)]
pub struct PassOptions {
    pub max_inflight_loads: usize,
}

impl Default for PassOptions {
    fn default() -> Self {
        Self {
            max_inflight_loads: 8,
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

pub fn optimize(program: &mut Program) {
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(program, &PassOptions::default());
}
