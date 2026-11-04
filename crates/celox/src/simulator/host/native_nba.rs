//! Stable pages for native NBA records. Generated code owns their free lists,
//! FIFO and delayed-update heap; Rust only supplies additional storage.
use crate::RuntimeErrorCode;

pub(crate) const CLASSES: usize = usize::BITS as usize;
pub(crate) const RECORD_HEADER_WORDS: usize = 8;

#[repr(C)]
pub(crate) struct Header {
    pub free: [u64; CLASSES],
    pub head: u64,
    pub tail: u64,
    pub sequence: u64,
    pub delayed: *mut u64,
    pub delayed_len: u64,
    pub delayed_capacity: u64,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            free: [0; CLASSES],
            head: 0,
            tail: 0,
            sequence: 0,
            delayed: std::ptr::null_mut(),
            delayed_len: 0,
            delayed_capacity: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct Pool {
    pages: Vec<Box<[u64]>>,
    delayed: Vec<u64>,
}

impl Pool {
    /// Materialize portable queued state at entry to generated code. Only this
    /// cold boundary uses Rust; publication and steady allocation stay native.
    pub fn import(&mut self, header: &mut Header, fields: &[u64]) -> Result<u64, RuntimeErrorCode> {
        let words = fields
            .len()
            .max(RECORD_HEADER_WORDS)
            .checked_next_power_of_two()
            .ok_or_else(Self::fault)?;
        let class = words.trailing_zeros() as usize;
        if header.free[class] == 0 {
            self.reserve_class(header, class as u64)?;
        }
        let pointer = header.free[class];
        let next = self.record(pointer, words).ok_or_else(Self::fault)?[0];
        header.free[class] = next;
        // SAFETY: record validated this exclusively borrowed pool's stable
        // allocation. The class-sized block has room for every imported word.
        unsafe {
            let dst = pointer as *mut u64;
            std::ptr::write_bytes(dst, 0, words);
            std::ptr::copy_nonoverlapping(fields.as_ptr(), dst, fields.len());
            dst.add(5).write(class as u64);
        }
        Ok(pointer)
    }

    pub fn append_imported(
        &mut self,
        header: &mut Header,
        pointer: u64,
    ) -> Result<(), RuntimeErrorCode> {
        self.record(pointer, RECORD_HEADER_WORDS)
            .ok_or_else(Self::fault)?;
        if header.tail == 0 {
            header.head = pointer;
        } else {
            self.record(header.tail, RECORD_HEADER_WORDS)
                .ok_or_else(Self::fault)?;
            // SAFETY: validated owned records, exclusively borrowed at entry.
            unsafe {
                (header.tail as *mut u64).write(pointer);
            }
        }
        header.tail = pointer;
        Ok(())
    }

    pub fn append_delayed(
        &mut self,
        header: &mut Header,
        pointer: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let len = usize::try_from(header.delayed_len).map_err(|_| Self::fault())?;
        *self.delayed.get_mut(len).ok_or_else(Self::fault)? = pointer;
        header.delayed_len += 1;
        Ok(())
    }
    pub fn delayed_records(&self, header: &Header) -> Result<&[u64], RuntimeErrorCode> {
        let len = usize::try_from(header.delayed_len).map_err(|_| Self::fault())?;
        self.delayed.get(..len).ok_or_else(Self::fault)
    }

    fn fault() -> RuntimeErrorCode {
        RuntimeErrorCode::Runtime {
            message: "native NBA queue allocation failed".into(),
            signals: Vec::new(),
        }
    }

    pub fn reserve_class(
        &mut self,
        header: &mut Header,
        class: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let class = usize::try_from(class).map_err(|_| Self::fault())?;
        let words = 1usize
            .checked_shl(u32::try_from(class).map_err(|_| Self::fault())?)
            .filter(|&words| words >= RECORD_HEADER_WORDS)
            .ok_or_else(Self::fault)?;
        let blocks = (4096 / words).clamp(1, 64);
        let len = words.checked_mul(blocks).ok_or_else(Self::fault)?;
        self.pages.try_reserve(1).map_err(|_| Self::fault())?;
        let mut page = Vec::new();
        page.try_reserve_exact(len).map_err(|_| Self::fault())?;
        page.resize(len, 0);
        // Box before taking addresses: shrinking a Vec may move its allocation.
        let mut page = page.into_boxed_slice();
        let base = page.as_mut_ptr();
        let mut next = header.free[class];
        for block in (0..blocks).rev() {
            let index = block * words;
            page[index] = next;
            // SAFETY: every block begins inside this owned, stable allocation.
            next = unsafe { base.add(index) } as u64;
        }
        self.pages.push(page);
        header.free[class] = next;
        Ok(())
    }

    pub fn reserve_delayed(
        &mut self,
        header: &mut Header,
        required: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let required = usize::try_from(required).map_err(|_| Self::fault())?;
        if required > self.delayed.len() {
            let capacity = required.max(self.delayed.len().saturating_mul(2)).max(64);
            self.delayed
                .try_reserve(capacity - self.delayed.len())
                .map_err(|_| Self::fault())?;
            self.delayed.resize(capacity, 0);
        }
        header.delayed = self.delayed.as_mut_ptr();
        header.delayed_capacity = self.delayed.len() as u64;
        Ok(())
    }

