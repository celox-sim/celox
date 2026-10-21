use crate::HashMap;
use crate::MEM_SHIFT_THRESHOLD;
use crate::*;

/// Safety margin: 50% of Cranelift's ~16M instruction index limit.
pub const CLIF_INST_THRESHOLD: usize = 8_000_000;

/// The binding VReg constraint in Cranelift (via regalloc2) is
/// `VReg::MAX = (1 << 21) - 1 = 2_097_151`.
///
/// During lowering each CLIF Value maps to 1–2 VRegs, and the x86-64
/// backend allocates extra temporaries.  Empirically the CLIF instruction
/// count is a good upper-bound proxy for the Value count (actual ratio
/// values/insts ≈ 0.89).  We use the instruction estimate as the Value
/// estimate and set the threshold so that `inst_count * ~1.7` (the
/// worst-case VReg multiplier observed) stays below VReg::MAX.
///
///   VReg::MAX / 1.7 ≈ 1_233_000  →  rounded down to 1_000_000.
pub const VREG_VALUE_THRESHOLD: usize = 1_000_000;

/// Keep generated spill frames below the stack available on ordinary host
/// threads. The single-pass allocator gives branch arguments and parameters
/// distinct eight-byte spill slots, even when their live ranges do not overlap.
/// Leave headroom for lowering temporaries and ABI bookkeeping; compilation
/// also checks the actual frame size before exposing executable code.
pub const FUNCTION_VALUE_THRESHOLD: usize = 64 * 1024;

/// Maximum generated frame, including spills and outgoing call arguments.
pub const STACK_FRAME_LIMIT: u32 = 1024 * 1024;

