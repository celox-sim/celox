#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
use std::sync::Arc;

#[cfg(feature = "host-runtime")]
use crate::ir::LaidOutProgram;
use crate::ir::{AbsoluteAddr, OptimizedSir};
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
use crate::ir::{
    ExecutionUnit, RegionedAbsoluteAddr, SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION,
};

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub use celox_sir_opt::optimizer::pass_eliminate_working_round_trip;
pub(crate) use celox_sir_opt::optimizer::{
    eliminate_shared_comb_state_stores, promote_fused_comb_static_slots,
    remove_dead_sir_definitions,
};
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub(crate) use celox_sir_opt::optimizer::{
    eliminate_unobserved_comb_state_stores, promote_eval_apply_working_round_trips,
};

pub(crate) fn retain_final_identity_aliases(program: &mut OptimizedSir, four_state: bool) {
    super::with_optimized_program(program, |unit| {
        celox_sir_opt::optimizer::retain_final_identity_aliases(unit, four_state);
    });
}

pub(crate) fn remove_final_identity_alias_stores(
    program: &mut OptimizedSir,
    validated_aliases: &crate::HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) {
    super::with_optimized_program(program, |unit| {
        celox_sir_opt::optimizer::remove_final_identity_alias_stores(
            unit,
            validated_aliases,
            four_state,
        );
    });
}

#[cfg(feature = "host-runtime")]
pub(crate) fn optimize_rooted_comb_memory(
    program: &mut LaidOutProgram,
    externally_live: &crate::HashSet<AbsoluteAddr>,
    four_state: bool,
) {
    super::with_laid_out_program(program, |unit| {
        celox_sir_opt::optimizer::optimize_rooted_comb_memory(unit, externally_live, four_state);
    });
}

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub(crate) fn optimize_native_merged_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &crate::backend::MemoryLayout,
    four_state: bool,
    recover_merged_effect_regions: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
) -> Result<(), (&'static str, celox_sir::verify::SirVerifyError)> {
    let element_widths = Arc::new(
        layout
            .unpacked_arrays
            .iter()
            .filter(|(_, array)| {
                (1..=64).contains(&array.element_width)
                    && array.element_stride * 8 != array.element_width
                    && array.plane_size <= 256
            })
            .flat_map(|(&address, array)| {
                [STABLE_REGION, WORKING_REGION, SPARSE_WORKING_REGION].map(move |region| {
                    (
                        RegionedAbsoluteAddr::from_absolute_addr(region, address),
                        array.element_width,
                    )
                })
            })
            .collect(),
    );
    celox_sir_opt::optimizer::optimize_merged_chain(
        eu,
        element_widths,
        |destination, start, width| {
            packed_range_is_physically_contiguous(layout, destination, start, width)
        },
        four_state,
        recover_merged_effect_regions,
        diagnostics,
    )
}

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn packed_range_is_physically_contiguous(
    layout: &crate::backend::MemoryLayout,
    destination: RegionedAbsoluteAddr,
    start: usize,
    width: usize,
) -> bool {
    let Some((base_byte, base_bit)) = layout.regioned_static_byte_and_intra(&destination, start)
    else {
        return false;
    };
    (0..width).all(|bit| {
        let Some(bit_offset) = start.checked_add(bit) else {
            return false;
        };
        let Some((byte, intra)) = layout.regioned_static_byte_and_intra(&destination, bit_offset)
        else {
            return false;
        };
        let physical_bit = base_bit + bit;
        byte == base_byte + i32::try_from(physical_bit / 8).unwrap_or(i32::MAX)
            && intra == physical_bit % 8
    })
}
