//! Class object operations (IEEE 1800-2023 clause 8), lowered to calls of
//! the heap helper.
//!
//! An object handle is 32 bits like a string handle. The analyzer has laid
//! out each class, so an operation carries the byte offset and width of the
//! property it reads or writes.

use super::*;
use celox_heap::ops;
use sv::ir::{HandleProperty, ObjectOp};

impl Ff<'_, '_> {
    /// The value of object operation `op`, as a temporary.
    pub(super) fn object_call(
        &mut self,
        op: &ObjectOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported(format!("omitted argument of {op}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = match op {
            ObjectOp::Randomize(program) => {
                let object = self.handle(&args[0])?;
                let check = self.integer_of_width(&args[1], 1, false)?;
                let bytes = serde_json::to_vec(program).expect("constraint program serialization");
                let mut buffer = self.heap_call(ops::NEW, &[]);
                for chunk in bytes.chunks(8) {
                    let value = chunk
                        .iter()
                        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
                    let value = self.constant64(value);
                    let count = self.constant64(chunk.len() as u64);
                    buffer = self.heap_call(ops::APPEND_BYTES, &[buffer, value, count]);
                }
                let program = self.heap_call(ops::FREEZE, &[buffer]);
                if args.len() == 2 {
                    self.heap_call(ops::OBJECT_RANDOMIZE, &[object, program, check])
                } else {
                    let mut inputs = self.heap_call(ops::NEW, &[]);
                    let one = self.constant64(1);
                    inputs = self.heap_call(ops::APPEND_BYTES, &[inputs, check, one]);
                    for value in &args[2..] {
                        let value = self.integer_of_width(value, 64, false)?;
                        let eight = self.constant64(8);
                        inputs = self.heap_call(ops::APPEND_BYTES, &[inputs, value, eight]);
                    }
                    self.heap_call(ops::OBJECT_RANDOMIZE_INPUTS, &[object, program, inputs])
                }
            }
            ObjectOp::ConstraintModeGet(index) => {
                let object = self.handle(&args[0])?;
                self.heap_call(
                    ops::encode(ops::OBJECT_CONSTRAINT_MODE_GET, u64::from(*index)),
                    &[object],
                )
            }
            ObjectOp::ConstraintModeSet(index) => {
                let object = self.handle(&args[0])?;
                let enabled = self.integer_of_width(&args[1], 1, false)?;
                self.heap_call(
                    ops::encode(
                        ops::OBJECT_CONSTRAINT_MODE_SET,
                        u64::from(index.unwrap_or(u32::MAX)),
                    ),
                    &[object, enabled],
                )
            }
            ObjectOp::Container { .. } => unreachable!("handled by container_call"),
            ObjectOp::New {
                class,
                size,
                handles,
            } => {
                let size = self.constant64(u64::from(*size));
                let object =
                    self.heap_call(ops::encode(ops::OBJECT_NEW, u64::from(*class)), &[size]);
                // Object seeding consumes the creating process's RNG once,
                // before the constructor body (IEEE 1800-2023 18.14.3).
                let seed = self.urandom(None)?;
                let seed = self.integer_of_width(&seed, 32, false)?;
                self.heap_call(ops::OBJECT_SEED, &[object, seed]);
                for &(offset, kind) in handles {
                    let offset = self.constant64(u64::from(offset));
                    let kind = self.constant64(match kind {
                        HandleProperty::String => 0,
                        HandleProperty::Object => 1,
                    });
                    self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
                }
                object
            }
            ObjectOp::Load(field) if field.four_state && self.m.four_state => {
                let object = self.handle(&args[0])?;
                let [value, mask] = [0, field.bytes()].map(|plane| {
                    let offset = self.constant64(u64::from(field.offset + plane));
                    let count = self.constant64(u64::from(field.bytes()));
                    let stored = self.heap_call(ops::OBJECT_LOAD, &[object, offset, count]);
                    self.complement(stored)
                });
                let width = field.width as usize;
                let result = self.container_join(value, mask, width, true);
                let (temp, name) = self.m.temp("object", width, field.signed, true);
                self.store(temp, SIROffset::Static(0), width, result);
                return Ok(sv::ir::Expr::Ident(name));
            }
            ObjectOp::Load(field) => {
                let object = self.handle(&args[0])?;
                let offset = self.constant64(u64::from(field.offset));
                let count = self.constant64(u64::from(field.bytes()));
                self.heap_call(ops::OBJECT_LOAD, &[object, offset, count])
            }
            ObjectOp::Store(field) if field.four_state && self.m.four_state => {
                let object = self.handle(&args[0])?;
                let expr = self.hoist(&args[1])?;
                let value = self.eval(&expr, Some((field.width as usize, field.signed)))?;
                let (value, mask) = self.container_split(value);
                for (plane, bits) in [(0, value), (field.bytes(), mask)] {
                    let place = self.constant64(
                        u64::from(field.offset + plane) | u64::from(field.bytes()) << 32,
                    );
                    let stored = self.complement(bits);
                    self.heap_call(ops::OBJECT_STORE, &[object, place, stored]);
                }
                self.constant64(0)
            }
            ObjectOp::Store(field) => {
                let object = self.handle(&args[0])?;
                let place =
                    self.constant64(u64::from(field.offset) | u64::from(field.bytes()) << 32);
                let value = self.integer_of_width(&args[1], field.width as usize, field.signed)?;
                self.heap_call(ops::OBJECT_STORE, &[object, place, value])
            }
            ObjectOp::RandomCyclic { offset, count } => {
                let object = self.handle(&args[0])?;
                let count = self.constant64(u64::from(*count));
                self.heap_call(
                    ops::encode(ops::OBJECT_RANDOM_CYCLIC, u64::from(*offset)),
                    &[object, count],
                )
            }
            ObjectOp::Random
            | ObjectOp::Check(_)
            | ObjectOp::ModeGet(..)
            | ObjectOp::ModePush
            | ObjectOp::ModePop
            | ObjectOp::ModeSelected(_) => {
                let object = self.handle(&args[0])?;
                self.heap_call(
                    match op {
                        ObjectOp::Check(class) => ops::encode(ops::OBJECT_CHECK, u64::from(*class)),
                        ObjectOp::ModeGet(offset, random) => ops::encode(
                            ops::OBJECT_MODE_GET,
                            u64::from(*offset) | (u64::from(*random) << 32),
                        ),
                        ObjectOp::ModePush => ops::OBJECT_MODE_PUSH,
                        ObjectOp::ModePop => ops::OBJECT_MODE_POP,
                        ObjectOp::ModeSelected(offset) => {
                            ops::encode(ops::OBJECT_MODE_SELECTED, u64::from(*offset))
                        }
                        _ => ops::OBJECT_RANDOM,
                    },
                    &[object],
                )
            }
            ObjectOp::ModeSet(offset) | ObjectOp::ModeSelect(offset, _) => {
                let object = self.handle(&args[0])?;
                let enabled = self.integer_of_width(&args[1], 1, false)?;
                self.heap_call(
                    match op {
                        ObjectOp::ModeSelect(_, random) => ops::encode(
                            ops::OBJECT_MODE_SELECT,
                            u64::from(*offset) | (u64::from(*random) << 32),
                        ),
                        _ => ops::encode(ops::OBJECT_MODE_SET, u64::from(*offset)),
                    },
                    &[object, enabled],
                )
            }
            ObjectOp::Seed => {
                let object = self.handle(&args[0])?;
                let seed = self.integer_of_width(&args[1], 32, false)?;
                self.heap_call(ops::OBJECT_SEED, &[object, seed])
            }
            ObjectOp::ClassOf => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_CLASS, &[object])
            }
            ObjectOp::Copy => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_COPY, &[object])
            }
        };
        let (width, signed) = op.result_type();
        let value = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("object", width, signed, false);
        let kind = match op {
            ObjectOp::Container { .. } => unreachable!("handled by container_call"),
            ObjectOp::New { .. } | ObjectOp::Copy => Some(celox_design::ValueKind::ObjRef),
            ObjectOp::Load(field) => field.handle.map(|handle| match handle {
                HandleProperty::String => celox_design::ValueKind::StrRef,
                HandleProperty::Object => celox_design::ValueKind::ObjRef,
            }),
            ObjectOp::Randomize(_)
            | ObjectOp::ConstraintModeGet(_)
            | ObjectOp::ConstraintModeSet(_)
            | ObjectOp::Store(_)
            | ObjectOp::ClassOf
            | ObjectOp::Random
            | ObjectOp::RandomCyclic { .. }
            | ObjectOp::Seed
            | ObjectOp::Check(_)
            | ObjectOp::ModeGet(..)
            | ObjectOp::ModePush
            | ObjectOp::ModePop
            | ObjectOp::ModeSelect(..)
            | ObjectOp::ModeSelected(_)
            | ObjectOp::ModeSet(_) => None,
        };
        if let Some(kind) = kind {
            self.m.variables.get_mut(&temp).unwrap().value_kind = kind;
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// A 4-state property holds the complement of its value and mask planes,
    /// so the zeroed bytes of a new object read as X (IEEE 1800-2023 6.8).
    fn complement(&mut self, plane: RegisterId) -> RegisterId {
        let complement = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Unary(complement, UnaryOp::BitNot, plane));
        complement
    }
}
