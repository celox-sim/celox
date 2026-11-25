//! Coverage operations of covergroups (IEEE 1800-2023 clause 19), lowered
//! to calls of the coverage helper.
//!
//! The helper is the heap helper under a `func` of its own
//! (`extern_abi::COVERAGE_HELPER_ID`): the heap keeps what covergroups
//! collect, and a coverage operation reads and writes nothing of the
//! simulation state, which the scheduler tells by the `func`. So a unit
//! that samples in clocked logic is planned as one without the call.

use super::*;
use celox_heap::coverage::{self, op};
use sv::ir::{CoverageOp, CoverageQuery, HandleProperty};

impl Ff<'_, '_> {
    /// Call the coverage helper with coverage operation `sub`, its
    /// immediate operands `x` and `y`, and up to three operands; the result
    /// is a 64-bit register.
    fn coverage_helper_call(
        &mut self,
        sub: u64,
        x: u64,
        y: u64,
        operands: &[RegisterId],
    ) -> RegisterId {
        let code = coverage::code(sub, x, y);
        debug_assert_eq!(code & 0xff, coverage::COVERAGE);
        let mut args = vec![self.constant64(code)];
        args.extend_from_slice(operands);
        while args.len() < celox_sir::extern_abi::HEAP_HELPER_ARGUMENTS {
            args.push(self.constant64(0));
        }
        let dst = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(dst),
            func: celox_sir::extern_abi::COVERAGE_HELPER_ID,
            args,
        });
        dst
    }

    /// The value of coverage operation `op`, as a temporary.
    pub(super) fn coverage_call(
        &mut self,
        op: &CoverageOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        // `$get_coverage` of a design that constructs no covergroup is 100
        // (19.11), with no call.
        if matches!(op, CoverageOp::Overall) && self.m.no_covergroups {
            return Ok(sv::ir::Expr::Literal("100.0".into()));
        }
        // The first operand is the key of a type or the handle of an
        // instance; the second names an item.
        let keyed = matches!(
            op,
            CoverageOp::Registered
                | CoverageOp::Register(_)
                | CoverageOp::New { .. }
                | CoverageOp::TypeCoverage { .. }
        );
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported("omitted argument of a coverage operation"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if args.iter().any(|arg| self.m.calls(arg)) {
            for (index, arg) in args.iter_mut().enumerate() {
                *arg = if index == 0 && keyed {
                    self.heap_operand(arg, Some((64, false)), None)?
                } else if index == 0 {
                    self.heap_operand(
                        arg,
                        Some((celox_design::HANDLE_WIDTH, false)),
                        Some(celox_design::ValueKind::ObjRef),
                    )?
                } else {
                    self.heap_operand(arg, Some((32, false)), None)?
                };
            }
        }
        let first = match args.first() {
            Some(first) if keyed => Some(self.integer_of_width(first, 64, false)?),
            Some(first) => Some(self.handle(first)?),
            None => None,
        };
        let first = |operation: &str| {
            first.ok_or_else(|| {
                unsupported(format!("coverage operation {operation} without an operand"))
            })
        };
        let result = match op {
            CoverageOp::Registered => {
                let key = first("registered")?;
                self.coverage_helper_call(op::REGISTERED, 0, 0, &[key])
            }
            CoverageOp::Register(descriptor) => {
                let key = first("register")?;
                // The descriptor travels as its JSON text, eight bytes a
                // call, to the heap that then registers it.
                let bytes = serde_json::to_vec(&**descriptor)
                    .map_err(|error| unsupported(format!("covergroup descriptor: {error}")))?;
                for chunk in bytes.chunks(8) {
                    let word = chunk
                        .iter()
                        .fold(0u64, |word, byte| (word << 8) | u64::from(*byte));
                    let word = self.constant64(word);
                    self.coverage_helper_call(
                        op::REGISTER_BYTES,
                        chunk.len() as u64,
                        0,
                        &[key, word],
                    );
                }
                self.coverage_helper_call(op::REGISTER, 0, 0, &[key])
            }
            CoverageOp::New {
                class,
                size,
                handles,
            } => {
                let key = first("new")?;
                let size = self.constant64(u64::from(*size));
                let class = self.constant64(u64::from(*class));
                let object = self.coverage_helper_call(op::NEW, 0, 0, &[key, size, class]);
                for &(offset, kind) in handles {
                    let offset = self.constant64(u64::from(offset));
                    let kind = self.constant64(match kind {
                        HandleProperty::String => 0,
                        HandleProperty::Object => 1,
                    });
                    self.heap_call(celox_heap::ops::OBJECT_HANDLE, &[object, offset, kind]);
                }
                object
            }
            CoverageOp::Build => {
                let object = first("build")?;
                self.coverage_helper_call(op::BUILD, 0, 0, &[object])
            }
            CoverageOp::Sampling => {
                let object = first("sampling")?;
                self.coverage_helper_call(op::SAMPLING, 0, 0, &[object])
            }
            CoverageOp::SampleEnd => {
                let object = first("sample")?;
                self.coverage_helper_call(op::SAMPLE_END, 0, 0, &[object])
            }
            CoverageOp::InstCoverage { what } | CoverageOp::TypeCoverage { what } => {
                let target = first("query")?;
                let item = match args.get(1) {
                    Some(item) => self.integer_of_width(item, 32, false)?,
                    None => self.constant64(0),
                };
                let what = match what {
                    CoverageQuery::Real => 0,
                    CoverageQuery::Covered => 1,
                    CoverageQuery::Total => 2,
                };
                let sub = if matches!(op, CoverageOp::InstCoverage { .. }) {
                    op::INST_COVERAGE
                } else {
                    op::TYPE_COVERAGE
                };
                self.coverage_helper_call(sub, what, 0, &[target, item])
            }
            CoverageOp::Overall => self.coverage_helper_call(op::OVERALL, 0, 0, &[]),
        };
        let (width, signed) = op.result_type();
        let value = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("coverage", width, signed, false);
        let variable = self.m.variables.get_mut(&temp).unwrap();
        if matches!(op, CoverageOp::New { .. }) {
            variable.value_kind = celox_design::ValueKind::ObjRef;
        }
        // A coverage is a `real`, whose bits the helper returns.
        if op.is_real() {
            variable.float = Some(celox_sir::FloatPrecision::F64);
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }
}
