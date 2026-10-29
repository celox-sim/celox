//! Bit streams of streaming concatenations of dynamically sized data
//! (IEEE 1800-2023 11.4.14).
//!
//! A stream is a sequence of bits, each with an unknown flag, and a read
//! cursor. Packing appends the data of the stream expressions to a new
//! stream, re-orders it, and converts it to its target; unpacking re-orders
//! the source and takes the targets from the cursor. Like a string buffer, a
//! stream does not outlive the kernel that created it, and the operation
//! that converts it last frees it.

use crate::containers::{Container, Contents, Element, Layout};
use crate::{Fault, Handle, Heap};

/// `new()`: an empty stream.
pub const NEW: u64 = 112;
/// `bits(stream, value, mask)`: `stream` extended by the low `count` bits of
/// `value`, most significant first, where the parameter is `count`, at most
/// 64. A bit set in `mask` is unknown: X when its value bit is set, else Z.
pub const BITS: u64 = 113;
/// `string(stream, a)`: `stream` extended by the bytes of string `a`.
pub const STRING: u64 = 114;
/// `container(stream, a, range)`: `stream` extended by the elements of
/// container `a`, whose [`crate::containers::parameter`] is the parameter, in
/// order (an associative array in index order). `range` selects elements
/// by a [`range`] whose form the parameter's [`RANGE_FORM`] bits give when
/// its [`RANGED`] bit is set, or all of them; an element past the end streams the
/// default value of the element type. String elements stream their bytes.
pub const CONTAINER: u64 = 115;
/// `append(stream, other)`: `stream` extended by the unread bits of stream
/// `other`, which is freed.
pub const APPEND: u64 = 116;
/// `reorder(stream)`: the stream sliced into blocks of the parameter's size
/// from the right, in reverse order (11.4.14.2).
pub const REORDER: u64 = 117;
/// `unorder(stream)`: the stream that [`REORDER`] with the same parameter
/// turns into `stream`.
pub const UNORDER: u64 = 118;
/// `len(stream)`: the number of unread bits.
pub const LEN: u64 = 119;
/// `peek(stream, skip)`: the `count` unread bits after the first `skip`, most
/// significant first, as a value or (with the parameter's low bit) its
/// unknown mask; the parameter is `plane | count << 1`. Bits past the end
/// read as 0.
pub const PEEK: u64 = 120;
/// `skip(stream, count)`: `stream` with `count` more bits read. Reading past
/// the end is a fault: the targets need more bits than the source has.
pub const SKIP: u64 = 121;
/// `take_rest(stream, reserve)`: a new container of the parameter's layout
/// with the unread bits but the last `reserve`, in as few elements as hold
/// them, the last filled with zeros on the right; the bits are read.
pub const TAKE_REST: u64 = 122;
/// `take_range(stream, a, range)`: a copy of sequence `a`, long enough to
/// hold the elements that [`range`] selects, which are read from the stream;
/// the parameter's [`RANGE_FORM`] bits give the form of the range.
pub const TAKE_RANGE: u64 = 123;
/// `take_string(stream, reserve)`: a string of the unread bits but the last
/// `reserve`, the last byte filled with zeros on the right.
pub const TAKE_STRING: u64 = 124;
/// `free(stream)`.
pub const FREE: u64 = 125;
/// `check(stream)`: `stream`, recording a fault when it has more unread bits
/// than the width in the [`check_parameter`]: the fixed-size target is
/// narrower than the stream; or, for a bit-stream cast, other than that many
/// (IEEE 1800-2023 6.24.3).
pub const CHECK: u64 = 126;

/// `truncate(stream)`: `stream` without its unread bits past the first
/// parameter's number: a source wider than its fixed-size targets gives
/// them its leading bits (IEEE 1800-2023 11.4.14.3).
pub const TRUNCATE: u64 = 127;

/// The [`CHECK`] parameter.
pub const fn check_parameter(width: u64, exact: bool) -> u64 {
    width | (exact as u64) << 40
}

/// The [`PEEK`] parameter.
pub const fn peek_parameter(count: u32, mask: bool) -> u64 {
    mask as u64 | (count as u64) << 1
}

/// The `range` operand of a `with [first, second]` selection (IEEE
/// 1800-2023 11.4.14.4), each operand a signed 32-bit integer.
pub const fn range(first: u32, second: u32) -> u64 {
    first as u64 | (second as u64) << 32
}

/// The parameter bits of the form of a [`range`]: `[base +: count]` (0),
/// `[left : right]` (1) or `[base -: count]` (2).
pub const RANGE_FORM: u64 = 3 << 48;

