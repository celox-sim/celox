//! Source-independent testbench bytecode contracts.
//!
//! Frontends translate source-language operators into this vocabulary before
//! execution. No Veryl AST, parser ID, simulator backend, or runtime handle is
//! part of the bytecode representation.

use num_bigint::BigUint;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TestbenchOperator {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    BitXnor,
    BitNand,
    BitNor,
    LogicShiftL,
    LogicShiftR,
    ArithShiftL,
    ArithShiftR,
    Eq,
    EqWildcard,
    Ne,
    NeWildcard,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    LogicAnd,
    LogicOr,
    LogicNot,
    BitNot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprOpcode {
    ConstU64(u64),
    ConstWide(BigUint),
    LoadU64 {
        offset: usize,
        byte_size: usize,
        mask: u64,
    },
    LoadWide {
        offset: usize,
        byte_size: usize,
        width: usize,
    },
    BinOp(TestbenchOperator),
    TypedBinOp {
        op: TestbenchOperator,
        lhs_width: usize,
        rhs_width: usize,
        result_width: usize,
        lhs_signed: bool,
        rhs_signed: bool,
    },
    TypedUnary {
        op: TestbenchOperator,
        operand_width: usize,
        result_width: usize,
    },
    Resize {
        source_width: usize,
        target_width: usize,
        signed: bool,
    },
    ConcatPart {
        part_width: usize,
        result_width: usize,
    },
    Ternary {
        then_len: usize,
        else_len: usize,
    },
    LoadIndexed {
        base_offset: usize,
        stride_bytes: usize,
        element_byte_size: usize,
        element_width: usize,
    },
    LoadBitSelect {
        base_offset: usize,
        base_byte_size: usize,
        select_width: usize,
    },
    StoreU64 {
        offset: usize,
        byte_size: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExprBytecode {
    ops: Vec<ExprOpcode>,
}

impl ExprBytecode {
    pub fn new(ops: Vec<ExprOpcode>) -> Self {
        Self { ops }
    }

    pub fn ops(&self) -> &[ExprOpcode] {
        &self.ops
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_uses_source_independent_operators() {
        let code = ExprBytecode::new(vec![
            ExprOpcode::ConstU64(1),
            ExprOpcode::ConstU64(2),
            ExprOpcode::TypedBinOp {
                op: TestbenchOperator::Add,
                lhs_width: 8,
                rhs_width: 8,
                result_width: 8,
                lhs_signed: false,
                rhs_signed: false,
            },
        ]);
        assert_eq!(code.ops().len(), 3);
        assert!(!code.is_empty());
    }
}
