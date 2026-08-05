use std::collections::BTreeMap;

use celox_design::{
    BitAccess, InstanceId, ModuleId, SPARSE_WORKING_REGION, STABLE_REGION, TriggerSet, VarAtomBase,
    WORKING_REGION,
};
use celox_sir::{SIRBuilder, SIRInstruction, SIROffset};
use celox_slt::{FfAccessSummary, scheduler};
use veryl_analyzer::ir::{Declaration, FfDeclaration, Module, VarId};

use crate::{
    AbsoluteAddr, BuildConfig, HashMap, HashSet, ParserError, RegionedAbsoluteAddr,
    RegionedVarAddr, SimModule, ff,
};

#[derive(Clone)]
pub struct FfRuntimeRelocation {
    pub error_codes: HashMap<i64, i64>,
    pub event_site_base: u32,
}

#[derive(Clone)]
pub struct FfClockRecipe<'a> {
    pub id: usize,
    instance_id: InstanceId,
    module: &'a Module,
    declarations: Vec<&'a FfDeclaration>,
    summary: FfAccessSummary<RegionedAbsoluteAddr>,
    runtime: FfRuntimeRelocation,
}

pub struct SharedClockLowering<'a> {
    recipes: Vec<FfClockRecipe<'a>>,
    summaries: Vec<FfAccessSummary<RegionedAbsoluteAddr>>,
    config: BuildConfig,
}

impl<'a> SharedClockLowering<'a> {
    pub fn new(recipes: Vec<FfClockRecipe<'a>>, config: BuildConfig) -> Self {
        let summaries = recipes
            .iter()
            .map(|recipe| recipe.summary.clone())
            .collect();
        Self {
            recipes,
            summaries,
            config,
        }
    }

    fn direct_write(
        direct_writes: &[VarAtomBase<RegionedAbsoluteAddr>],
        write: &VarAtomBase<RegionedAbsoluteAddr>,
    ) -> bool {
        direct_writes.contains(write)
    }

    fn direct_dynamic_var(
        summary: &FfAccessSummary<RegionedAbsoluteAddr>,
        direct_writes: &[VarAtomBase<RegionedAbsoluteAddr>],
        address: AbsoluteAddr,
    ) -> bool {
        let mut writes = summary
            .writes
            .iter()
            .filter(|write| write.id.absolute_addr() == address)
            .peekable();
        writes.peek().is_some() && writes.all(|write| Self::direct_write(direct_writes, write))
    }

    fn emit_region_copies(
        builder: &mut SIRBuilder<RegionedAbsoluteAddr>,
        summaries: &[FfAccessSummary<RegionedAbsoluteAddr>],
        direct_writes: &[Vec<VarAtomBase<RegionedAbsoluteAddr>>],
        src_region: u32,
        dst_region: u32,
    ) {
        let dynamic = summaries
            .iter()
            .enumerate()
            .flat_map(|(index, summary)| {
                let direct_writes = direct_writes.get(index).map_or(&[][..], Vec::as_slice);
                summary.dynamic_writes.iter().filter(move |address| {
                    !Self::direct_dynamic_var(summary, direct_writes, address.absolute_addr())
                })
            })
            .map(RegionedAbsoluteAddr::absolute_addr)
            .collect::<HashSet<_>>();
        let mut ranges = BTreeMap::<AbsoluteAddr, Vec<BitAccess>>::new();
        for target in summaries.iter().enumerate().flat_map(|(index, summary)| {
            let direct_writes = direct_writes.get(index).map_or(&[][..], Vec::as_slice);
            summary
                .writes
                .iter()
                .filter(move |target| !Self::direct_write(direct_writes, target))
        }) {
            let addr = target.id.absolute_addr();
            if !dynamic.contains(&addr) {
                ranges.entry(addr).or_default().push(target.access);
            }
        }
        for (addr, mut ranges) in ranges {
            ranges.sort_unstable_by_key(|range| (range.lsb, range.msb));
            let mut merged = Vec::<BitAccess>::new();
            for range in ranges {
                if let Some(previous) = merged.last_mut()
                    && range.lsb <= previous.msb.saturating_add(1)
                {
                    previous.msb = previous.msb.max(range.msb);
                } else {
                    merged.push(range);
                }
            }
            for range in merged {
                builder.emit(SIRInstruction::Commit(
                    RegionedAbsoluteAddr::from_absolute_addr(src_region, addr),
                    RegionedAbsoluteAddr::from_absolute_addr(dst_region, addr),
                    SIROffset::Static(range.lsb),
                    range.msb - range.lsb + 1,
                    Vec::new(),
                ));
            }
        }
    }