/// The parameter bit of a [`CONTAINER`] item that selects elements by a
/// [`range`]; without it, `range` is unused.
pub const RANGED: u64 = 1 << 50;

/// The parameter `parameter` with range form `form`.
pub const fn range_parameter(parameter: u64, form: u64) -> u64 {
    parameter & !RANGE_FORM | form << 48 | RANGED
}

/// The first element and the number of elements that [`range`] `range`
/// selects in the form in `parameter`.
fn selection(parameter: u64, range: u64) -> (i64, i64) {
    let (first, second) = (
        range as u32 as i32 as i64,
        (range >> 32) as u32 as i32 as i64,
    );
    match (parameter & RANGE_FORM) >> 48 {
        1 => (first.min(second), (first - second).abs() + 1),
        2 => (first - second + 1, second.max(0)),
        _ => (first, second.max(0)),
    }
}

/// A value bit (bit 0) and an unknown bit (bit 1) per bit, most significant
/// first, and the number of bits read.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Stream {
    bits: Vec<u8>,
    cursor: usize,
}

impl Stream {
    fn push(&mut self, value: u64, mask: u64, count: u32) {
        for bit in (0..count).rev() {
            self.bits
                .push((value >> bit & 1) as u8 | ((mask >> bit & 1) as u8) << 1);
        }
    }

    fn unread(&self) -> &[u8] {
        &self.bits[self.cursor.min(self.bits.len())..]
    }

    /// Up to 64 bits from `start` of the unread bits as a value and a mask.
    fn read(&self, start: usize, count: usize) -> (u64, u64) {
        let unread = self.unread();
        (0..count).fold((0, 0), |(value, mask), index| {
            let bit = unread.get(start + index).copied().unwrap_or(0);
            (
                value << 1 | u64::from(bit & 1),
                mask << 1 | u64::from(bit >> 1),
            )
        })
    }

    /// The blocks of `size` bits sliced from the right of the unread bits,
    /// first block first.
    fn blocks(&self, size: usize) -> Vec<&[u8]> {
        self.unread().rchunks(size.max(1)).collect()
    }
}

impl Heap {
    fn new_stream(&mut self, stream: Stream) -> u64 {
        match self.free_streams.pop() {
            Some(id) => {
                self.streams[id as usize] = Some(stream);
                id
            }
            None => {
                self.streams.push(Some(stream));
                (self.streams.len() - 1) as u64
            }
        }
    }

    fn stream(&mut self, id: u64) -> &mut Stream {
        self.streams
            .get_mut(id as usize)
            .and_then(Option::as_mut)
            .unwrap_or_else(|| panic!("heap stream {id} is not live"))
    }

    fn take_stream(&mut self, id: u64) -> Stream {
        let stream = std::mem::take(self.stream(id));
        self.streams[id as usize] = None;
        self.free_streams.push(id);
        stream
    }

    fn stream_fault(&mut self, reason: &str) {
        self.fault
            .get_or_insert_with(|| Fault::Stream(reason.to_owned()));
    }

    /// The elements of a new sequence of `layout` holding `bits`, the last
    /// filled with zeros on the right.
    fn elements(layout: Layout, bits: &[u8]) -> Vec<Element> {
        let width = layout.width() as usize;
        bits.chunks(width)
            .map(|chunk| {
                let (value, mask) = chunk.iter().fold((0u64, 0u64), |(value, mask), &bit| {
                    (
                        value << 1 | u64::from(bit & 1),
                        mask << 1 | u64::from(bit >> 1),
                    )
                });
                let shift = width - chunk.len();
                // A 2-state element reads unknown bits as 0 (11.4.14.3).
                if layout.two_state() {
                    Element {
                        value: (value & !mask) << shift,
                        mask: 0,
                    }
                } else {
                    Element {
                        value: value << shift,
                        mask: mask << shift,
                    }
                }
            })
            .collect()
    }