pub(crate) fn block_param_cost(
    params: &[RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> usize {
    let planes = if four_state { 2 } else { 1 };
    params
        .iter()
        .map(|reg| num_chunks(reg_width(register_map, reg)) * planes)
        .sum()
}

fn num_chunks(width: usize) -> usize {
    width.div_ceil(64).max(1)
}

fn reg_width(register_map: &HashMap<RegisterId, RegisterType>, reg: &RegisterId) -> usize {
    register_map.get(reg).map(|r| r.width()).unwrap_or(64)
}

/// Estimate the number of CLIF instructions a single SIR instruction will produce.
///
/// These costs are calibrated against the actual translator implementation in
/// `backend/translator/` and `backend/wide_ops.rs`.
///
/// IMPORTANT: For Binary/Unary operations, the translator uses
/// `common_logical_width = max(dst, lhs, rhs)`, NOT just the destination width.
/// A 1-bit comparison result of two 4096-bit operands still requires 4096-bit
/// computation internally.
pub fn estimate_clif_cost(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> usize {
    let state_mul = if four_state { 2 } else { 1 };

    // Scalar FP operations use translate_float's integer-bit helper ABI:
    // operand normalization, constants, one indirect call and result packing.
    // Include headroom for mask normalization and narrow-value extension.
    const FLOAT_HELPER_COST: usize = 32;

    match inst {
        SIRInstruction::Imm(dst, _) => {
            let width = reg_width(register_map, dst);
            num_chunks(width).max(1) * state_mul
        }
        SIRInstruction::Binary(_, _, BinaryOp::Float { .. }, _)
        | SIRInstruction::Unary(_, UnaryOp::Float { .. }, _) => FLOAT_HELPER_COST,
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            // The translator computes common_logical_width = max(dst, lhs, rhs)
            // and operates at that width for the computation.
            let d_w = reg_width(register_map, dst);
            let l_w = reg_width(register_map, lhs);
            let r_w = reg_width(register_map, rhs);
            let width = d_w.max(l_w).max(r_w);

            if matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor) {
                // Payload, precise unknown-mask expression, and X
                // normalization are separate operations. And/Or need five
                // mask operations per chunk, rather than a second copy of
                // the payload operation. Include promotion/padding overhead.
                let per_chunk = if !four_state {
                    3
                } else if matches!(op, BinaryOp::Xor) {
                    5
                } else {
                    10
                };
                return per_chunk * num_chunks(width) + 12;
            }

            if width <= 64 {
                let base = match op {
                    BinaryOp::Add | BinaryOp::Sub => 5,
                    BinaryOp::Mul => 5,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => 10,
                    BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS => 4,
                    _ => 3,
                };
                base * state_mul
            } else {
                let nc = num_chunks(width);
                let base = match op {
                    // Bitwise: 1 CLIF per chunk (band/bor/bxor)
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => nc,
                    // Add/Sub: carry chain, ~5 per chunk
                    BinaryOp::Add | BinaryOp::Sub => 5 * nc,
                    // Shl/Shr/Sar: memory-backed O(n) above threshold, else O(n²)
                    BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                        if nc >= MEM_SHIFT_THRESHOLD {
                            // Memory-backed: load_or_default ~6 insts, combine ~4, per chunk
                            10 * nc + 20
                        } else {
                            // Register-based: select-chain O(n²)
                            5 * nc * nc + 7 * nc + 5
                        }
                    }
                    // Mul: schoolbook O(n²), ~5*nc² + 5*nc
                    BinaryOp::Mul => 5 * nc * nc + 5 * nc,
                    // Div/Rem: trial division O(n²), ~640*nc² + 384*nc
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                        640 * nc * nc + 384 * nc
                    }
                    // Comparisons: ~3 per chunk
                    BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS => 3 * nc,
                    _ => nc,
                };
                base * state_mul
            }
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            // The condition is reduced to known-one/unknown predicates. An
            // unknown condition then merges both value and mask planes, using
            // nine operations per output chunk in addition to that reduction.
            let d_w = reg_width(register_map, dst);
            let c_w = reg_width(register_map, cond);
            let t_w = reg_width(register_map, then_val);
            let e_w = reg_width(register_map, else_val);
            let width = d_w.max(c_w).max(t_w).max(e_w);
            let output_chunks = num_chunks(width);
            12 * num_chunks(c_w) + (if four_state { 12 } else { 4 }) * output_chunks + 18
        }
        SIRInstruction::Unary(dst, op, src) => {
            // Unary also uses max(dst, src) as common width
            let d_w = reg_width(register_map, dst);
            let s_w = reg_width(register_map, src);
            let width = d_w.max(s_w);

            if matches!(
                op,
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros
            ) {
                // emit_wide_bit_count scans each source chunk. CLZ adds a
                // conditional contribution and zero predicate; CTZ also
                // selects the logical chunk width when a chunk is zero.
                let source_chunks = num_chunks(s_w);
                let payload = match op {
                    UnaryOp::PopCount => 2 * source_chunks + 12,
                    UnaryOp::CountLeadingZeros => 6 * source_chunks + 12,
                    UnaryOp::CountTrailingZeros => 8 * source_chunks + 12,
                    _ => unreachable!(),
                };
                return payload
                    + if four_state {
                        // Scan masks, construct each destination mask, and
                        // normalize its payload to X, plus setup/padding.
                        3 * source_chunks + 4 * num_chunks(d_w) + 8
                    } else {
                        0
                    };
            }

            if four_state {
                let nc = num_chunks(width);
                // Reduction masks perform a second, dominant-value scan of
                // the source; merely doubling the payload cost misses it.
                return match op {
                    UnaryOp::Float { .. } => FLOAT_HELPER_COST,
                    UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::And => 10 * nc + 32,
                    UnaryOp::Xor => 8 * nc + 20,
                    UnaryOp::Minus => 12 * nc + 16,
                    UnaryOp::Ident | UnaryOp::BitNot | UnaryOp::ToTwoState => 4 * nc + 12,
                    UnaryOp::PopCount => 8 * nc + 20,
                    UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => 10 * nc + 20,
                };
            }

            if width <= 64 {
                // Logical-to-physical promotions and the final width mask
                // add constants and operations even for one-bit values.
                12
            } else {
                let nc = num_chunks(width);
                let base = match op {
                    UnaryOp::Minus => 5 * nc + 1,
                    UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::And => 3 * nc + 12,
                    // popcnt + parity xor per source chunk; the wide helper
                    // also constructs zero padding before truncation.
                    UnaryOp::Xor => 4 * nc + 4,
                    UnaryOp::PopCount => 3 * nc + 12,
                    UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => 4 * nc + 12,
                    _ => 2 * nc,
                };
                base * state_mul
            }
        }
        SIRInstruction::Load(_, _, offset, op_width) => {
            let nc = num_chunks(*op_width);
            let base = if *op_width <= 64 {
                3
            } else if offset.is_dynamic() {
                // Dynamic offset: unaligned access, ~9 per chunk + 3 setup
                9 * nc + 3
            } else if op_width.is_multiple_of(64) {
                // Static word-aligned: fast path, ~1 per chunk
                nc
            } else {
                // Static but not word-aligned: uses slide-combine, ~7 per chunk + 5 setup
                7 * nc + 5
            };
            base * state_mul
        }
        SIRInstruction::Store(_, offset, op_width, _, _, _) => {
            let nc = num_chunks(*op_width);
            let base = if *op_width <= 64 {
                6
            } else if matches!(offset, SIROffset::Static(_)) && op_width.is_multiple_of(64) {
                // Aligned static word-multiple: ~2 per chunk
                2 * nc
            } else if matches!(offset, SIROffset::Static(_)) {
                // Static but not word-aligned: still uses RMW-like path
                8 * nc + 5
            } else {
                // Dynamic/unaligned: RMW per chunk, ~22 per chunk
                22 * nc
            };
            base * state_mul
        }
        SIRInstruction::Commit(_, _, offset, op_width, _) => {
            let nc = num_chunks(*op_width);
            let load_cost = if *op_width <= 64 {
                3
            } else if op_width.is_multiple_of(64) {
                nc
            } else {
                7 * nc + 5
            };
            let store_cost = if *op_width <= 64 {
                6
            } else if matches!(offset, SIROffset::Static(_)) && op_width.is_multiple_of(64) {
                2 * nc
            } else if matches!(offset, SIROffset::Static(_)) {
                8 * nc + 5
            } else {
                22 * nc
            };
            (load_cost + store_cost + 3) * state_mul
        }
        SIRInstruction::Concat(_, args) => 3 * args.len() * state_mul,
        SIRInstruction::Slice(_, _, _, _) => 3 * state_mul,
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => 12 + args.len() * 2,
        SIRInstruction::CombCaptureEnableIfChanged { sites, .. } => 4 + sites.len() * 2,
        // A call saves the caller's live values and moves each argument into
        // its ABI register.
        SIRInstruction::ExternCall { args, .. } => 20 + args.len() * 2,
    }
}

