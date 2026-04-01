//! Identity Store bypass: detect Store(B, identity_copy_from_A), remove the
//! Store, and register B as an alias of A in the memory layout.
//!
//! After aliasing, Load(B) reads from A's physical memory (correct because
//! the Store was writing A's exact value to B). The Store removal cascades
//! through DCE to eliminate the Concat chain that assembled the copy.
//!
//! Safety: only aliases addresses where the ONLY Store to B is the identity copy.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, sir_value_to_u64};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

pub(super) struct IdentityStoreBypassPass {
    /// Accumulated aliases: non-canonical → canonical address.
    /// Populated during `run`, read by the caller after all EUs are processed.
    pub aliases: std::cell::RefCell<HashMap<AbsoluteAddr, AbsoluteAddr>>,
}

impl IdentityStoreBypassPass {
    pub fn new() -> Self {
        Self {
            aliases: std::cell::RefCell::new(HashMap::default()),
        }
    }
}

impl ExecutionUnitPass for IdentityStoreBypassPass {
    fn name(&self) -> &'static str {
        "identity_store_bypass"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        // Build global def map
        let mut defs: HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>> =
            HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                if let Some(d) = def_reg(inst) {
                    defs.insert(d, inst.clone());
                }
            }
        }

        // Count Stores per absolute address to ensure single-writer
        let mut store_counts: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                if let SIRInstruction::Store(addr, _, _, _, _) = inst {
                    *store_counts.entry(addr.absolute_addr()).or_default() += 1;
                }
            }
        }

        // Find identity Stores: Store(B, 0, W, reg) where reg = identity_copy(A)
        // and B has exactly one Store
        let mut found_aliases: Vec<(RegionedAbsoluteAddr, RegionedAbsoluteAddr)> = Vec::new();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                let SIRInstruction::Store(addr_b, SIROffset::Static(0), width, src_reg, triggers) =
                    inst
                else {
                    continue;
                };
                if !triggers.is_empty() {
                    continue;
                }
                // Must be the only Store to this address
                if store_counts
                    .get(&addr_b.absolute_addr())
                    .copied()
                    .unwrap_or(0)
                    != 1
                {
                    continue;
                }
                if let Some(addr_a) = trace_identity_source(*src_reg, *width, &defs) {
                    if addr_a.absolute_addr() != addr_b.absolute_addr() {
                        found_aliases.push((*addr_b, addr_a));
                    }
                }
            }
        }

        if found_aliases.is_empty() {
            return;
        }

        // Register alias candidates (Store removal happens later after layout validation)
        let mut aliases = self.aliases.borrow_mut();
        for (addr_b, addr_a) in &found_aliases {
            aliases.insert(addr_b.absolute_addr(), addr_a.absolute_addr());
        }
    }
}

/// Trace a register to determine if it's an identity copy of some address.
/// Returns the source address if the value is a bit-for-bit copy.
fn trace_identity_source(
    reg: RegisterId,
    expected_width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegionedAbsoluteAddr> {
    let def = defs.get(&reg)?;

    match def {
        // Direct Load: Store(B, Load(A)) where widths match
        SIRInstruction::Load(_, addr, SIROffset::Static(0), width) if *width == expected_width => {
            Some(*addr)
        }

        // Concat of sequential 1-bit Loads from same address (MSB first)
        SIRInstruction::Concat(_, args) if args.len() == expected_width => {
            trace_concat_identity(args, expected_width, defs)
        }

        // Look through identity/cast
        SIRInstruction::Unary(_, UnaryOp::Ident, inner) => {
            trace_identity_source(*inner, expected_width, defs)
        }

        _ => None,
    }
}

/// Check if a Concat's args form an identity copy: each arg at position i
/// loads bit (W-1-i) from the same address.
fn trace_concat_identity(
    args: &[RegisterId],
    width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegionedAbsoluteAddr> {
    let mut source_addr: Option<RegionedAbsoluteAddr> = None;

    for (i, &arg) in args.iter().enumerate() {
        let expected_bit = width - 1 - i; // MSB first in Concat
        let arg_def = defs.get(&arg)?;

        let (addr, bit) = match arg_def {
            // Direct 1-bit Load
            SIRInstruction::Load(_, addr, SIROffset::Static(offset), 1) => (*addr, *offset),

            // Bit extract: (Load(A) >> K) & 1
            SIRInstruction::Binary(_, shifted, BinaryOp::And, mask_reg) => {
                let mask_def = defs.get(mask_reg)?;
                let SIRInstruction::Imm(_, mask_val) = mask_def else {
                    return None;
                };
                if sir_value_to_u64(mask_val)? != 1 {
                    return None;
                }

                match defs.get(shifted)? {
                    SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) => {
                        let SIRInstruction::Imm(_, sv) = defs.get(shift_reg)? else {
                            return None;
                        };
                        let shift = sir_value_to_u64(sv)? as usize;
                        // src must be a Load
                        let SIRInstruction::Load(_, addr, SIROffset::Static(0), _) =
                            defs.get(src)?
                        else {
                            return None;
                        };
                        (*addr, shift)
                    }
                    // No shift: bit 0
                    SIRInstruction::Load(_, addr, SIROffset::Static(0), _) => (*addr, 0),
                    _ => return None,
                }
            }

            _ => return None,
        };

        if bit != expected_bit {
            return None;
        }

        match &source_addr {
            Some(a) if a.absolute_addr() != addr.absolute_addr() => return None,
            None => source_addr = Some(addr),
            _ => {}
        }
    }

    source_addr
}
