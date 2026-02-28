# 4-State テスト計画

`tests/four_state.rs` のカバレッジ状況と追加テストの計画です。
各項目の実装時にチェックを入れてください。

## 凡例

- [x] テスト済み
- [ ] 未テスト（実装予定）

---

## 1. ビット演算 (Binary Bitwise)

| 演算 | 単一幅 (≤64bit) | ワイド (>64bit) |
|------|-----------------|----------------|
| AND  | [x] `test_four_state_and_or` | [x] `test_four_state_wide_128bit` |
| OR   | [x] `test_four_state_and_or` | [x] `test_four_state_wide_128bit` |
| XOR  | [x] `test_four_state_xor_partial_x` | [x] `test_four_state_wide_128bit` (暗黙) |

## 2. 算術演算 (Arithmetic)

| 演算 | 単一幅 | ワイド | 備考 |
|------|--------|--------|------|
| ADD  | [x] `test_four_state_arithmetic_ops` | [x] `test_four_state_wide_arith` | |
| SUB  | [x] `test_four_state_arithmetic_ops` | [x] `test_four_state_wide_arith` | |
| MUL  | [x] `test_four_state_mul_with_x` | [x] `test_four_state_wide_mul_with_x` | 保守的 all-X |
| DIV  | [x] `test_four_state_div_with_x` | [x] `test_four_state_wide_div_with_x` | 保守的 all-X |
| MOD  | [x] `test_four_state_mod_with_x` | [x] `test_four_state_wide_mod_with_x` | 保守的 all-X |

## 3. シフト演算 (Shift)

| 条件 | SHL | SHR | SAR |
|------|-----|-----|-----|
| 定数シフト量 | [x] `test_four_state_shift_by_constant` | [x] 同左 | [x] `test_four_state_wide_signed` |
| 変数シフト量 (確定) | [x] `test_four_state_wide_shifts` | [x] 同左 | [x] `test_four_state_sar_x_shift_amount` (確定ケース) |
| シフト量に X | [x] `test_four_state_shift_by_x_amount` | [x] `test_four_state_wide_shifts` | [x] `test_four_state_sar_x_shift_amount` |
| データと量の両方に X | [x] `test_four_state_shift_both_x` | [x] 同左 | [x] `test_four_state_sar_both_x` |

## 4. 比較演算 (Comparison)

| 演算 | 単一幅 | ワイド | 備考 |
|------|--------|--------|------|
| EQ (`==`) | [x] `test_four_state_comparison_with_x` | [x] `test_four_state_wide_comparison_with_x` | |
| NE (`!=`) | [x] `test_four_state_ne_with_x` | [x] `test_four_state_wide_ne_with_x` | |
| LT (`<` unsigned) | [x] `test_four_state_comparison_with_x` | [x] `test_four_state_wide_comparison_with_x` | |
| GT (`>` unsigned) | [x] `test_four_state_gt_with_x` | [x] `test_four_state_wide_gt_with_x` | |
| LE (`<=` unsigned) | [x] `test_four_state_ge_le_with_x` | [x] `test_four_state_wide_ge_le_with_x` | |
| GE (`>=` unsigned) | [x] `test_four_state_ge_le_with_x` | [x] `test_four_state_wide_ge_le_with_x` | |
| LT (`<` signed) | [x] `test_four_state_signed_comparison_with_x` | [x] `test_four_state_wide_signed_comparison_with_x` | `signed logic<128>` |
| GT (`>` signed) | [x] `test_four_state_signed_comparison_with_x` | [x] `test_four_state_wide_signed_comparison_with_x` | |
| LE (`<=` signed) | [x] `test_four_state_signed_comparison_with_x` | [x] `test_four_state_wide_signed_comparison_with_x` | |
| GE (`>=` signed) | [x] `test_four_state_signed_comparison_with_x` | [x] `test_four_state_wide_signed_comparison_with_x` | |

## 5. 単項演算 (Unary)

| 演算 | 単一幅 | ワイド |
|------|--------|--------|
| Bitwise NOT (`~`) | [x] `test_four_state_unary_ops` | [x] `test_four_state_wide_unary_not_with_x` |
| Negation (`-`) | [x] `test_four_state_negation_with_x` | [x] `test_four_state_wide_negation_with_x` |
| Logical NOT (`!`) | [x] `test_four_state_logical_not_with_x` | [x] `test_four_state_wide_logical_not_with_x` |
| Reduction AND | [x] `test_four_state_unary_ops` (部分) | [x] `test_four_state_wide_reduction_with_x` |
| Reduction OR | [x] `test_four_state_unary_ops` (部分) | [x] 同上 (※保守的実装) |
| Reduction XOR | [x] `test_four_state_reduction_xor_with_x` | [x] 同上 |