    fn emit_sparse_commits(
        builder: &mut SIRBuilder<RegionedAbsoluteAddr>,
        summaries: &[FfAccessSummary<RegionedAbsoluteAddr>],
        direct_writes: &[Vec<VarAtomBase<RegionedAbsoluteAddr>>],
    ) {
        let dynamic = summaries
            .iter()
            .enumerate()
            .flat_map(|(index, summary)| {
                let direct_writes = direct_writes.get(index).map_or(&[][..], Vec::as_slice);
                summary.dynamic_writes.iter().filter(move |address| {
                    !Self::direct_dynamic_var(summary, direct_writes, address.absolute_addr())
                })
            })
            .map(RegionedAbsoluteAddr::absolute_addr)
            .collect::<HashSet<_>>();
        let mut widths = BTreeMap::<AbsoluteAddr, usize>::new();
        for target in summaries.iter().enumerate().flat_map(|(index, summary)| {
            let direct_writes = direct_writes.get(index).map_or(&[][..], Vec::as_slice);
            summary
                .writes
                .iter()
                .filter(move |target| !Self::direct_write(direct_writes, target))
        }) {
            let addr = target.id.absolute_addr();
            if dynamic.contains(&addr) {
                widths
                    .entry(addr)
                    .and_modify(|width| *width = (*width).max(target.access.msb.saturating_add(1)))
                    .or_insert_with(|| target.access.msb.saturating_add(1));
            }
        }
        for (addr, width) in widths {
            builder.emit(SIRInstruction::Commit(
                RegionedAbsoluteAddr::from_absolute_addr(SPARSE_WORKING_REGION, addr),
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                SIROffset::Static(0),
                width,
                Vec::new(),
            ));
        }
    }
}

impl scheduler::ClockFfLowering<RegionedAbsoluteAddr> for SharedClockLowering<'_> {
    type Error = ParserError;

    fn summaries(&self) -> &[FfAccessSummary<RegionedAbsoluteAddr>] {
        &self.summaries
    }

    fn begin(
        &mut self,
        builder: &mut SIRBuilder<RegionedAbsoluteAddr>,
        direct_writes: &[Vec<VarAtomBase<RegionedAbsoluteAddr>>],
    ) -> Result<(), ParserError> {
        // Only ranges whose old-state anti-dependencies could not be ordered
        // need a private snapshot.
        Self::emit_region_copies(
            builder,
            &self.summaries,
            direct_writes,
            STABLE_REGION,
            WORKING_REGION,
        );
        Ok(())
    }

    fn lower(
        &mut self,
        index: usize,
        direct_writes: &[VarAtomBase<RegionedAbsoluteAddr>],
        builder: &mut SIRBuilder<RegionedAbsoluteAddr>,
    ) -> Result<(), ParserError> {
        let recipe = self.recipes.get(index).ok_or_else(|| {
            ParserError::illegal_context(
                "shared comb/FF scheduling",
                format!("FF action {index} is outside the recipe table"),
                None,
            )
        })?;
        let sparse_write_vars = recipe
            .summary
            .dynamic_writes
            .iter()
            .map(|address| address.var_id)
            .collect();
        let direct_static_ranges = recipe
            .summary
            .writes
            .iter()
            .filter(|write| Self::direct_write(direct_writes, write))
            .fold(
                HashMap::<VarId, Vec<BitAccess>>::default(),
                |mut ranges, write| {
                    ranges
                        .entry(write.id.var_id)
                        .or_default()
                        .push(write.access);
                    ranges
                },
            );
        let direct_dynamic_vars = recipe
            .summary
            .dynamic_writes
            .iter()
            .filter_map(|address| {
                Self::direct_dynamic_var(&recipe.summary, direct_writes, address.absolute_addr())
                    .then_some(address.var_id)
            })
            .collect::<HashSet<_>>();
        let mut parser = ff::FfParser::new(recipe.module, self.config)
            .with_relocated_runtime_ids(
                recipe.runtime.error_codes.clone(),
                recipe.runtime.event_site_base,
            )
            .with_sparse_write_vars(sparse_write_vars)
            .with_direct_write_ranges(direct_static_ranges, direct_dynamic_vars);
        parser.parse_ff_group_into(
            &recipe.declarations,
            &|var_id, region| RegionedAbsoluteAddr {
                region,
                instance_id: recipe.instance_id,
                var_id,
            },
            builder,
        )?;
        Ok(())
    }

    fn finish(
        &mut self,
        builder: &mut SIRBuilder<RegionedAbsoluteAddr>,
        direct_writes: &[Vec<VarAtomBase<RegionedAbsoluteAddr>>],
    ) -> Result<(), ParserError> {
        // Staged ranges publish together. Proven direct ranges have already
        // published at their scheduled placement.
        Self::emit_region_copies(
            builder,
            &self.summaries,
            direct_writes,
            WORKING_REGION,
            STABLE_REGION,
        );
        Self::emit_sparse_commits(builder, &self.summaries, direct_writes);
        Ok(())
    }
}