/// Estimate the total CLIF cost for an entire execution unit.
pub fn estimate_eu_cost(eu: &ExecutionUnit<RegionedAbsoluteAddr>, four_state: bool) -> usize {
    let mut cost = 0usize;
    for block in eu.blocks.values() {
        // Block params
        cost += block_param_cost(&block.params, &eu.register_map, four_state);
        // Instructions
        for inst in &block.instructions {
            cost += estimate_clif_cost(inst, &eu.register_map, four_state);
        }
        // Terminator
        cost += match &block.terminator {
            SIRTerminator::Jump(_, _) => 1,
            SIRTerminator::Branch { .. } => 2,
            SIRTerminator::Switch { .. } => 2,
            SIRTerminator::Return => 2,
            SIRTerminator::Error(_) => 2,
        };
    }
    cost
}

/// Estimate the CLIF Value count for an execution unit.
///
/// Uses the CLIF instruction estimate as an upper-bound proxy: empirically
/// `values ≈ 0.89 × insts`, so the instruction count is a conservative but
/// well-calibrated estimate.  This avoids maintaining a separate (and
/// error-prone) per-instruction value estimation.
pub fn estimate_eu_value_count(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) -> usize {
    // Delegate to the instruction cost estimator — it already accounts for
    // block params, terminators, and per-instruction CLIF expansion.
    estimate_eu_cost(eu, four_state)
}