## 6. 連結 (Concatenation)

| パターン | テスト状況 |
|----------|-----------|
| 2要素 (同幅) | [x] `test_four_state_concat` |
| 2要素 (ワイド混合) | [x] `test_four_state_wide_concat_mixed` |
| 3要素以上 | [x] `test_four_state_concat_three_elements` |
| 奇数幅 (例: 3bit + 5bit) | [x] `test_four_state_concat_odd_width` |
| チャンク境界をまたぐ X | [x] `test_four_state_concat_chunk_boundary_x` |

## 7. Mux / 三項演算子

| パターン | テスト状況 |
|----------|-----------|
| X 条件 (1bit セレクタ) | [x] `test_four_state_mux_x_condition` |
| 確定条件, X 分岐 | [x] `test_four_state_mux_x_in_branch` |
| マルチビットセレクタ | [x] `test_four_state_multibit_mux_with_x` |
| カスケード Mux | [x] `test_four_state_cascaded_mux_with_x` |
| 両分岐とも X | [x] `test_four_state_mux_both_branches_x` |

## 8. FF (フリップフロップ)

| パターン | テスト状況 |
|----------|-----------|
| X キャプチャ | [x] `test_four_state_ff_capture_and_reset` |
| リセットで X クリア | [x] 同上 |
| 同期リセット + X | [x] `test_four_state_ff_sync_reset_with_x` |
| FF 内条件分岐 + X | [x] `test_four_state_ff_conditional_with_x` |

## 9. 型変換・代入

| パターン | テスト状況 |
|----------|-----------|
| logic → bit (X ドロップ) | [x] `test_four_state_mixing` |
| bit → logic (mask=0 維持) | [x] `test_four_state_mixing` |
| 狭幅 → 広幅 + X | [x] `test_four_state_width_widening_with_x` |
| 広幅 → 狭幅 + X | [x] `test_four_state_width_narrowing_with_x` |
| 明示的キャスト + X | [x] `test_four_state_explicit_cast_with_x` |

## 10. 境界幅

| 幅 | テスト状況 | 備考 |
|----|-----------|------|
| 1bit | [x] 比較演算結果で暗黙カバー | |
| 8bit | [x] 複数テスト | |
| 64bit | [x] `test_four_state_wide_128bit_simple` | 1チャンク上限 |
| 65bit | [x] `test_four_state_65bit_boundary` | 2チャンク下限 |
| 127bit | [x] `test_four_state_127bit` | |
| 128bit | [x] 複数ワイドテスト | |

## 11. 正規化 (IEEE 1800)

| パターン | テスト状況 |
|----------|-----------|
| 演算結果の v &= ~m | [x] `test_four_state_wide_shifts`, `test_four_state_wide_concat_mixed` |
| 全ビット X 入力 | [x] `test_four_state_initial_and_set` |
| 2-state 変数経由の X 遮断 | [x] `test_four_state_mixing_propagation` |

---

## 優先度ガイド

### P0 — コアセマンティクスの検証
- [x] MUL / DIV / MOD + X （単一幅）
- [x] 比較演算の網羅 (NE, GT, GE, LE + 符号付き)
- [x] Reduction XOR + X
- [x] 65bit 幅テスト

### P1 — よくある HDL パターン
- [x] Negation (`-`) + X
- [x] Logical NOT (`!`) + X
- [x] SAR + X シフト量
- [x] 3要素以上の連結
- [x] ワイド比較 + X

### P2 — エッジケースの堅牢性
- [x] マルチビットセレクタ Mux
- [x] 広幅 ↔ 狭幅の代入 + X
- [x] FF 内条件分岐 + X
- [x] 127bit / 奇数幅の連結

## 修正済みの問題

- **`case` 文 + 4-state** (修正済み): `Op::EqWildcard` が `BinaryOp::Eq` にマッピングされていたため、4-state の保守的マスク計算で比較結果が X 扱いになっていた。`BinaryOp::EqWildcard` / `BinaryOp::NeWildcard` を新設し、IEEE 1800 のワイルドカードセマンティクスを実装。テスト: `test_four_state_case_defined_selector`, `test_four_state_case_x_in_selector`。
- **Reduction OR/AND の保守的実装** (修正済み): IEEE 1800 の dominant-value セマンティクスを実装。`|a` は確定 1 ビットが存在すれば結果は確定 1、`&a` は確定 0 ビットが存在すれば結果は確定 0。テスト: `test_four_state_reduction_or_dominant_one`, `test_four_state_reduction_and_dominant_zero`, `test_four_state_wide_reduction_or_dominant`, `test_four_state_wide_reduction_and_dominant`。