    pub(crate) fn stream_call(&mut self, op: u64, parameter: u64, a: u64, b: u64, c: u64) -> u64 {
        match op {
            NEW => self.new_stream(Stream::default()),
            BITS => {
                let count = (parameter as u32).min(64);
                self.stream(a).push(b, c, count);
                a
            }
            STRING => {
                let bytes = self.string(b as Handle).to_vec();
                let stream = self.stream(a);
                for byte in bytes {
                    stream.push(byte.into(), 0, 8);
                }
                a
            }
            CONTAINER => {
                let layout = Layout(parameter);
                let elements: Vec<Element> = match self.containers.get(&(b as Handle)) {
                    Some(container) => match &container.contents {
                        Contents::Sequence(v) => v.iter().copied().collect(),
                        Contents::Associative(v) => v.values().copied().collect(),
                    },
                    None => Vec::new(),
                };
                let selected: Vec<Element> = if parameter & RANGED == 0 {
                    elements
                } else {
                    let (low, count) = selection(parameter, c);
                    (0..count)
                        .map(|offset| {
                            usize::try_from(low + offset)
                                .ok()
                                .and_then(|index| elements.get(index).copied())
                                .unwrap_or_else(|| layout.default())
                        })
                        .collect()
                };
                if layout.string_elements() {
                    let bytes: Vec<u8> = selected
                        .iter()
                        .flat_map(|element| self.string(element.value as Handle).to_vec())
                        .collect();
                    let stream = self.stream(a);
                    for byte in bytes {
                        stream.push(byte.into(), 0, 8);
                    }
                } else {
                    let width = layout.width();
                    let stream = self.stream(a);
                    for element in selected {
                        stream.push(element.value, element.mask, width);
                    }
                }
                a
            }
            APPEND => {
                let other = self.take_stream(b);
                let bits = other.unread().to_vec();
                self.stream(a).bits.extend(bits);
                a
            }
            REORDER | UNORDER => {
                let stream = self.stream(a);
                let size = parameter as usize;
                let bits: Vec<u8> = if op == REORDER {
                    stream.blocks(size).concat()
                } else {
                    // The blocks of the re-ordered stream, first block first,
                    // have the sizes of the blocks sliced from the right.
                    let lengths: Vec<usize> = stream.blocks(size).iter().map(|b| b.len()).collect();
                    let mut blocks = Vec::with_capacity(lengths.len());
                    let mut start = 0;
                    let unread = stream.unread();
                    for length in lengths {
                        blocks.push(&unread[start..start + length]);
                        start += length;
                    }
                    blocks.reverse();
                    blocks.concat()
                };
                *stream = Stream { bits, cursor: 0 };
                a
            }
            LEN => self.stream(a).unread().len() as u64,
            PEEK => {
                let count = ((parameter >> 1) as usize).min(64);
                let (value, mask) = self.stream(a).read(b as usize, count);
                if parameter & 1 == 0 { value } else { mask }
            }
            SKIP => {
                let stream = self.stream(a);
                let unread = stream.unread().len() as u64;
                stream.cursor += b.min(unread) as usize;
                if b > unread {
                    self.stream_fault("the stream has fewer bits than its targets");
                }
                a
            }
            CHECK => {
                let width = parameter & ((1 << 40) - 1);
                let unread = self.stream(a).unread().len() as u64;
                if unread > width {
                    self.stream_fault("the stream is wider than its fixed-size target");
                } else if parameter >> 40 & 1 != 0 && unread != width {
                    self.stream_fault("the stream and its cast type differ in size");
                }
                a
            }
            TAKE_REST | TAKE_STRING => {
                let stream = self.stream(a);
                let unread = stream.unread();
                let count = unread.len().saturating_sub(b as usize);
                let bits = unread[..count].to_vec();
                stream.cursor += count;
                if op == TAKE_STRING {
                    let bytes: Vec<u8> = bits
                        .chunks(8)
                        .map(|chunk| {
                            // String bytes are 2-state: unknown bits are 0.
                            chunk
                                .iter()
                                .fold(0u8, |byte, bit| byte << 1 | (bit & !(bit >> 1) & 1))
                                << (8 - chunk.len())
                        })
                        .collect();
                    return self.intern(&bytes).into();
                }
                let layout = Layout(parameter);
                let mut container = Container::empty(layout);
                if let Contents::Sequence(v) = &mut container.contents {
                    v.extend(Self::elements(layout, &bits));
                    // A bounded queue keeps the elements it can hold.
                    v.truncate(layout.capacity());
                }
                self.publish_container(container, true).into()
            }
            TAKE_RANGE => {
                let layout = Layout(parameter);
                let (low, count) = selection(parameter, c);
                let count = count as usize;
                let Ok(low) = usize::try_from(low) else {
                    self.stream_fault("a with range of a dynamic array is negative");
                    return b;
                };
                let width = layout.width() as usize;
                let stream = self.stream(a);
                let needed = count * width;
                let available = stream.unread().len().min(needed);
                let bits = stream.unread()[..available].to_vec();
                stream.cursor += available;
                if available < needed {
                    self.stream_fault("the stream has fewer bits than its targets");
                }
                let mut container = self
                    .containers
                    .get(&(b as Handle))
                    .map(|entry| (**entry).clone())
                    .unwrap_or_else(|| Container::empty(layout));
                if let Contents::Sequence(v) = &mut container.contents {
                    if v.len() < low + count {
                        v.resize(low + count, layout.default());
                    }
                    for (index, element) in Self::elements(layout, &bits).into_iter().enumerate() {
                        v[low + index] = element;
                    }
                    v.truncate(layout.capacity());
                }
                self.publish_container(container, true).into()
            }
            FREE => {
                self.take_stream(a);
                0
            }
            TRUNCATE => {
                let stream = self.stream(a);
                let end = stream.cursor.saturating_add(parameter as usize);
                stream.bits.truncate(end);
                a
            }
            other => panic!("unknown heap stream operation {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::parameter;
    use crate::ops::encode;

    /// A queue of 8-bit 2-state elements.
    const BYTES: u64 = parameter(8, true, 0, 1, 0, false, false, false);

    #[test]
    fn strings_and_containers_round_trip_through_a_stream() {
        let mut heap = Heap::new();
        let ab = heap.intern(b"ab") as u64;
        let stream = heap.call(NEW, 0, 0, 0);
        heap.call(STRING, stream, ab, 0);
        heap.call(encode(BITS, 8), stream, u64::from(b'c'), 0);
        assert_eq!(heap.call(LEN, stream, 0, 0), 24);
        let string = heap.call(TAKE_STRING, stream, 0, 0);
        assert_eq!(heap.string(string as Handle), b"abc");
        let stream = heap.call(NEW, 0, 0, 0);
        heap.call(encode(BITS, 16), stream, 0x1234, 0);
        let queue = heap.call(encode(TAKE_REST, BYTES), stream, 0, 0);
        let stream = heap.call(NEW, 0, 0, 0);
        heap.call(encode(CONTAINER, BYTES), stream, queue, 0);
        assert_eq!(
            heap.call(encode(PEEK, peek_parameter(16, false)), stream, 0, 0),
            0x1234
        );
    }

    #[test]
    fn unorder_undoes_reorder_with_a_partial_block() {
        let mut heap = Heap::new();
        let stream = heap.call(NEW, 0, 0, 0);
        heap.call(encode(BITS, 8), stream, 0b1101_0011, 0);
        heap.call(encode(REORDER, 3), stream, 0, 0);
        assert_eq!(
            heap.call(encode(PEEK, peek_parameter(8, false)), stream, 0, 0),
            0b0110_1011
        );
        heap.call(encode(UNORDER, 3), stream, 0, 0);
        assert_eq!(
            heap.call(encode(PEEK, peek_parameter(8, false)), stream, 0, 0),
            0b1101_0011
        );
    }

    #[test]
    fn strings_read_unknown_bits_as_zero() {
        let mut heap = Heap::new();
        let stream = heap.call(NEW, 0, 0, 0);
        // 8'b01x00001: X is a set value and unknown bit; Z a clear one.
        heap.call(encode(BITS, 8), stream, 0b0110_0001, 0b0010_0000);
        let string = heap.call(TAKE_STRING, stream, 0, 0);
        assert_eq!(heap.string(string as Handle), b"A");
    }

    #[test]
    fn explicit_ranges_select_even_when_all_ones() {
        let mut heap = Heap::new();
        let source = heap.call(NEW, 0, 0, 0);
        heap.call(encode(BITS, 16), source, 0x1234, 0);
        let queue = heap.call(encode(TAKE_REST, BYTES), source, 0, 0);
        let stream = heap.call(NEW, 0, 0, 0);
        let parameter = range_parameter(BYTES, 1);
        heap.call(
            encode(CONTAINER, parameter),
            stream,
            queue,
            range(u32::MAX, u32::MAX),
        );
        assert_eq!(heap.call(LEN, stream, 0, 0), 8);
        assert_eq!(
            heap.call(encode(PEEK, peek_parameter(8, false)), stream, 0, 0),
            0
        );
    }

    #[test]
    fn reading_past_the_end_and_a_narrow_target_fault() {
        let mut heap = Heap::new();
        let stream = heap.call(NEW, 0, 0, 0);
        heap.call(encode(BITS, 8), stream, 0xff, 0);
        heap.call(encode(CHECK, check_parameter(8, true)), stream, 0, 0);
        assert!(heap.fault.is_none());
        heap.call(encode(CHECK, check_parameter(4, false)), stream, 0, 0);
        assert!(matches!(heap.fault.take(), Some(Fault::Stream(_))));
        heap.call(SKIP, stream, 9, 0);
        assert!(matches!(heap.fault.take(), Some(Fault::Stream(_))));
    }
}
