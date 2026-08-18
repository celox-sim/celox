use std::collections::BTreeMap;

use celox_design::{
    BitAccess, InstanceId, ModuleId, SPARSE_WORKING_REGION, STABLE_REGION, TriggerSet, VarAtomBase,
    WORKING_REGION,
};
use celox_frontend_core::{
    ParserError as CoreParserError, SourceVarId,
    symbolic::assembly::{FfRuntimeRelocation, FusedFfAction, FusedFfLoweringFactory},
};
use celox_sir::{SIRBuilder, SIRInstruction, SIROffset};
use celox_slt::{FfAccessSummary, scheduler};
use veryl_analyzer::ir::{Declaration, FfDeclaration, Module, VarId};

use crate::{BuildConfig, HashMap, HashSet, ff};

type FusedAbsoluteAddr = celox_design::AbsoluteAddrBase<SourceVarId>;
type FusedRegionedAddr = celox_design::RegionedAbsoluteAddrBase<SourceVarId>;

#[derive(Clone)]
struct FfClockRecipe<'a> {
    instance_id: InstanceId,
    module: &'a Module,
    declarations: Vec<&'a FfDeclaration>,
    summary: FfAccessSummary<FusedRegionedAddr>,
    runtime: FfRuntimeRelocation,
    veryl_to_source: &'a HashMap<VarId, SourceVarId>,
    source_to_veryl: HashMap<SourceVarId, VarId>,
}

struct SharedClockLowering<'a> {
    recipes: Vec<FfClockRecipe<'a>>,
    summaries: Vec<FfAccessSummary<FusedRegionedAddr>>,
    config: BuildConfig,
}

impl<'a> SharedClockLowering<'a> {
    fn new(recipes: Vec<FfClockRecipe<'a>>, config: BuildConfig) -> Self {
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
        direct_writes: &[VarAtomBase<FusedRegionedAddr>],
        write: &VarAtomBase<FusedRegionedAddr>,
    ) -> bool {
        direct_writes.contains(write)
    }

    fn direct_dynamic_var(
        summary: &FfAccessSummary<FusedRegionedAddr>,
        direct_writes: &[VarAtomBase<FusedRegionedAddr>],
        address: FusedAbsoluteAddr,
    ) -> bool {
        let mut writes = summary
            .writes
            .iter()
            .filter(|write| write.id.absolute_addr() == address)
            .peekable();
        writes.peek().is_some() && writes.all(|write| Self::direct_write(direct_writes, write))
    }

    fn emit_region_copies(
        builder: &mut SIRBuilder<FusedRegionedAddr>,
        summaries: &[FfAccessSummary<FusedRegionedAddr>],
        direct_writes: &[Vec<VarAtomBase<FusedRegionedAddr>>],
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
            .map(FusedRegionedAddr::absolute_addr)
            .collect::<HashSet<_>>();
        let mut ranges = BTreeMap::<FusedAbsoluteAddr, Vec<BitAccess>>::new();
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
                    FusedRegionedAddr::from_absolute_addr(src_region, addr),
                    FusedRegionedAddr::from_absolute_addr(dst_region, addr),
                    SIROffset::Static(range.lsb),
                    range.msb - range.lsb + 1,
                    Vec::new(),
                ));
            }
        }
    }

    fn emit_sparse_commits(
        builder: &mut SIRBuilder<FusedRegionedAddr>,
        summaries: &[FfAccessSummary<FusedRegionedAddr>],
        direct_writes: &[Vec<VarAtomBase<FusedRegionedAddr>>],
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
            .map(FusedRegionedAddr::absolute_addr)
            .collect::<HashSet<_>>();
        let mut widths = BTreeMap::<FusedAbsoluteAddr, usize>::new();
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
                FusedRegionedAddr::from_absolute_addr(SPARSE_WORKING_REGION, addr),
                FusedRegionedAddr::from_absolute_addr(STABLE_REGION, addr),
                SIROffset::Static(0),
                width,
                Vec::new(),
            ));
        }
    }
}