    /// Return a record marshalled at a host boundary to its native free list.
    pub fn release(
        &mut self,
        header: &mut Header,
        pointer: u64,
        class: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let class = usize::try_from(class).map_err(|_| Self::fault())?;
        let words = 1usize
            .checked_shl(u32::try_from(class).map_err(|_| Self::fault())?)
            .filter(|&words| words >= RECORD_HEADER_WORDS)
            .ok_or_else(Self::fault)?;
        self.record(pointer, words).ok_or_else(Self::fault)?;
        // SAFETY: record validated alignment and that the entire block belongs
        // to this exclusively borrowed pool. Its boxes cannot move.
        unsafe {
            (pointer as *mut u64).write(header.free[class]);
        }
        header.free[class] = pointer;
        Ok(())
    }

    /// Checked host-boundary access when exporting queued values for a snapshot.
    /// No scheduling decision or queue mutation happens here.
    pub fn record(&self, pointer: u64, words: usize) -> Option<&[u64]> {
        let pointer = usize::try_from(pointer).ok()?;
        if !pointer.is_multiple_of(std::mem::size_of::<u64>()) {
            return None;
        }
        for page in &self.pages {
            let start = page.as_ptr() as usize;
            let Some(bytes) = pointer.checked_sub(start) else {
                continue;
            };
            let offset = bytes / std::mem::size_of::<u64>();
            let Some(end) = offset.checked_add(words) else {
                continue;
            };
            if let Some(slice) = page.get(offset..end) {
                return Some(slice);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_keeps_live_record_pointers_and_free_lists() {
        let mut pool = Pool::default();
        let mut header = Header::default();
        pool.reserve_class(&mut header, 4).unwrap();
        let first = header.free[4];
        // SAFETY: reserve_class creates a writable 16-word record at this head.
        let next = unsafe { (first as *mut u64).read() };
        header.free[4] = next;
        unsafe {
            (first as *mut u64).add(8).write(0xdead_beef);
        }
        for class in [4, 4, 7, 12, 4] {
            pool.reserve_class(&mut header, class).unwrap();
        }
        assert_eq!(pool.record(first, 16).unwrap()[8], 0xdead_beef);
        let mut pointer = header.free[4];
        let mut visited = crate::HashSet::default();
        while pointer != 0 {
            assert_ne!(pointer, first, "live record must not rejoin the free list");
            assert!(visited.insert(pointer), "free list must remain acyclic");
            pointer = pool.record(pointer, 16).unwrap()[0];
        }
        assert_eq!(visited.len(), 4 * 64 - 1);
        assert!(pool.record(first + 1, 1).is_none());
        assert!(pool.record(first, usize::MAX).is_none());
        assert!(pool.reserve_class(&mut header, 2).is_err());
        assert!(pool.reserve_class(&mut header, u64::MAX).is_err());
    }

    #[test]
    fn imported_records_copy_portable_values_and_keep_queue_order() {
        let mut pool = Pool::default();
        let mut header = Header::default();
        let mut fields = vec![0; 12];
        fields[2] = 257;
        fields[3] = 63;
        fields[4] = 73;
        fields[7] = 1;
        fields[8] = 0x1234;
        fields[10] = u64::MAX;
        let first = pool.import(&mut header, &fields).unwrap();
        pool.append_imported(&mut header, first).unwrap();
        fields[8] = 0x5678;
        let second = pool.import(&mut header, &fields).unwrap();
        pool.append_imported(&mut header, second).unwrap();
        fields.fill(0);
        assert_eq!(header.head, first);
        assert_eq!(header.tail, second);
        assert_eq!(pool.record(first, 12).unwrap()[0], second);
        assert_eq!(pool.record(first, 12).unwrap()[8], 0x1234);
        assert_eq!(pool.record(second, 12).unwrap()[8], 0x5678);
        assert_eq!(pool.record(first, 12).unwrap()[10], u64::MAX);
        assert_eq!(pool.record(first, 12).unwrap()[5], 4);
        pool.reserve_delayed(&mut header, 2).unwrap();
        pool.append_delayed(&mut header, second).unwrap();
        pool.append_delayed(&mut header, first).unwrap();
        assert_eq!(pool.delayed_records(&header).unwrap(), &[second, first]);
    }

    #[test]
    fn heap_growth_preserves_pending_record_addresses() {
        let mut pool = Pool::default();
        let mut header = Header::default();
        pool.reserve_delayed(&mut header, 3).unwrap();
        let first = header.delayed;
        unsafe {
            first.add(2).write(0x1234_5678);
        }
        pool.reserve_delayed(&mut header, 2).unwrap();
        assert_eq!(header.delayed, first);
        pool.reserve_delayed(&mut header, 300).unwrap();
        assert_eq!(unsafe { header.delayed.add(2).read() }, 0x1234_5678);
        assert!(header.delayed_capacity >= 300);
        assert!(pool.reserve_delayed(&mut header, u64::MAX).is_err());
    }
}
