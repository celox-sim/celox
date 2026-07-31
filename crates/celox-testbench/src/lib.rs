//! Source-independent testbench bytecode contracts.
//!
//! Frontends translate source-language operators into this vocabulary before
//! execution. No Veryl AST, parser ID, simulator backend, or runtime handle is
//! part of the bytecode representation.

use num_bigint::BigUint;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateLocation<A> {
    pub address: A,
    pub byte_offset: usize,
}

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
pub enum ExprOpcode<L = usize> {
    ConstU64(u64),
    ConstWide(BigUint),
    LoadU64 {
        location: L,
        byte_size: usize,
        mask: u64,
    },
    LoadWide {
        location: L,
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
        location: L,
        stride_bytes: usize,
        element_byte_size: usize,
        element_width: usize,
    },
    LoadBitSelect {
        location: L,
        base_byte_size: usize,
        select_width: usize,
    },
    StoreU64 {
        location: L,
        byte_size: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExprBytecode<L = usize> {
    ops: Vec<ExprOpcode<L>>,
}

impl<L> ExprBytecode<L> {
    pub fn new(ops: Vec<ExprOpcode<L>>) -> Self {
        Self { ops }
    }

    pub fn ops(&self) -> &[ExprOpcode<L>] {
        &self.ops
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindError<A> {
    pub address: A,
}

impl<A> ExprBytecode<StateLocation<A>> {
    pub fn bind_with(
        self,
        mut resolve: impl FnMut(&A) -> Option<usize>,
    ) -> Result<ExprBytecode, BindError<A>> {
        let mut ops = Vec::with_capacity(self.ops.len());
        for op in self.ops {
            let bound = match op {
                ExprOpcode::ConstU64(value) => ExprOpcode::ConstU64(value),
                ExprOpcode::ConstWide(value) => ExprOpcode::ConstWide(value),
                ExprOpcode::LoadU64 {
                    location,
                    byte_size,
                    mask,
                } => ExprOpcode::LoadU64 {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                    mask,
                },
                ExprOpcode::LoadWide {
                    location,
                    byte_size,
                    width,
                } => ExprOpcode::LoadWide {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                    width,
                },
                ExprOpcode::BinOp(op) => ExprOpcode::BinOp(op),
                ExprOpcode::TypedBinOp {
                    op,
                    lhs_width,
                    rhs_width,
                    result_width,
                    lhs_signed,
                    rhs_signed,
                } => ExprOpcode::TypedBinOp {
                    op,
                    lhs_width,
                    rhs_width,
                    result_width,
                    lhs_signed,
                    rhs_signed,
                },
                ExprOpcode::TypedUnary {
                    op,
                    operand_width,
                    result_width,
                } => ExprOpcode::TypedUnary {
                    op,
                    operand_width,
                    result_width,
                },
                ExprOpcode::Resize {
                    source_width,
                    target_width,
                    signed,
                } => ExprOpcode::Resize {
                    source_width,
                    target_width,
                    signed,
                },
                ExprOpcode::ConcatPart {
                    part_width,
                    result_width,
                } => ExprOpcode::ConcatPart {
                    part_width,
                    result_width,
                },
                ExprOpcode::Ternary { then_len, else_len } => {
                    ExprOpcode::Ternary { then_len, else_len }
                }
                ExprOpcode::LoadIndexed {
                    location,
                    stride_bytes,
                    element_byte_size,
                    element_width,
                } => ExprOpcode::LoadIndexed {
                    location: bind_location(location, &mut resolve)?,
                    stride_bytes,
                    element_byte_size,
                    element_width,
                },
                ExprOpcode::LoadBitSelect {
                    location,
                    base_byte_size,
                    select_width,
                } => ExprOpcode::LoadBitSelect {
                    location: bind_location(location, &mut resolve)?,
                    base_byte_size,
                    select_width,
                },
                ExprOpcode::StoreU64 {
                    location,
                    byte_size,
                } => ExprOpcode::StoreU64 {
                    location: bind_location(location, &mut resolve)?,
                    byte_size,
                },
            };
            ops.push(bound);
        }
        Ok(ExprBytecode::new(ops))
    }
}

fn bind_location<A>(
    location: StateLocation<A>,
    resolve: &mut impl FnMut(&A) -> Option<usize>,
) -> Result<usize, BindError<A>> {
    let Some(base) = resolve(&location.address) else {
        return Err(BindError {
            address: location.address,
        });
    };
    Ok(base + location.byte_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_uses_source_independent_operators() {
        let code: ExprBytecode = ExprBytecode::new(vec![
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

    #[test]
    fn semantic_state_locations_bind_after_layout() {
        let code = ExprBytecode::new(vec![ExprOpcode::LoadU64 {
            location: StateLocation {
                address: 7u32,
                byte_offset: 3,
            },
            byte_size: 2,
            mask: 0xffff,
        }]);

        let bound = code
            .bind_with(|address| (*address == 7).then_some(100))
            .unwrap();
        assert_eq!(
            bound.ops(),
            &[ExprOpcode::LoadU64 {
                location: 103,
                byte_size: 2,
                mask: 0xffff,
            }]
        );
    }

    #[test]
    fn binding_reports_an_unmapped_semantic_address() {
        let code = ExprBytecode::new(vec![ExprOpcode::StoreU64 {
            location: StateLocation {
                address: 9u32,
                byte_offset: 0,
            },
            byte_size: 1,
        }]);

        assert_eq!(code.bind_with(|_| None), Err(BindError { address: 9 }));
    }
}
