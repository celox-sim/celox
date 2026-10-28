//! Heap operations that compiled code calls through one helper.
//!
//! An operation takes an opcode word and up to three 64-bit arguments and
//! returns a 64-bit result, so every backend calls it like a C function. The
//! low byte of the opcode word selects the operation; higher bits carry its
//! immediate parameters.
//!
//! String arguments are [`Handle`]s of interned strings. Operations that
//! build a string return a buffer: a mutable string outside the intern table
//! that later operations extend in place and [`FREEZE`] interns. A buffer is
//! consumed by the operation it is passed to, and none outlives the kernel
//! that created it, so a collection discards every buffer.

use crate::{Handle, Heap};

/// `compare(a, b)`: -1, 0 or 1 as `strcmp` orders strings `a` and `b`.
pub const COMPARE: u64 = 0;
/// `icompare(a, b)`: [`COMPARE`] ignoring ASCII case.
pub const ICOMPARE: u64 = 1;
/// `len(a)`: the length of string `a` in bytes.
pub const LEN: u64 = 2;
/// `getc(a, i)`: byte `i` of string `a`, or 0 out of range. `i` is signed.
pub const GETC: u64 = 3;
/// `atoi(a)`: the leading digits of string `a` in the radix of the
/// parameter, as a 32-bit integer.
pub const ATOI: u64 = 4;
/// `new()`: an empty buffer.
pub const NEW: u64 = 16;
/// `copy(a)`: a buffer holding string `a`.
pub const COPY: u64 = 17;
/// `append(buffer, a)`: `buffer` extended by string `a`.
pub const APPEND: u64 = 18;
/// `append_bytes(buffer, value, count)`: `buffer` extended by the low `count`
/// bytes of `value`, most significant first. `count` is at most 8.
pub const APPEND_BYTES: u64 = 19;
/// `putc(buffer, i, c)`: `buffer` with byte `i` replaced by `c`, unless `i`
/// is out of range or `c` is 0. `i` is signed.
pub const PUTC: u64 = 20;
/// `substr(a, i, j)`: a buffer with bytes `i..=j` of string `a`, or empty if
/// `i < 0`, `j < i` or `j >= len`. `i` and `j` are signed.
pub const SUBSTR: u64 = 21;
/// `toupper(a)`: a buffer with string `a` in ASCII upper case.
pub const TO_UPPER: u64 = 22;
/// `tolower(a)`: a buffer with string `a` in ASCII lower case.
pub const TO_LOWER: u64 = 23;
/// `replicate(a, n)`: a buffer with `n` copies of string `a`.
pub const REPLICATE: u64 = 24;
/// `itoa(value)`: a buffer with `value` in the radix of the parameter.
pub const ITOA: u64 = 25;
/// `freeze(buffer)`: the interned string holding `buffer`, which is freed.
pub const FREEZE: u64 = 32;
/// `new_object(size)`: a new object of the class in the parameter, with
/// `size` zeroed bytes.
pub const OBJECT_NEW: u64 = 48;
/// `new_event()`: the handle of a new named event (IEEE 1800-2023 15.5). The
/// scheduler keeps the event's state; the heap only names it.
pub const EVENT_NEW: u64 = 47;
/// `declare_handle(object, offset, kind)`: the property at byte `offset`
/// holds a handle; `kind` is 0 for a string and 1 for an object.
pub const OBJECT_HANDLE: u64 = 49;
/// `load(object, offset, count)`: `count` bytes at `offset`, at most 8.
pub const OBJECT_LOAD: u64 = 50;
/// `store(object, offset | count << 32, value)`: write `count` bytes.
pub const OBJECT_STORE: u64 = 51;
/// `class_of(object)`: its class, or 0 for `null`.
pub const OBJECT_CLASS: u64 = 52;
/// `random(object)`: 64 random bits from the object's RNG.
pub const OBJECT_RANDOM: u64 = 53;
/// `seed(object, seed)`: reset the object's RNG using the low 32 bits.
pub const OBJECT_SEED: u64 = 54;
/// `check(object)`: record a fault for an invalid receiver or, when the
/// parameter is nonzero, a different runtime class. Return 1 on success, 0 on failure.
pub const OBJECT_CHECK: u64 = 55;
/// Query the random mode of the property at the parameter's byte offset.
pub const OBJECT_MODE_GET: u64 = 56;
/// Set the random mode at the parameter's byte offset from the low bit of b.
pub const OBJECT_MODE_SET: u64 = 57;
/// Save/restore random modes around an inline member selection.
pub const OBJECT_MODE_PUSH: u64 = 58;
pub const OBJECT_MODE_POP: u64 = 59;
/// Install a selected mode; parameter bit 32 records a declared rand property.
pub const OBJECT_MODE_SELECT: u64 = 60;
/// Whether a non-rand property is temporarily random in the active scope.
pub const OBJECT_MODE_SELECTED: u64 = 61;
/// Draw a randc domain index; parameter is the property offset, b its domain size.
pub const OBJECT_RANDOM_CYCLIC: u64 = 62;
/// `copy(object)`: a new object with a copy of the properties of `object`.
pub const OBJECT_COPY: u64 = 63;
/// Solve the scalar constraint program in string b for object a; c selects check-only.
pub const OBJECT_RANDOMIZE: u64 = 64;
/// Program b for object a; c is an input buffer (check-only byte followed by
/// big-endian u64 values). The buffer is consumed, including on failure.
pub const OBJECT_RANDOMIZE_INPUTS: u64 = 65;
/// Constraint index is encoded in the parameter; set-all uses u32::MAX.
pub const OBJECT_CONSTRAINT_MODE_GET: u64 = 66;
pub const OBJECT_CONSTRAINT_MODE_SET: u64 = 67;
/// `load(record, offset, count)` of a structure record: `null` reads as
/// zero bytes without a fault.
pub const OBJECT_LOAD_NULLABLE: u64 = 68;
/// `finish(record)` of a structure record whose fields are all stored: the
/// handle of the one record with its contents, which may replace `record`.
pub const OBJECT_FINISH_RECORD: u64 = 69;

