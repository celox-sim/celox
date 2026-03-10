use crate::HashMap;
use crate::backend::MEM_SHIFT_THRESHOLD;
use crate::ir::*;

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

    match inst {
        SIRInstruction::Imm(dst, _) => {
            let width = reg_width(register_map, dst);
            num_chunks(width).max(1) * state_mul
        }
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            // The translator computes common_logical_width = max(dst, lhs, rhs)
            // and operates at that width for the computation.
            let d_w = reg_width(register_map, dst);
            let l_w = reg_width(register_map, lhs);
            let r_w = reg_width(register_map, rhs);
            let width = d_w.max(l_w).max(r_w);

            if width <= 64 {
                let base = match op {
                    BinaryOp::Add | BinaryOp::Sub => 5,
                    BinaryOp::Mul => 5,
                    BinaryOp::Div | BinaryOp::Rem => 10,
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
                    BinaryOp::Div | BinaryOp::Rem => 640 * nc * nc + 384 * nc,
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
        SIRInstruction::Unary(dst, op, src) => {
            // Unary also uses max(dst, src) as common width
            let d_w = reg_width(register_map, dst);
            let s_w = reg_width(register_map, src);
            let width = d_w.max(s_w);

            if width <= 64 {
                2 * state_mul
            } else {
                let nc = num_chunks(width);
                let base = match op {
                    UnaryOp::Minus => 5 * nc + 1,
                    UnaryOp::LogicNot => 2 * nc + 4,
                    _ => 2 * nc,
                };
                base * state_mul
            }
        }
        SIRInstruction::Load(_, _, offset, op_width) => {
            let nc = num_chunks(*op_width);
            let base = if *op_width <= 64 {
                3
            } else if matches!(offset, SIROffset::Dynamic(_)) {
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
        SIRInstruction::Store(_, offset, op_width, _, _) => {
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
    }
}

// ---------------------------------------------------------------------------
// CLIF Value estimation — tracks Value-producing instructions only.
//
// This is a separate metric from `estimate_clif_cost` (which counts total
// CLIF instructions). Each CLIF Value maps to ≈1 VReg during lowering;
// the x86-64 backend adds temps on top of that. Exceeding
// `regalloc2::VReg::MAX` (2^21 − 1) triggers `CodeTooLarge`.
//
// Calibrated against `backend/translator/memory.rs` and `arith.rs`:
//   - translate_load_native_aligned:  load + cast  → 2 Values
//   - translate_load_native:          load + cast + ushr + band_imm + cast → 5 Values
//   - translate_store_native_aligned: cast + store(0) → 1 Value
//   - translate_store_native (RMW):   iconst + cast×2 + load + ishl + bnot
//                                     + ishl + band×2 + bor + store(0) → 10 Values
//   - translate_load_multi_word_aligned_words: 1 load per chunk → nc Values
// ---------------------------------------------------------------------------

/// Estimate the number of CLIF Values a single SIR instruction will produce.
///
/// Unlike `estimate_clif_cost`, this counts only value-producing instructions
/// (excludes `store`, `jump`, `brif`, `return`).
#[cfg(test)]
fn estimate_value_count(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> usize {
    let state_mul = if four_state { 2 } else { 1 };

    match inst {
        // iconst per chunk
        SIRInstruction::Imm(dst, _) => {
            let width = reg_width(register_map, dst);
            num_chunks(width).max(1) * state_mul
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let d_w = reg_width(register_map, dst);
            let l_w = reg_width(register_map, lhs);
            let r_w = reg_width(register_map, rhs);
            let width = d_w.max(l_w).max(r_w);

            if width <= 64 {
                // Simple ops: maybe extend + op + maybe reduce → 1–3 Values
                // Comparisons: extend + cmp + bint → 3 Values
                // Div/Rem: helper calls with setup → 5 Values
                let base = match op {
                    BinaryOp::Div | BinaryOp::Rem => 5,
                    BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS => 3,
                    _ => 3,
                };
                base * state_mul
            } else {
                let nc = num_chunks(width);
                let base = match op {
                    // Bitwise: 1 Value per chunk (result only)
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => nc,
                    // Add/Sub: carry chain values, ~4 per chunk
                    BinaryOp::Add | BinaryOp::Sub => 4 * nc,
                    // Shifts: select chains (values) vs memory-backed loads
                    BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                        if nc >= MEM_SHIFT_THRESHOLD {
                            6 * nc + 10
                        } else {
                            // select chain: each select is a Value
                            4 * nc * nc + 5 * nc
                        }
                    }
                    BinaryOp::Mul => 5 * nc * nc + 3 * nc,
                    BinaryOp::Div | BinaryOp::Rem => 400 * nc * nc + 200 * nc,
                    // Comparisons: result is 1-chunk, but per-chunk compares produce Values
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

        SIRInstruction::Unary(dst, op, src) => {
            let d_w = reg_width(register_map, dst);
            let s_w = reg_width(register_map, src);
            let width = d_w.max(s_w);

            if width <= 64 {
                2 * state_mul
            } else {
                let nc = num_chunks(width);
                let base = match op {
                    UnaryOp::Minus => 4 * nc,
                    UnaryOp::LogicNot => 2 * nc + 3,
                    _ => nc,
                };
                base * state_mul
            }
        }

        // Load: addr computation (iadd_imm) + load per chunk + maybe casts
        SIRInstruction::Load(_, _, offset, op_width) => {
            let nc = num_chunks(*op_width);
            let base = if *op_width <= 64 {
                // translate_load_native_aligned: load + cast → 2
                // translate_load_native: load + cast + ushr + band_imm + cast → 5
                if matches!(offset, SIROffset::Static(v) if v & 7 == 0)
                    && matches!(*op_width, 8 | 16 | 32 | 64)
                {
                    // iadd_imm(1) + load_native_aligned(2) = 3
                    3
                } else {
                    // offset computation(2) + iadd_imm(1) + iadd(1) + load_native(5) = 9
                    9
                }
            } else if matches!(offset, SIROffset::Static(v) if v & 7 == 0)
                && op_width.is_multiple_of(64)
            {
                // iadd_imm(1) + nc loads
                nc + 1
            } else {
                // offset computation + addr + per-chunk slide-combine
                6 * nc + 5
            };
            base * state_mul
        }

        // Store: address computation + masking Values (store instruction itself produces 0)
        SIRInstruction::Store(_, offset, op_width, _, _) => {
            let nc = num_chunks(*op_width);
            let base = if *op_width <= 64 {
                if matches!(offset, SIROffset::Static(v) if v & 7 == 0)
                    && matches!(*op_width, 8 | 16 | 32 | 64)
                {
                    // iconst(1) + iconst(1) + iadd_imm(1) + iadd(1) + cast(1) = 5
                    // (store itself = 0)
                    5
                } else {
                    // offset(2) + addr(2) + translate_store_native RMW(10) = 14
                    14
                }
            } else if matches!(offset, SIROffset::Static(_)) && op_width.is_multiple_of(64) {
                // addr(2) + per-chunk: nothing value-producing (just stores)
                // But offset computation iconst+iconst+iadd_imm+iadd = 4, then nc stores (0 each)
                4
            } else {
                // RMW per chunk: load + mask + shift + combine = ~8 values/chunk + setup
                8 * nc + 5
            };
            base * state_mul
        }

        // Commit: load (src) + store (dst), addr computation for both
        SIRInstruction::Commit(_, _, offset, op_width, _) => {
            let nc = num_chunks(*op_width);
            // Fast path (byte-aligned static): 2 iadd_imms + copy_bytes (nc loads + nc stores)
            // → 2 + nc Values (loads produce values, stores don't)
            let load_values = if *op_width <= 64 {
                3
            } else if op_width.is_multiple_of(64) {
                nc + 1
            } else {
                6 * nc + 5
            };
            let store_values = if *op_width <= 64 {
                if matches!(offset, SIROffset::Static(v) if v & 7 == 0)
                    && op_width.is_multiple_of(8)
                {
                    2
                } else {
                    10
                }
            } else if matches!(offset, SIROffset::Static(_)) && op_width.is_multiple_of(64) {
                2
            } else {
                8 * nc + 5
            };
            (load_values + store_values + 2) * state_mul
        }

        // Per arg: shift + or → 2 Values, plus setup
        SIRInstruction::Concat(_, args) => 3 * args.len() * state_mul,
    }
}

/// Estimate the total CLIF cost for an entire execution unit.
pub fn estimate_eu_cost(eu: &ExecutionUnit<RegionedAbsoluteAddr>, four_state: bool) -> usize {
    let state_mul = if four_state { 2 } else { 1 };
    let mut cost = 0usize;
    for block in eu.blocks.values() {
        // Block params
        cost += block.params.len() * state_mul;
        // Instructions
        for inst in &block.instructions {
            cost += estimate_clif_cost(inst, &eu.register_map, four_state);
        }
        // Terminator
        cost += match &block.terminator {
            SIRTerminator::Jump(_, _) => 1,
            SIRTerminator::Branch { .. } => 2,
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
    fn test_threshold_constants() {
        const _: () = assert!(CLIF_INST_THRESHOLD < 16_000_000);
        const _: () = assert!(CLIF_INST_THRESHOLD > 4_000_000);
        // VReg::MAX = (1 << 21) - 1 = 2_097_151
        const _: () = assert!(VREG_VALUE_THRESHOLD < 2_097_151);
        const _: () = assert!(VREG_VALUE_THRESHOLD > 500_000);
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
    fn test_value_count_less_than_or_equal_to_inst_cost() {
        // For most instruction types, value count ≤ instruction count
        // (stores are the exception: RMW stores produce more values than
        // the inst cost model estimates, but that's intentional — the inst
        // cost was underestimating those).
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(2),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );

        // Imm: values == insts
        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u64));
        let values = estimate_value_count(&inst, &register_map, false);
        let insts = estimate_clif_cost(&inst, &register_map, false);
        assert!(values <= insts + 1, "Imm: values={values} insts={insts}");

        // Binary Add
        let inst: SIRInstruction<RegionedAbsoluteAddr> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Add, RegisterId(2));
        let values = estimate_value_count(&inst, &register_map, false);
        let insts = estimate_clif_cost(&inst, &register_map, false);
        assert!(values <= insts + 1, "Add: values={values} insts={insts}");
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
