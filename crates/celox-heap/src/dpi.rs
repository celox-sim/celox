//! The call area of DPI-C imports: the C values an imported function takes
//! or gives back through a pointer (IEEE 1800-2023 35.5.6, H.7).
//!
//! A call first sizes the area, stores the words its input and inout
//! arguments point at, copies its string arguments into C strings, and
//! then takes the address of the words. The pointers stay valid until the
//! next call sizes the area again, which is as long as the C function may
//! use them (H.10.1.2).

use super::*;
use std::ffi::{CStr, CString, c_char};

/// The storage one DPI-C call passes pointers into.
#[derive(Clone, Debug, Default)]
pub(crate) struct CallArea {
    /// Words pointed at by the output, inout and packed array arguments.
    words: Vec<u64>,
    /// The NUL-terminated copies of the string arguments.
    strings: Vec<CString>,
}

impl Heap {
    /// Perform DPI-C call area operation `op`, see [`ops::DPI_AREA`].
    pub(crate) fn dpi_call(&mut self, op: u64, a: u64, b: u64) -> u64 {
        match op {
            ops::DPI_AREA => {
                self.dpi.strings.clear();
                self.dpi.words.clear();
                self.dpi.words.resize(a as usize, 0);
                0
            }
            ops::DPI_POINTER => self.dpi.words.as_mut_ptr() as u64,
            ops::DPI_STORE => {
                if let Some(word) = self.dpi.words.get_mut(a as usize) {
                    *word = b;
                }
                0
            }
            ops::DPI_LOAD => self.dpi.words.get(a as usize).copied().unwrap_or(0),
            ops::DPI_C_STRING => {
                // Strings hold no NUL bytes.
                let string = CString::new(self.string(a as Handle)).unwrap_or_default();
                // The characters are on the C heap and do not move with the
                // vector that owns them.
                let pointer = string.as_ptr() as u64;
                self.dpi.strings.push(string);
                pointer
            }
            ops::DPI_FROM_C_STRING => {
                let pointer = a as *const c_char;
                if pointer.is_null() {
                    return EMPTY_STRING.into();
                }
                // SAFETY: the operand is the `const char *` a DPI-C function
                // gave as a `string` result or output. Whoever linked the
                // function vouched for its prototype, by which it is a
                // NUL-terminated string that is valid until the function is
                // called again (IEEE 1800-2023 35.5.6, H.10.1.2).
                let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes().to_vec();
                self.intern(&bytes).into()
            }
            _ => unreachable!("not a DPI-C call area operation: {op}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_area_holds_words_and_c_strings() {
        let mut heap = Heap::new();
        heap.call(ops::DPI_AREA, 2, 0, 0);
        heap.call(ops::DPI_STORE, 1, 0x1122_3344_5566_7788, 0);
        // A word outside the area is not stored.
        heap.call(ops::DPI_STORE, 2, 7, 0);
        let base = heap.call(ops::DPI_POINTER, 0, 0, 0) as *const u64;
        // SAFETY: the area holds two words until it is sized again.
        assert_eq!(unsafe { base.add(1).read() }, 0x1122_3344_5566_7788);
        assert_eq!(heap.call(ops::DPI_LOAD, 0, 0, 0), 0);
        assert_eq!(heap.call(ops::DPI_LOAD, 1, 0, 0), 0x1122_3344_5566_7788);
        assert_eq!(heap.call(ops::DPI_LOAD, 2, 0, 0), 0);

        let hello = heap.intern(b"hello");
        let first = heap.call(ops::DPI_C_STRING, hello.into(), 0, 0);
        let empty = heap.call(ops::DPI_C_STRING, EMPTY_STRING.into(), 0, 0);
        // Both copies stay valid together.
        assert_eq!(
            heap.call(ops::DPI_FROM_C_STRING, first, 0, 0),
            u64::from(hello)
        );
        assert_eq!(
            heap.call(ops::DPI_FROM_C_STRING, empty, 0, 0),
            u64::from(EMPTY_STRING)
        );
        assert_eq!(
            heap.call(ops::DPI_FROM_C_STRING, 0, 0, 0),
            u64::from(EMPTY_STRING)
        );
        // Sizing the area again clears it.
        heap.call(ops::DPI_AREA, 2, 0, 0);
        assert_eq!(heap.call(ops::DPI_LOAD, 1, 0, 0), 0);
    }
}
