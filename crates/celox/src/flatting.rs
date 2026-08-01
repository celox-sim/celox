pub use celox_frontend_veryl::flattening::{collect_inputs, flatten_module};

use crate::{HashMap, ir};
use std::collections::BTreeSet;

pub fn flatting(
    module: &ir::SimModule,
    path: &ir::InstancePath,
    instance_ids: &HashMap<ir::InstancePath, ir::InstanceId>,
    global_boundaries: &HashMap<ir::AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<ir::AbsoluteAddr, usize>,
    arena: &mut crate::logic_tree::SLTNodeArena<ir::AbsoluteAddr>,
    trace_opts: &celox_frontend_veryl::FrontendTraceOptions,
    mut trace: Option<&mut celox_frontend_veryl::FrontendTrace>,
) -> Result<ir::RelocationModule, crate::logic_tree::SLTNodeFactsError> {
    let flattened = flatten_module(
        module,
        path,
        instance_ids,
        global_boundaries,
        unpacked_element_widths,
        arena,
    )?;

    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        match &mut trace.pre_atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.pre_atomized_comb_blocks);
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.pre_atomized_comb_blocks, arena.clone()));
            }
        }
    }

    if let Some(trace) = trace
        && trace_opts.atomized_comb_blocks
    {
        match &mut trace.atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.relocation.comb_blocks.iter().cloned());
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.relocation.comb_blocks.clone(), arena.clone()));
            }
        }
    }

    Ok(flattened.relocation)
}
