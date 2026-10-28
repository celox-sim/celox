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
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported(format!("omitted argument of {op}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if args.iter().any(|arg| self.m.calls(arg)) {
            for (index, arg) in args.iter_mut().enumerate() {
                // A record is built from a flat structure value, not a handle.
                if let ObjectOp::Box(record) = op {
                    *arg = self.heap_operand(arg, Some((record.width(), false)), None)?;
                    continue;
                }
                let (width, signed) = if index == 0 {
                    (32, false)
                } else {
                    match op {
                        ObjectOp::Store(field) => (field.width as usize, field.signed),
                        ObjectOp::Randomize(_) if index == 1 => (1, false),
                        ObjectOp::ConstraintModeSet(_)
                        | ObjectOp::ModeSet(_)
                        | ObjectOp::ModeSelect(..) => (1, false),
                        ObjectOp::Seed => (32, false),
                        _ => (64, false),
                    }
                };
                *arg = self.heap_operand(
                    arg,
                    Some((width, signed)),
                    (index == 0).then_some(celox_design::ValueKind::ObjRef),
                )?;
            }
        }
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
            ObjectOp::Locator(_) => unreachable!("handled by locator"),
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
                let result = self.load_field(object, *field);
                let width = field.width as usize;
                let (temp, name) = self.m.temp("object", width, field.signed, true);
                self.store(temp, SIROffset::Static(0), width, result);
                return Ok(sv::ir::Expr::Ident(name));
            }
            ObjectOp::Load(field) => {
                let object = self.handle(&args[0])?;
                self.load_field(object, *field)
            }
            ObjectOp::Unbox(record) => {
                let object = self.handle(&args[0])?;
                let parts = record
                    .fields
                    .iter()
                    .map(|field| self.load_field(object, *field))
                    .collect::<Vec<_>>();
                let width = record.width();
                let four_state = self.m.four_state && record.fields.iter().any(|f| f.four_state);
                let value = if four_state {
                    self.b.alloc_logic(width)
                } else {
                    self.b.alloc_bit(width, false)
                };
                self.b.emit(SIRInstruction::Concat(value, parts));
                let (temp, name) = self.m.temp("record", width, false, four_state);
                self.store(temp, SIROffset::Static(0), width, value);
                return Ok(sv::ir::Expr::Ident(name));
            }
            ObjectOp::Box(record) => {
                // Evaluate the value first: its calls may end the block, so
                // the record's handle is only used after them.
                let expr = self.hoist(&args[0])?;
                let width = record.width();
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(&expr, Some((width, false)), &mut arena)?;
                let node =
                    coerce_node_width(&mut arena, node, Some(width), false).map_err(slt_error)?;
                let value = self.lower_slt(&arena, node)?;
                // A record is not seeded: it has no random state, and seeding
                // would draw from the creating process's RNG.
                let size = self.constant64(u64::from(record.size));
                let object = self.heap_call(
                    ops::encode(ops::OBJECT_NEW, u64::from(sv::ir::Record::CLASS)),
                    &[size],
                );
                for field in &record.fields {
                    if let Some(kind) = field.handle {
                        let offset = self.constant64(u64::from(field.offset));
                        let kind = self.constant64(match kind {
                            HandleProperty::String => 0,
                            HandleProperty::Object => 1,
                        });
                        self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
                    }
                }
                let mut lsb = width;
                for field in &record.fields {
                    lsb -= field.width as usize;
                    let part = if matches!(self.b.register(&value), RegisterType::Logic { .. }) {
                        self.b.alloc_logic(field.width as usize)
                    } else {
                        self.b.alloc_bit(field.width as usize, false)
                    };
                    self.b.emit(SIRInstruction::Slice(
                        part,
                        value,
                        lsb,
                        field.width as usize,
                    ));
                    self.store_field(object, *field, part);
                }
                // An equal value keeps the handle it already has, so that
                // storing it again leaves a container unchanged.
                self.heap_call(ops::OBJECT_FINISH_RECORD, &[object])
            }
            ObjectOp::Store(field) if field.four_state && self.m.four_state => {
                let object = self.handle(&args[0])?;
                let expr = self.hoist(&args[1])?;
                let value = self.eval(&expr, Some((field.width as usize, field.signed)))?;
                self.store_field(object, *field, value);
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
            ObjectOp::Locator(_) => unreachable!("handled by locator"),
            ObjectOp::New { .. } | ObjectOp::Copy | ObjectOp::Box(_) => {
                Some(celox_design::ValueKind::ObjRef)
            }
            ObjectOp::Unbox(_) => unreachable!("lowered to its own temporary"),
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

    /// The value of `field` of the object in 64-bit `object`: a register of
    /// the field's width, 4-state for a 4-state field in 4-state simulation.
    fn load_field(&mut self, object: RegisterId, field: sv::ir::ObjectField) -> RegisterId {
        let opcode = if field.nullable {
            ops::OBJECT_LOAD_NULLABLE
        } else {
            ops::OBJECT_LOAD
        };
        let width = field.width as usize;
        if field.four_state && self.m.four_state {
            let [value, mask] = [0, field.bytes()].map(|plane| {
                let offset = self.constant64(u64::from(field.offset + plane));
                let count = self.constant64(u64::from(field.bytes()));
                let stored = self.heap_call(opcode, &[object, offset, count]);
                self.complement(stored)
            });
            return self.container_join(value, mask, width, true);
        }
        let offset = self.constant64(u64::from(field.offset));
        let count = self.constant64(u64::from(field.bytes()));
        let loaded = self.heap_call(opcode, &[object, offset, count]);
        let value = self.b.alloc_bit(width, field.signed);
        self.b.emit(SIRInstruction::Slice(value, loaded, 0, width));
        value
    }

    /// Store register `value` of `field`'s width into `field` of the object in
    /// 64-bit `object`.
    fn store_field(&mut self, object: RegisterId, field: sv::ir::ObjectField, value: RegisterId) {
        let planes = if field.four_state && self.m.four_state {
            let (value, mask) = self.container_split(value);
            vec![
                (0, self.complement(value)),
                (field.bytes(), self.complement(mask)),
            ]
        } else {
            let known = self.b.alloc_bit(field.width as usize, false);
            self.b
                .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
            let wide = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, known));
            vec![(0, wide)]
        };
        for (plane, bits) in planes {
            let place =
                self.constant64(u64::from(field.offset + plane) | u64::from(field.bytes()) << 32);
            self.heap_call(ops::OBJECT_STORE, &[object, place, bits]);
        }
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