impl scheduler::ClockFfLowering<FusedRegionedAddr> for SharedClockLowering<'_> {
    type Error = CoreParserError;

    fn summaries(&self) -> &[FfAccessSummary<FusedRegionedAddr>] {
        &self.summaries
    }

    fn begin(
        &mut self,
        builder: &mut SIRBuilder<FusedRegionedAddr>,
        direct_writes: &[Vec<VarAtomBase<FusedRegionedAddr>>],
    ) -> Result<(), CoreParserError> {
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
        direct_writes: &[VarAtomBase<FusedRegionedAddr>],
        builder: &mut SIRBuilder<FusedRegionedAddr>,
    ) -> Result<(), CoreParserError> {
        let recipe = self.recipes.get(index).ok_or_else(|| {
            CoreParserError::illegal_context(
                "shared comb/FF scheduling",
                format!("FF action {index} is outside the recipe table"),
                None,
            )
        })?;
        let sparse_write_vars = recipe
            .summary
            .dynamic_writes
            .iter()
            .map(|address| recipe.source_to_veryl[&address.var_id])
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
                        .entry(recipe.source_to_veryl[&write.id.var_id])
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
                    .then_some(recipe.source_to_veryl[&address.var_id])
            })
            .collect::<HashSet<_>>();
        let mut parser = ff::FfParser::new(recipe.module, self.config)
            .with_relocated_runtime_ids(
                recipe.runtime.error_codes.clone(),
                recipe.runtime.event_site_base,
            )
            .with_sparse_write_vars(sparse_write_vars)
            .with_direct_write_ranges(direct_static_ranges, direct_dynamic_vars);
        parser
            .parse_ff_group_into(
                &recipe.declarations,
                &|var_id, region| FusedRegionedAddr {
                    region,
                    instance_id: recipe.instance_id,
                    var_id: recipe.veryl_to_source[&var_id],
                },
                builder,
            )
            .map_err(|error| {
                CoreParserError::illegal_context("Veryl fused FF lowering", error.to_string(), None)
            })?;
        Ok(())
    }

    fn finish(
        &mut self,
        builder: &mut SIRBuilder<FusedRegionedAddr>,
        direct_writes: &[Vec<VarAtomBase<FusedRegionedAddr>>],
    ) -> Result<(), CoreParserError> {
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

pub(crate) struct VerylFusedFfFactory<'a> {
    module_ir: &'a HashMap<ModuleId, &'a Module>,
    source_id_maps: &'a HashMap<ModuleId, HashMap<VarId, SourceVarId>>,
    config: BuildConfig,
}

impl<'a> VerylFusedFfFactory<'a> {
    pub(crate) fn new(
        module_ir: &'a HashMap<ModuleId, &'a Module>,
        source_id_maps: &'a HashMap<ModuleId, HashMap<VarId, SourceVarId>>,
        config: BuildConfig,
    ) -> Self {
        Self {
            module_ir,
            source_id_maps,
            config,
        }
    }
}

impl FusedFfLoweringFactory for VerylFusedFfFactory<'_> {
    fn create(
        &self,
        actions: Vec<FusedFfAction>,
    ) -> Result<
        Box<dyn scheduler::ClockFfLowering<FusedRegionedAddr, Error = CoreParserError> + '_>,
        CoreParserError,
    > {
        let mut recipes = Vec::with_capacity(actions.len());
        for action in actions {
            let module = self.module_ir[&action.module_id];
            let veryl_to_source = &self.source_id_maps[&action.module_id];
            let source_to_veryl = veryl_to_source
                .iter()
                .map(|(&veryl, &source)| (source, veryl))
                .collect::<HashMap<_, _>>();
            let trigger = TriggerSet {
                clock: source_to_veryl[&action.trigger.clock],
                resets: action
                    .trigger
                    .resets
                    .iter()
                    .map(|reset| source_to_veryl[reset])
                    .collect(),
            };
            let detector = ff::FfParser::new(module, self.config);
            let mut groups = BTreeMap::<TriggerSet<VarId>, Vec<&FfDeclaration>>::new();
            for declaration in &module.declarations {
                if let Declaration::Ff(ff) = declaration {
                    groups
                        .entry(detector.detect_trigger_set(ff))
                        .or_default()
                        .push(ff);
                }
            }
            let declarations = groups.remove(&trigger).ok_or_else(|| {
                CoreParserError::illegal_context(
                    "Veryl fused FF lowering",
                    format!(
                        "module {:?} has no FF declaration group for the projected trigger",
                        action.module_id
                    ),
                    None,
                )
            })?;
            recipes.push(FfClockRecipe {
                instance_id: action.instance_id,
                module,
                declarations,
                summary: action.summary,
                runtime: action.runtime,
                veryl_to_source,
                source_to_veryl,
            });
        }
        Ok(Box::new(SharedClockLowering::new(recipes, self.config)))
    }
}
