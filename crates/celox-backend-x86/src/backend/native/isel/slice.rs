//! Keep direct slice loads only while their source memory is unchanged.

use super::*;

impl ISelContext<'_> {
    pub(super) fn update_slice_load_origins(
        &mut self,
        inst: &SIRInstruction<RegionedAbsoluteAddr>,
    ) {
        if matches!(inst, SIRInstruction::ExternCall { func, .. }
            if celox_sir::extern_abi::native_kernel_index(*func).is_some())
        {
            self.reg_addrs.clear();
        }
        if let Some(write) = inst.memory_write()
            && write.width != 0
        {
            let layout = self.layout;
            // Compare physical homes: distinct regions can share a home.
            // Include both value and mask planes, and conservatively cover the
            // whole object for dynamic offsets and element-strided storage.
            let physical_range = |addr: &RegionedAbsoluteAddr| {
                let start = i64::from(layout.regioned_static_byte_and_intra(addr, 0)?.0);
                let planes = if self.four_state
                    && layout
                        .is_4states
                        .get(&addr.absolute_addr())
                        .copied()
                        .unwrap_or(false)
                {
                    2
                } else {
                    1
                };
                let size = layout
                    .plane_size(&addr.absolute_addr())
                    .checked_mul(planes)?;
                Some((start, start.checked_add(i64::try_from(size).ok()?)?))
            };
            let written = physical_range(write.addr);
            self.reg_addrs
                .retain(|_, (addr, _)| match (physical_range(addr), written) {
                    (Some((start, end)), Some((write_start, write_end))) => {
                        end <= write_start || write_end <= start
                    }
                    _ => false,
                });
        }
        if let SIRInstruction::Load(dst, addr, offset, _) = inst {
            match offset {
                SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
                    self.reg_addrs.insert(*dst, (*addr, *bit_offset));
                }
                _ => {
                    self.reg_addrs.remove(dst);
                }
            }
        }
    }
}
