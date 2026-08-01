use crate::HashMap;
use crate::ir::{AbsoluteAddr, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset};
use crate::optimizer::PassOptions;
use std::sync::Arc;

pub(super) trait ExecutionUnitPass {
    fn name(&self) -> &'static str;
    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions);
}

#[derive(Default)]
pub(super) struct ExecutionUnitPassManager {
    passes: Vec<Box<dyn ExecutionUnitPass>>,
    unpacked_element_widths: Arc<HashMap<AbsoluteAddr, usize>>,
}

impl ExecutionUnitPassManager {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn with_unpacked_element_widths(
        mut self,
        unpacked_element_widths: Arc<HashMap<AbsoluteAddr, usize>>,
    ) -> Self {
        self.unpacked_element_widths = unpacked_element_widths;
        self
    }

    pub(super) fn add_pass<P>(&mut self, pass: P)
    where
        P: ExecutionUnitPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub(super) fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
        let verify_boundaries =
            cfg!(debug_assertions) || std::env::var_os("CELOX_SIR_VERIFY").is_some();
        let verify_passes = std::env::var_os("CELOX_SIR_VERIFY_PASSES").is_some();
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
                    eprintln!("[pass-timing] {:>40}: {:?}", pass.name(), elapsed);
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
) -> Result<(), String> {
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
                format!(
                    "{operation} at b{}/{instruction_index} overflows its unpacked range: address={address:?}, start={start}, width={width}, element_width={element_width}",
                    block.id.0
                )
            })?;
            if width != 0 && *start / element_width != end.saturating_sub(1) / element_width {
                return Err(format!(
                    "{operation} at b{}/{instruction_index} crosses an unpacked element: address={address:?}, start={start}, width={width}, element_width={element_width}",
                    block.id.0
                ));
            }
        }
    }
    Ok(())
}