/// Estimate the total CLIF cost for a slice of execution units.
pub fn estimate_units_cost(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
) -> usize {
    units
        .iter()
        .map(|eu| estimate_eu_cost(eu, four_state))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_float_helper_costs_cover_both_precisions_and_state_modes() {
        use celox_design::{FloatBinaryOp, FloatPrecision, FloatUnaryOp};
        for precision in [FloatPrecision::F32, FloatPrecision::F64] {
            let register_map = (0..3)
                .map(|register| {
                    (
                        RegisterId(register),
                        RegisterType::Bit {
                            width: precision.width(),
                            signed: false,
                        },
                    )
                })
                .collect();
            let binary = SIRInstruction::Binary(
                RegisterId(0),
                RegisterId(1),
                BinaryOp::Float {
                    op: FloatBinaryOp::Add,
                    precision,
                },
                RegisterId(2),
            );
            for op in [
                FloatUnaryOp::Neg,
                FloatUnaryOp::FromSigned { width: 32 },
                FloatUnaryOp::ToSigned {
                    width: 32,
                    round: true,
                },
            ] {
                let unary = SIRInstruction::Unary(
                    RegisterId(0),
                    UnaryOp::Float { op, precision },
                    RegisterId(1),
                );
                for four_state in [false, true] {
                    assert_eq!(estimate_clif_cost(&unary, &register_map, four_state), 32);
                    assert_eq!(estimate_clif_cost(&binary, &register_map, four_state), 32);
                }
            }
        }
    }

    #[test]
    fn test_threshold_constants() {
        const _: () = assert!(CLIF_INST_THRESHOLD < 16_000_000);
        const _: () = assert!(CLIF_INST_THRESHOLD > 4_000_000);
        // VReg::MAX = (1 << 21) - 1 = 2_097_151
        const _: () = assert!(VREG_VALUE_THRESHOLD < 2_097_151);
        const _: () = assert!(VREG_VALUE_THRESHOLD > 500_000);
    }

    #[test]
    fn wide_block_parameters_count_each_machine_value_and_plane() {
        let register_map = [
            (RegisterId(0), RegisterType::Logic { width: 512 }),
            (RegisterId(1), RegisterType::Logic { width: 65 }),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            block_param_cost(&[RegisterId(0), RegisterId(1)], &register_map, false),
            10
        );
        assert_eq!(
            block_param_cost(&[RegisterId(0), RegisterId(1)], &register_map, true),
            20
        );
    }

    #[test]
    fn test_estimate_imm_cost() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );

        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u64));
        let cost = estimate_clif_cost(&inst, &register_map, false);
        assert!(cost >= 1);

        let cost_4s = estimate_clif_cost(&inst, &register_map, true);
        assert!(cost_4s >= cost);
    }

    #[test]
    fn test_shift_linear_cost_above_threshold() {
        let mut register_map = HashMap::default();
        // 4096-bit register → 64 chunks (above MEM_SHIFT_THRESHOLD)
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 4096,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 4096,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(2),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );

        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Shl, RegisterId(2));
        let cost = estimate_clif_cost(&inst, &register_map, false);
        // Memory-backed: 10*64 + 20 = 660 (linear, not quadratic)
        assert!(
            cost < 1_000,
            "Shift cost for 4096-bit should be linear (<1K), got {cost}"
        );
        assert!(
            cost > 500,
            "Shift cost for 4096-bit should be >500, got {cost}"
        );
    }

    #[test]
    fn test_shift_quadratic_cost_below_threshold() {
        let mut register_map = HashMap::default();
        // 128-bit register → 2 chunks (below MEM_SHIFT_THRESHOLD)
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 128,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 128,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(2),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );

        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Shl, RegisterId(2));
        let cost = estimate_clif_cost(&inst, &register_map, false);
        // Register-based: 5*2² + 7*2 + 5 = 20 + 14 + 5 = 39
        assert!(
            cost > 30,
            "Shift cost for 128-bit should be >30, got {cost}"
        );
    }

    #[test]
    fn test_comparison_uses_operand_width() {
        let mut register_map = HashMap::default();
        // Comparison: 1-bit result, but 4096-bit operands
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 4096,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(2),
            RegisterType::Bit {
                width: 4096,
                signed: false,
            },
        );

        // Shr with common_logical_width = max(1, 4096, 4096) = 4096
        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Shr, RegisterId(2));
        let cost = estimate_clif_cost(&inst, &register_map, false);
        // Memory-backed linear cost, but should still be non-trivial
        assert!(
            cost > 100,
            "Shr with 4096-bit operands should be >100, got {cost}"
        );
    }
}
