use crate::HashMap;
use crate::PassOptions;
use crate::ir::{AbsoluteAddr, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset};
use std::sync::Arc;

pub(in crate::optimizer) trait ExecutionUnitPass: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions);

    /// Whether the pass handles [`SIRInstruction::Call`]s: it reads every
    /// instruction's state accesses through `state_reads`/`state_writes`
    /// rather than assuming instructions other than Load, Store and Commit
    /// touch no state. A pass that does not is never run on a unit with a
    /// call; a pass opts in once a test with a writing call between two of
    /// its accesses shows it handles them.
    fn handles_calls(&self) -> bool {
        false
    }
}

/// Whether `eu` makes a function call.
pub(in crate::optimizer) fn makes_calls(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    eu.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(instruction, SIRInstruction::Call { .. }))
}

/// Run one pass outside a manager, as the program-wide stages do, unless it
/// does not handle the calls `eu` makes.
pub(in crate::optimizer) fn run_pass(
    pass: &dyn ExecutionUnitPass,
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    options: &PassOptions,
) {
    if pass.handles_calls() || !makes_calls(eu) {
        pass.run(eu, options);
    }
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
        let calls = makes_calls(eu);
        self.run_passes(eu, options, calls);
    }

    /// Run the pipeline over the body of a function unit. Only passes that
    /// handle calls see it, whether or not it makes one: its frame slots
    /// live only during its activation, and the caller's state accesses
    /// surround it, which a pass that does not handle calls may not assume
    /// away.
    pub(in crate::optimizer) fn run_function(
        &self,
        eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        options: &PassOptions,
    ) {
        self.run_passes(eu, options, true);
    }

    fn run_passes(
        &self,
        eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        options: &PassOptions,
        calls: bool,
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
            if calls && !pass.handles_calls() {
                continue;
            }
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

    /// Run the same ordered pipeline over independent execution units.
    ///
    /// Workers take the units in place, the largest first, so only unrelated
    /// per-unit work overlaps and each unit ends as a sequential run leaves
    /// it.
    pub(in crate::optimizer) fn run_parallel(
        &self,
        units: &mut [ExecutionUnit<RegionedAbsoluteAddr>],
        options: &PassOptions,
    ) {
        self.run_each_parallel(units, |unit| unit, Self::run, options);
    }

    /// [`Self::run_function`] over the bodies of function units, as
    /// [`Self::run_parallel`] runs units.
    pub(in crate::optimizer) fn run_functions_parallel<T: Send>(
        &self,
        functions: &mut [T],
        unit: impl Fn(&mut T) -> &mut ExecutionUnit<RegionedAbsoluteAddr> + Sync,
        options: &PassOptions,
    ) {
        self.run_each_parallel(functions, unit, Self::run_function, options);
    }

    fn run_each_parallel<T: Send>(
        &self,
        items: &mut [T],
        unit: impl Fn(&mut T) -> &mut ExecutionUnit<RegionedAbsoluteAddr> + Sync,
        run: impl Fn(&Self, &mut ExecutionUnit<RegionedAbsoluteAddr>, &PassOptions) + Sync,
        options: &PassOptions,
    ) {
        #[cfg(not(target_arch = "wasm32"))]
        const MAX_PARALLEL_UNITS: usize = 4;

        #[cfg(not(target_arch = "wasm32"))]
        let worker_count = items.len().min(
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(MAX_PARALLEL_UNITS),
        );
        #[cfg(target_arch = "wasm32")]
        let worker_count = 1;
        if worker_count <= 1 {
            for item in items {
                run(self, unit(item), options);
            }
            return;
        }

        let mut pending = items
            .iter_mut()
            .enumerate()
            .map(|(index, item)| {
                let eu = unit(item);
                let work = eu.blocks.len()
                    + eu.blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>();
                (work, index, item)
            })
            .collect::<Vec<_>>();
        // Workers pop the largest units first to keep the long tail bounded.
        pending.sort_unstable_by_key(|(work, index, _)| (*work, *index));
        #[expect(
            clippy::disallowed_types,
            reason = "cross-thread: the scoped SIR optimization worker threads pop execution units from this shared work queue"
        )]
        let pending = std::sync::Mutex::new(pending);

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let pending = &pending;
                let unit = &unit;
                let run = &run;
                scope.spawn(move || {
                    loop {
                        let task = pending
                            .lock()
                            .expect("SIR optimization work queue must not be poisoned")
                            .pop();
                        let Some((_, _, item)) = task else {
                            break;
                        };
                        run(self, unit(item), options);
                    }
                });
            }
        });
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
            // A frame slot may share its object identity with a state object,
            // but not that object's elements.
            if address.region == crate::ir::FRAME_REGION {
                continue;
            }
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

#[cfg(test)]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: test passes count their runs on the optimizer's worker threads"
)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, BlockId, RegisterId, RegisterType, SIRTerminator, SIRValue};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A pass that counts the units it runs on.
    struct Counting<'a> {
        runs: &'a AtomicUsize,
        handles_calls: bool,
    }

    impl ExecutionUnitPass for Counting<'static> {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn run(&self, _: &mut ExecutionUnit<RegionedAbsoluteAddr>, _: &PassOptions) {
            self.runs.fetch_add(1, Ordering::Relaxed);
        }

        fn handles_calls(&self) -> bool {
            self.handles_calls
        }
    }

    fn unit(calls: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut instructions = vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8))];
        if calls {
            instructions.push(SIRInstruction::Call {
                function: celox_sir::FunctionId(0),
                frame: RegisterId(0),
                args: Vec::new(),
                outputs: Vec::new(),
                dst: None,
                effects: Default::default(),
            });
        }
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [(
                RegisterId(0),
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn a_pass_that_does_not_opt_in_never_sees_a_call() {
        static PLAIN: AtomicUsize = AtomicUsize::new(0);
        static OPTED: AtomicUsize = AtomicUsize::new(0);
        let mut manager = ExecutionUnitPassManager::new();
        manager.add_pass(Counting {
            runs: &PLAIN,
            handles_calls: false,
        });
        manager.add_pass(Counting {
            runs: &OPTED,
            handles_calls: true,
        });
        let options = PassOptions::default();
        manager.run(&mut unit(true), &options);
        assert_eq!(PLAIN.load(Ordering::Relaxed), 0);
        assert_eq!(OPTED.load(Ordering::Relaxed), 1);
        manager.run(&mut unit(false), &options);
        assert_eq!(PLAIN.load(Ordering::Relaxed), 1);
        assert_eq!(OPTED.load(Ordering::Relaxed), 2);
    }
}
