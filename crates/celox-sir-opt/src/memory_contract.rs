use crate::ir::{
    AbsoluteAddr, BlockId, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset,
};

pub fn verify_memory_offset_contract(
    design: &celox_design::ElaboratedDesign<AbsoluteAddr>,
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<(), celox_sir::verify::SirVerifyError> {
    for block in eu.blocks.values() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let (address, offset, width, operation, explicit_memory_copy) = match instruction {
                SIRInstruction::Load(_, address, offset, width) => {
                    (address, offset, *width, "Load", false)
                }
                SIRInstruction::Store(address, offset, width, _, _, _) => {
                    (address, offset, *width, "Store", false)
                }
                SIRInstruction::Commit(source, destination, offset, width, _) => {
                    verify_address(
                        design,
                        block.id,
                        index,
                        destination,
                        offset,
                        *width,
                        "Commit destination",
                        true,
                    )?;
                    (source, offset, *width, "Commit source", true)
                }
                _ => continue,
            };
            verify_address(
                design,
                block.id,
                index,
                address,
                offset,
                width,
                operation,
                explicit_memory_copy,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_address(
    design: &celox_design::ElaboratedDesign<AbsoluteAddr>,
    block: BlockId,
    index: usize,
    address: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    operation: &'static str,
    explicit_memory_copy: bool,
) -> Result<(), celox_sir::verify::SirVerifyError> {
    let Some(info) = design.state_objects.get(&address.absolute_addr()) else {
        return Err(celox_sir::verify::SirVerifyError::instruction(
            "MEMORY.ADDRESS_HAS_DECLARATION",
            block,
            index,
            format!("no design state object for memory address {address:?}"),
        ));
    };
    let element_count = info
        .array_dims
        .iter()
        .try_fold(1usize, |count, &dimension| count.checked_mul(dimension));
    let declared_element_width = (!info.array_dims.is_empty())
        .then_some(element_count)
        .flatten()
        .filter(|&count| count != 0 && info.width % count == 0)
        .map(|count| info.width / count);

    match offset {
        SIROffset::Dynamic(_) if !info.array_dims.is_empty() => {
            return Err(celox_sir::verify::SirVerifyError::instruction(
                "MEMORY.UNPACKED_OFFSET_IS_ELEMENT",
                block,
                index,
                format!(
                    "{operation} addresses unpacked array {address} with dimensions {:?} by an arbitrary dynamic bit offset; preserve the element index as SIROffset::Element",
                    info.array_dims,
                ),
            ));
        }
        SIROffset::Element {
            bit_offset,
            element_width,
            ..
        } => {
            if info.array_dims.is_empty() {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.ELEMENT_OFFSET_REQUIRES_UNPACKED_ARRAY",
                    block,
                    index,
                    "SIROffset::Element used for a state object without unpacked dimensions",
                ));
            }
            let Some(declared_element_width) = declared_element_width else {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.UNPACKED_DECLARATION_HAS_ELEMENT_WIDTH",
                    block,
                    index,
                    format!(
                        "array dimensions {:?} do not divide declared width {}",
                        info.array_dims, info.width
                    ),
                ));
            };
            if *element_width != declared_element_width {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.ELEMENT_WIDTH_MATCHES_DECLARATION",
                    block,
                    index,
                    format!(
                        "SIR element width {element_width} does not match declared element width {declared_element_width}"
                    ),
                ));
            }
            if bit_offset
                .checked_add(width)
                .is_none_or(|end| end > declared_element_width)
            {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!(
                        "{operation} range [{bit_offset} +: {width}] exceeds unpacked element width {declared_element_width}"
                    ),
                ));
            }
        }
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => {
            let Some(declared_element_width) = declared_element_width else {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_REQUIRE_UNPACKED_ARRAY",
                    block,
                    index,
                    format!("{operation} uses packed-elements addressing on a non-array state"),
                ));
            };
            let valid = *element_width == declared_element_width
                && bit_offset.is_multiple_of(declared_element_width)
                && width.is_multiple_of(declared_element_width)
                && bit_offset
                    .checked_add(width)
                    .is_some_and(|end| end <= info.width);
            if !valid {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_MATCH_DECLARATION",
                    block,
                    index,
                    format!(
                        "{operation} packed-elements range [{bit_offset} +: {width}] with element width {element_width} does not match declared element width {declared_element_width} and total width {}",
                        info.width
                    ),
                ));
            }
        }
        SIROffset::Static(start)
            if !explicit_memory_copy
                && let Some(element_width) = declared_element_width
                && width != 0 =>
        {
            let Some(end) = start.checked_add(width) else {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!("{operation} range overflows usize"),
                ));
            };
            if *start / element_width != end.saturating_sub(1) / element_width {
                return Err(celox_sir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!(
                        "{operation} at {address:?} range [{start} +: {width}] crosses unpacked element width {element_width}; use an explicit array-copy operation for a multi-element transfer"
                    ),
                ));
            }
        }
        SIROffset::Static(_) | SIROffset::Dynamic(_) => {}
    }
    Ok(())
}