pub fn build_ff_clock_recipes<'a>(
    module_ir: &'a HashMap<ModuleId, &'a Module>,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    runtime_relocations: &HashMap<InstanceId, FfRuntimeRelocation>,
    config: BuildConfig,
) -> HashMap<AbsoluteAddr, Vec<FfClockRecipe<'a>>> {
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    let mut result = HashMap::<AbsoluteAddr, Vec<FfClockRecipe<'a>>>::default();
    let mut next_recipe_id = 0usize;

    for (&instance_id, &module_id) in instances {
        let module = module_ir[&module_id];
        let sim_module = &modules[&module_id];
        let detector = ff::FfParser::new(module, config);
        let mut groups = BTreeMap::<TriggerSet<VarId>, Vec<&FfDeclaration>>::new();
        for declaration in &module.declarations {
            if let Declaration::Ff(ff) = declaration {
                groups
                    .entry(detector.detect_trigger_set(ff))
                    .or_default()
                    .push(ff);
            }
        }
        for (trigger, declarations) in groups {
            let Some(summary) = sim_module.ff_access_summaries.get(&trigger) else {
                continue;
            };
            let relocate_addr = |addr: RegionedVarAddr| RegionedAbsoluteAddr {
                region: addr.region,
                instance_id,
                var_id: addr.var_id,
            };
            let summary = FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate_addr(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        // Scheduler summaries describe the persistent state
                        // object, not the temporary region chosen by FF
                        // lowering.  Reads and comb definitions use STABLE;
                        // normalize writes to the same identity so old-state
                        // anti-dependencies are visible.
                        id: RegionedAbsoluteAddr {
                            region: STABLE_REGION,
                            instance_id,
                            var_id: write.id.var_id,
                        },
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .map(|address| RegionedAbsoluteAddr {
                        region: STABLE_REGION,
                        instance_id,
                        var_id: address.var_id,
                    })
                    .collect(),
            };
            let recipe = FfClockRecipe {
                id: next_recipe_id,
                instance_id,
                module,
                declarations,
                summary,
                runtime: runtime_relocations[&instance_id].clone(),
            };
            next_recipe_id += 1;
            let clock = AbsoluteAddr {
                instance_id,
                var_id: trigger.clock,
            };
            let clock = clock_domains.get(&clock).copied().unwrap_or(clock);
            result.entry(clock).or_default().push(recipe.clone());
            for reset in trigger.resets {
                let reset = AbsoluteAddr {
                    instance_id,
                    var_id: reset,
                };
                let reset = clock_domains.get(&reset).copied().unwrap_or(reset);
                result.entry(reset).or_default().push(recipe.clone());
            }
        }
    }
    result
}
