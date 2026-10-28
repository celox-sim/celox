use crate::lowering::native_tests::evaluate;
use crate::{ExprOpcode as TbOpcode, TestbenchOperator as Op};
use celox_design::{FloatBinaryOp, FloatPrecision, FloatUnaryOp};

#[test]
fn deep_expression_compiles_without_a_runtime_operand_stack() {
    for depth in [1, 8, 9, 32, 128, 129, 256] {
        let mut ops: Vec<_> = (1..=depth).map(TbOpcode::ConstU64).collect();
        ops.extend((1..depth).map(|_| TbOpcode::BinOp(Op::Add)));

        assert_eq!(
            evaluate(ops, &[]),
            num_bigint::BigUint::from(depth * (depth + 1) / 2)
        );
    }
}

#[test]
fn floating_bytecode_keeps_precision_and_numeric_conversions() {
    for (precision, expected) in [
        (FloatPrecision::F32, 16777216),
        (FloatPrecision::F64, 16777217),
    ] {
        let ops = vec![
            TbOpcode::ConstU64(16777216),
            TbOpcode::FloatUnary {
                op: FloatUnaryOp::FromUnsigned { width: 32 },
                precision,
            },
            TbOpcode::ConstU64(1),
            TbOpcode::FloatUnary {
                op: FloatUnaryOp::FromUnsigned { width: 32 },
                precision,
            },
            TbOpcode::FloatBinary {
                op: FloatBinaryOp::Add,
                precision,
            },
            TbOpcode::FloatUnary {
                op: FloatUnaryOp::ToUnsigned {
                    width: 32,
                    round: true,
                },
                precision,
            },
        ];
        assert_eq!(
            evaluate(ops, &[]),
            num_bigint::BigUint::from(expected as u64)
        );
    }
}