/// The opcode word of `op` with parameter `parameter`.
pub const fn encode(op: u64, parameter: u64) -> u64 {
    op | parameter << 8
}

/// The parameter of [`ATOI`] and [`ITOA`]: the radix, and for [`ITOA`] the
/// width of `value` in bits and whether it is signed.
pub const fn radix_parameter(radix: u32, width: u32, signed: bool) -> u64 {
    radix as u64 | (width as u64) << 8 | (signed as u64) << 16
}

/// The most copies [`REPLICATE`] makes, so a runaway count fails loudly
/// instead of exhausting memory.
const MAX_REPLICATE_BYTES: usize = 1 << 30;

impl Heap {
    /// Perform the operation selected by `code` on `a`, `b` and `c`.
    pub fn call(&mut self, code: u64, a: u64, b: u64, c: u64) -> u64 {
        let parameter = code >> 8;
        match code & 0xff {
            op @ crate::containers::NEW..=crate::containers::RSORT => {
                self.container_call(op, parameter, a, b, c)
            }
            crate::containers::PATTERN => {
                let bytes = self.container_pattern(a as Handle, parameter);
                self.new_buffer(bytes)
            }
            crate::containers::HANDLE_PATTERN => {
                self.new_buffer(crate::containers::handle_pattern(a as u32 as u64))
            }
            COMPARE => ordering(self.string(a as Handle).cmp(self.string(b as Handle))),
            ICOMPARE => {
                let lower = |handle| {
                    self.string(handle as Handle)
                        .iter()
                        .map(u8::to_ascii_lowercase)
                };
                ordering(lower(a).cmp(lower(b)))
            }
            LEN => self.string(a as Handle).len() as u64,
            GETC => usize::try_from(b as i64)
                .ok()
                .and_then(|index| self.string(a as Handle).get(index))
                .map_or(0, |&byte| byte.into()),
            ATOI => atoi(self.string(a as Handle), parameter as u32 & 0xff),
            NEW => self.new_buffer(Vec::new()),
            COPY => self.new_buffer(self.string(a as Handle).to_vec()),
            APPEND => {
                let tail = self.string(b as Handle).to_vec();
                self.buffer(a).extend_from_slice(&tail);
                a
            }
            APPEND_BYTES => {
                let count = (c as usize).min(8);
                let bytes = b.to_be_bytes();
                self.buffer(a).extend_from_slice(&bytes[8 - count..]);
                a
            }
            PUTC => {
                let buffer = self.buffer(a);
                if let Ok(index) = usize::try_from(b as i64)
                    && c as u8 != 0
                    && let Some(byte) = buffer.get_mut(index)
                {
                    *byte = c as u8;
                }
                a
            }
            SUBSTR => {
                let string = self.string(a as Handle);
                let (i, j) = (b as i64, c as i64);
                let bytes = if i < 0 || j < i || j >= string.len() as i64 {
                    Vec::new()
                } else {
                    string[i as usize..=j as usize].to_vec()
                };
                self.new_buffer(bytes)
            }
            TO_UPPER => self.new_buffer(self.string(a as Handle).to_ascii_uppercase()),
            TO_LOWER => self.new_buffer(self.string(a as Handle).to_ascii_lowercase()),
            REPLICATE => {
                let string = self.string(a as Handle);
                let count = usize::try_from(b as i64).unwrap_or(0);
                assert!(
                    string.len().saturating_mul(count) <= MAX_REPLICATE_BYTES,
                    "string replication of {} bytes {count} times is too large",
                    string.len()
                );
                self.new_buffer(string.repeat(count))
            }
            ITOA => self.new_buffer(itoa(
                a,
                parameter as u32 & 0xff,
                (parameter >> 8) as u32 & 0xff,
                parameter >> 16 & 1 != 0,
            )),
            FREEZE => {
                let bytes = self.take_buffer(a);
                self.intern(&bytes).into()
            }
            EVENT_NEW => self.new_event().into(),
            OBJECT_NEW => self.new_object(parameter as u32, a as usize).into(),
            OBJECT_HANDLE => {
                let kind = if c == 0 {
                    crate::HandleKind::String
                } else {
                    crate::HandleKind::Object
                };
                self.declare_handle(a as Handle, b as u32, kind);
                0
            }
            OBJECT_LOAD => self.load(a as Handle, b as usize, c as usize),
            OBJECT_LOAD_NULLABLE if a as Handle == crate::objects::NULL => 0,
            OBJECT_LOAD_NULLABLE => self.load(a as Handle, b as usize, c as usize),
            OBJECT_FINISH_RECORD => self.finish_record(a as Handle).into(),
            OBJECT_STORE => {
                self.store(a as Handle, b as u32 as usize, (b >> 32) as usize, c);
                0
            }
            OBJECT_CLASS => self.class_of(a as Handle).into(),
            op @ (OBJECT_RANDOMIZE | OBJECT_RANDOMIZE_INPUTS) => {
                let input_bytes = (op == OBJECT_RANDOMIZE_INPUTS).then(|| self.take_buffer(c));
                let result = (|| -> Result<_, String> {
                    let program = serde_json::from_slice::<crate::randomization::Program>(
                        self.string(b as Handle),
                    )
                    .map_err(|error| error.to_string())?;
                    let (check_only, inputs) = if op == OBJECT_RANDOMIZE_INPUTS {
                        let bytes = input_bytes.as_ref().unwrap();
                        let (&check, values) = bytes
                            .split_first()
                            .ok_or("missing constraint input buffer")?;
                        if check > 1 || values.len() % 8 != 0 {
                            return Err("invalid constraint input buffer".into());
                        }
                        (
                            check != 0,
                            values
                                .as_chunks::<8>()
                                .0
                                .iter()
                                .map(|chunk| u64::from_be_bytes(*chunk))
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        (c != 0, Vec::new())
                    };
                    Ok(self.randomize_object_with_inputs(
                        a as Handle,
                        &program,
                        check_only,
                        &inputs,
                    ))
                })();
                match result {
                    Ok(success) => u64::from(success),
                    Err(error) => {
                        self.fault
                            .get_or_insert(crate::Fault::ConstraintRandomization(error));
                        0
                    }
                }
            }
            OBJECT_RANDOM => self.random_object(a as Handle),
            OBJECT_RANDOM_CYCLIC => self.random_cyclic_object(a as Handle, parameter as u32, b),
            OBJECT_SEED => {
                self.seed_object(a as Handle, b as u32);
                0
            }
            OBJECT_CHECK => u64::from(self.check_object(a as Handle, parameter as u32)),
            OBJECT_CONSTRAINT_MODE_GET => {
                self.constraint_mode(a as Handle, parameter as u32) as u64
            }
            OBJECT_CONSTRAINT_MODE_SET => {
                self.set_constraint_mode(
                    a as Handle,
                    (parameter != u64::from(u32::MAX)).then_some(parameter as u32),
                    b & 1 != 0,
                );
                0
            }
            OBJECT_MODE_GET => {
                u64::from(self.random_mode(a as Handle, parameter as u32, parameter >> 32 != 0))
            }
            OBJECT_MODE_SET => {
                self.set_random_mode(a as Handle, parameter as u32, b & 1 != 0);
                0
            }
            OBJECT_MODE_PUSH => {
                self.push_random_modes(a as Handle);
                0
            }
            OBJECT_MODE_POP => {
                self.pop_random_modes(a as Handle);
                0
            }
            OBJECT_MODE_SELECT => {
                self.select_random_mode(
                    a as Handle,
                    parameter as u32,
                    parameter >> 32 != 0,
                    b & 1 != 0,
                );
                0
            }
            OBJECT_MODE_SELECTED => {
                u64::from(self.selected_random_mode(a as Handle, parameter as u32))
            }
            OBJECT_COPY => self.copy_object(a as Handle).into(),
            other => panic!("unknown heap operation {other}"),
        }
    }
}

fn ordering(order: std::cmp::Ordering) -> u64 {
    order as i64 as u64
}

/// IEEE 1800-2023 6.16.9: leading digits and underscores, wrapping to 32 bits.
fn atoi(string: &[u8], radix: u32) -> u64 {
    let mut value: u32 = 0;
    for &byte in string {
        if byte == b'_' {
            continue;
        }
        let Some(digit) = char::from(byte).to_digit(radix) else {
            break;
        };
        value = value.wrapping_mul(radix).wrapping_add(digit);
    }
    value.into()
}

/// `value`, the low `width` bits of which are significant, in `radix`.
fn itoa(value: u64, radix: u32, width: u32, signed: bool) -> Vec<u8> {
    let width = width.clamp(1, 64);
    let unsigned = if width == 64 {
        value
    } else {
        value & ((1 << width) - 1)
    };
    let negative = signed && radix == 10 && unsigned >> (width - 1) & 1 != 0;
    let mut magnitude = if negative {
        // Two's complement magnitude of the sign-extended value.
        (unsigned | !0 << (width - 1)).wrapping_neg()
    } else {
        unsigned
    };
    let mut digits = Vec::new();
    loop {
        let digit = (magnitude % u64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).unwrap() as u8);
        magnitude /= u64::from(radix);
        if magnitude == 0 {
            break;
        }
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(heap: &mut Heap, text: &str) -> u64 {
        heap.intern(text.as_bytes()).into()
    }

    fn frozen(heap: &mut Heap, buffer: u64) -> Vec<u8> {
        let handle = heap.call(FREEZE, buffer, 0, 0);
        heap.string(handle as Handle).to_vec()
    }

    #[test]
    fn comparisons_order_like_strcmp() {
        let mut heap = Heap::new();
        let (abc, abd, upper) = (
            string(&mut heap, "abc"),
            string(&mut heap, "abd"),
            string(&mut heap, "ABC"),
        );
        assert_eq!(heap.call(COMPARE, abc, abd, 0) as i64, -1);
        assert_eq!(heap.call(COMPARE, abd, abc, 0) as i64, 1);
        assert_eq!(heap.call(COMPARE, abc, abc, 0), 0);
        assert_eq!(heap.call(COMPARE, 0, abc, 0) as i64, -1);
        assert_eq!(heap.call(ICOMPARE, abc, upper, 0), 0);
    }

    #[test]
    fn character_access_ignores_out_of_range_indices() {
        let mut heap = Heap::new();
        let abc = string(&mut heap, "abc");
        assert_eq!(heap.call(LEN, abc, 0, 0), 3);
        assert_eq!(heap.call(GETC, abc, 1, 0), u64::from(b'b'));
        assert_eq!(heap.call(GETC, abc, 3, 0), 0);
        assert_eq!(heap.call(GETC, abc, -1i64 as u64, 0), 0);
        let buffer = heap.call(COPY, abc, 0, 0);
        let buffer = heap.call(PUTC, buffer, 0, u64::from(b'x'));
        let buffer = heap.call(PUTC, buffer, 1, 0);
        let buffer = heap.call(PUTC, buffer, 3, u64::from(b'y'));
        assert_eq!(frozen(&mut heap, buffer), b"xbc");
    }

    #[test]
    fn substrings_follow_the_lrm_bounds() {
        let mut heap = Heap::new();
        let text = string(&mut heap, "hello");
        let cases = [
            (1, 3, "ell"),
            (0, 4, "hello"),
            (-1, 2, ""),
            (3, 2, ""),
            (2, 5, ""),
        ];
        for (i, j, expected) in cases {
            let buffer = heap.call(SUBSTR, text, i as u64, j as u64);
            assert_eq!(frozen(&mut heap, buffer), expected.as_bytes(), "{i}..={j}");
        }
    }

    #[test]
    fn buffers_build_concatenations_and_replications() {
        let mut heap = Heap::new();
        let (a, b) = (string(&mut heap, "ab"), string(&mut heap, "cd"));
        let buffer = heap.call(COPY, a, 0, 0);
        let buffer = heap.call(APPEND, buffer, b, 0);
        let buffer = heap.call(APPEND_BYTES, buffer, 0x0065_6667, 3);
        let joined = heap.call(FREEZE, buffer, 0, 0);
        assert_eq!(heap.string(joined as Handle), b"abcdefg");
        // A literal assembled from bytes interns to the same handle.
        let buffer = heap.call(NEW, 0, 0, 0);
        let buffer = heap.call(APPEND_BYTES, buffer, u64::from_be_bytes(*b"\0abcdefg"), 7);
        assert_eq!(heap.call(FREEZE, buffer, 0, 0), joined);
        let buffer = heap.call(REPLICATE, a, 3, 0);
        assert_eq!(frozen(&mut heap, buffer), b"ababab");
        let buffer = heap.call(TO_UPPER, a, 0, 0);
        assert_eq!(frozen(&mut heap, buffer), b"AB");
    }

    #[test]
    fn numeric_conversions_follow_the_lrm() {
        let mut heap = Heap::new();
        let atoi_of = |heap: &mut Heap, text: &str, radix| {
            let handle = string(heap, text);
            heap.call(encode(ATOI, radix_parameter(radix, 0, false)), handle, 0, 0)
        };
        assert_eq!(atoi_of(&mut heap, "1_234x5", 10), 1234);
        assert_eq!(atoi_of(&mut heap, "-12", 10), 0);
        assert_eq!(atoi_of(&mut heap, "fF", 16), 255);
        assert_eq!(atoi_of(&mut heap, "4294967297", 10), 1);
        let itoa_of = |heap: &mut Heap, value: i64, radix, width, signed| {
            let code = encode(ITOA, radix_parameter(radix, width, signed));
            let buffer = heap.call(code, value as u64, 0, 0);
            String::from_utf8(frozen(heap, buffer)).unwrap()
        };
        assert_eq!(itoa_of(&mut heap, -42, 10, 32, true), "-42");
        assert_eq!(itoa_of(&mut heap, -1, 10, 32, false), "4294967295");
        assert_eq!(itoa_of(&mut heap, -1, 16, 32, true), "ffffffff");
        assert_eq!(itoa_of(&mut heap, 0, 2, 32, true), "0");
        assert_eq!(
            itoa_of(&mut heap, i64::MIN, 10, 64, true),
            "-9223372036854775808"
        );
    }
}
