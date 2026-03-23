use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

pub(super) trait ExecutionUnitPass {
    fn name(&self) -> &'static str;
    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions);
}

#[derive(Default)]
pub(super) struct ExecutionUnitPassManager {
    passes: Vec<Box<dyn ExecutionUnitPass>>,
}

impl ExecutionUnitPassManager {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn add_pass<P>(&mut self, pass: P)
    where
        P: ExecutionUnitPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub(super) fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
        for pass in &self.passes {
            let start = timing.then(crate::timing::now);
            pass.run(eu, options);
            if let Some(start) = start {
                let elapsed = start.elapsed();
                if elapsed.as_millis() > 0 {
                    eprintln!("[pass-timing] {:>40}: {:?}", pass.name(), elapsed);
                }
            }
        }
    }
}
