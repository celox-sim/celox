use self::arena::SLTNodeArena;
mod arena;
mod effect;
mod expr;
mod recover_unrolled;
pub(crate) use recover_unrolled::{
    complete_binary_cases, proves_binary_combinational, proves_combinational,
};
pub(crate) mod sensitivity;
mod state;

mod assign;
mod comb;
mod control;
mod dynamic_select;
mod for_loop;
mod function_call;
mod statement;
mod written;
pub(crate) use assign::*;
pub use comb::*;
pub(crate) use control::*;
pub(in crate::lowering) use dynamic_select::*;
use for_loop::*;
pub(in crate::lowering) use function_call::*;
use statement::*;
pub(crate) use written::*;

pub(crate) mod node {
    pub(crate) use super::arena::SLTNodeArena;
    pub use celox_slt::{
        NodeId, SLTForEffect, SLTForFoldGroupState, SLTForFoldResult, SLTForUpdate, SLTIndex,
        SLTIndexKind, SLTLoopBound, SLTNode, SLTStepOp,
    };
}

pub(crate) mod node_facts {
    pub use celox_slt::SLTNodeFactsError;
}

pub use celox_slt::{LogicPath, LogicPathTarget};
pub use state::{BoundaryMap, SymbolicStore};

use std::{collections::BTreeSet, hash::Hash};

use crate::{
    HashMap, HashSet, ParserError,
    bitaccess::{
        PartSelectGeometry, celox_value_from_comptime, eval_constexpr, eval_var_select,
        eval_var_select_with_geometry, select_geometry,
    },
    function_call_has_arg,
    loop_provenance::LoopRecoveryCandidate,
    resolve_total_width,
};
use celox_design::{
    BinaryOp, BitAccess, DisplaySizing, RuntimeEventKind, RuntimeEventSite, UnaryOp, VarAtomBase,
};
use celox_slt::{CombObserver, RangeStore, RangeStoreError};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{ToPrimitive as _, Zero as _};
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignStatement, CasePattern, CaseStatement, CombDeclaration, Expression,
    Factor, ForBound, ForRange, ForStatement, Function, FunctionBody, FunctionCall, IfStatement,
    Module, Op, Statement, SystemFunctionCall, SystemFunctionInput, SystemFunctionKind, VarId,
    VarIndex, VarPath, VarSelect,
};
use veryl_analyzer::value::{MaskCache, Value, byte_value_to_string};
use veryl_parser::resource_table;
use veryl_parser::token_range::TokenRange;

pub(crate) use effect::{
    CombEffectCollector, collect_and_advance_expression, expression_contains_runtime_effect,
    subtract_written_sensitivity,
};
use effect::{collect_comb_effects_statements, statements_contain_runtime_effect};
pub use expr::coerce_node_width;
use expr::{
    eval_array_literal_expression_effectful, eval_case_arm_condition_effectful,
    eval_case_target_effectful, eval_function_body_return, merge_boundaries,
};
pub(crate) use expr::{eval_assignment_expression_effectful, eval_expression_effectful};
pub use expr::{eval_expression, get_width};
use state::{FunctionControlState, LoopControlState};

type ActiveGuard = (NodeId, HashSet<VarAtomBase<VarId>>);

pub use node::{
    NodeId, SLTForEffect, SLTForFoldGroupState, SLTForFoldResult, SLTForUpdate, SLTIndex,
    SLTIndexKind, SLTLoopBound, SLTNode, SLTStepOp,
};
pub use node_facts::SLTNodeFactsError;

pub(super) fn range_store_error(
    context: &'static str,
    error: RangeStoreError,
    token: Option<&TokenRange>,
) -> ParserError {
    ParserError::illegal_context(context, error.to_string(), token)
}

pub(super) fn symbolic_parts_or_default(
    store: &SymbolicStore<VarId>,
    id: VarId,
    access: BitAccess,
    context: &'static str,
    token: Option<&TokenRange>,
) -> Result<Vec<(Option<(NodeId, HashSet<VarAtomBase<VarId>>)>, BitAccess)>, ParserError> {
    match store.get(&id) {
        Some(range_store) => range_store
            .get_parts(access)
            .map_err(|error| range_store_error(context, error, token)),
        None => Ok(vec![(None, access)]),
    }
}

#[cfg(test)]
mod tests;
