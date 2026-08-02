use crate::HashMap;
use crate::PassOptions;
use crate::ir::{AbsoluteAddr, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset};
use std::sync::Arc;

pub(in crate::optimizer) trait ExecutionUnitPass {
    fn name(&self) -> &'static str;
    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions);
}

#[derive(Default)]
pub(in crate::optimizer) struct ExecutionUnitPassManager {
    passes: Vec<Box<dyn ExecutionUnitPass>>,
    unpacked_element_widths: Arc<HashMap<AbsoluteAddr, usize>>,
}

impl ExecutionUnitPassManager {
    pub(in crate::optimizer) fn new() -> Self {
        Self::default()
    }

    pub(in crate::optimizer) fn with_unpacked_element_widths(
        mut self,
        unpacked_element_widths: Arc<HashMap<AbsoluteAddr, usize>>,
    ) -> Self {
        self.unpacked_element_widths = unpacked_element_widths;
        self
    }

    pub(in crate::optimizer) fn add_pass<P>(&mut self, pass: P)
    where
        P: ExecutionUnitPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub(in crate::optimizer) fn run(
        &self,
        eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        options: &PassOptions,
    ) {
        let diagnostics = &options.optimize_options.diagnostics;
        let timing = diagnostics.pass_timing;
        let verify_boundaries = cfg!(debug_assertions) || diagnostics.verify_boundaries;
        let verify_passes = diagnostics.verify_passes;
        if verify_boundaries {
            if let Err(error) = eu.verify_result() {
                panic!("before SIR pass pipeline: {error}");
            }
        }
        if verify_passes
            && let Err(error) =
                verify_unpacked_element_boundaries(eu, &self.unpacked_element_widths)
        {
            panic!("before SIR pass pipeline: {error}");
        }
        for pass in &self.passes {
            let start = timing.then(crate::timing::now);
            pass.run(eu, options);
            if verify_passes {
                if let Err(error) = eu.verify_result() {
                    panic!("after SIR pass {}: {error}", pass.name());
                }
                if let Err(error) =
                    verify_unpacked_element_boundaries(eu, &self.unpacked_element_widths)
                {
                    panic!("after SIR pass {}: {error}", pass.name());
                }
            }
            if let Some(start) = start {
                let elapsed = start.elapsed();
                if elapsed.as_millis() > 0 {
                    tracing::debug!("[pass-timing] {:>40}: {:?}", pass.name(), elapsed);
                }
            }
        }
        if verify_boundaries {
            if let Err(error) = eu.verify_result() {
                panic!("after SIR pass pipeline: {error}");
            }
        }
    }
}

pub(super) fn verify_unpacked_element_boundaries(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> Result<(), crate::OptimizationError> {
    for block in eu.blocks.values() {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let (address, offset, width, operation) = match instruction {
                SIRInstruction::Load(_, address, offset, width) => {
                    (address, offset, *width, "Load")
                }
                SIRInstruction::Store(address, offset, width, ..) => {
                    (address, offset, *width, "Store")
                }
                _ => continue,
            };
            let SIROffset::Static(start) = offset else {
                continue;
            };
            let Some(&element_width) = unpacked_element_widths.get(&address.absolute_addr()) else {
                continue;
            };
            let end = start.checked_add(width).ok_or_else(|| {
                crate::OptimizationError::invariant(
                    "unpacked element boundary verification",
                    format!(
                    "{operation} at b{}/{instruction_index} overflows its unpacked range: address={address:?}, start={start}, width={width}, element_width={element_width}",
                    block.id.0
                    ),
                )
            })?;
            if width != 0 && *start / element_width != end.saturating_sub(1) / element_width {
                return Err(crate::OptimizationError::invariant(
                    "unpacked element boundary verification",
                    format!(
                        "{operation} at b{}/{instruction_index} crosses an unpacked element: address={address:?}, start={start}, width={width}, element_width={element_width}",
                        block.id.0
                    ),
                ));
            }
        }
    }
    Ok(())
}
