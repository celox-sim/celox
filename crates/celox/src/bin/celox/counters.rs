//! Hardware counters of the execute phase of a run (`--report`): the user
//! instructions and cycles of the calling thread, which leave out compilation
//! and process start-up, unlike `perf stat` on the whole command.

/// Running counters, or nothing where the system does not grant them.
pub(super) struct ExecuteCounters {
    #[cfg(target_os = "linux")]
    events: [libc::c_int; 2],
}

/// What [`ExecuteCounters`] counted.
pub(super) struct Counted {
    pub instructions: u64,
    pub cycles: u64,
}

#[cfg(target_os = "linux")]
mod linux {
    /// The leading fields of `struct perf_event_attr`
    /// (`PERF_ATTR_SIZE_VER0`, 64 bytes), which suffice for counting.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct Attr {
        pub kind: u32,
        pub size: u32,
        pub config: u64,
        pub sample_period: u64,
        pub sample_type: u64,
        pub read_format: u64,
        pub flags: u64,
        pub wakeup_events: u32,
        pub bp_type: u32,
        pub config1: u64,
    }

    pub(super) const TYPE_HARDWARE: u32 = 0;
    pub(super) const CPU_CYCLES: u64 = 0;
    pub(super) const INSTRUCTIONS: u64 = 1;
    /// `disabled`, `exclude_kernel` and `exclude_hv`.
    pub(super) const FLAGS: u64 = 1 | (1 << 5) | (1 << 6);
    pub(super) const IOC_ENABLE: libc::c_ulong = 0x2400;
    pub(super) const IOC_DISABLE: libc::c_ulong = 0x2401;
    pub(super) const IOC_FLAG_GROUP: libc::c_ulong = 1;

    pub(super) fn open(config: u64, group: libc::c_int) -> Option<libc::c_int> {
        let attr = Attr {
            kind: TYPE_HARDWARE,
            size: std::mem::size_of::<Attr>() as u32,
            config,
            flags: FLAGS,
            ..Default::default()
        };
        // SAFETY: `attr` is a valid `perf_event_attr` of the size it
        // declares; the call only reads it.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const Attr,
                0 as libc::pid_t,
                -1 as libc::c_int,
                group,
                0 as libc::c_ulong,
            )
        };
        (fd >= 0).then_some(fd as libc::c_int)
    }

    pub(super) fn read(fd: libc::c_int) -> Option<u64> {
        let mut value = 0u64;
        // SAFETY: the buffer is 8 writable bytes, the size of a counter
        // read without a read format.
        let read = unsafe { libc::read(fd, (&mut value as *mut u64).cast(), 8) };
        (read == 8).then_some(value)
    }
}

impl ExecuteCounters {
    /// Start counting, when the kernel lets this process count itself.
    pub(super) fn start() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let leader = linux::open(linux::INSTRUCTIONS, -1)?;
            let Some(cycles) = linux::open(linux::CPU_CYCLES, leader) else {
                // SAFETY: `leader` is a descriptor this function opened.
                unsafe { libc::close(leader) };
                return None;
            };
            let counters = Self {
                events: [leader, cycles],
            };
            // SAFETY: an ioctl of a perf event descriptor this owns.
            let enabled =
                unsafe { libc::ioctl(leader, linux::IOC_ENABLE, linux::IOC_FLAG_GROUP) } == 0;
            enabled.then_some(counters)
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// Stop counting and read the counts.
    pub(super) fn stop(self) -> Option<Counted> {
        #[cfg(target_os = "linux")]
        {
            let [leader, cycles] = self.events;
            // SAFETY: an ioctl of a perf event descriptor this owns.
            unsafe { libc::ioctl(leader, linux::IOC_DISABLE, linux::IOC_FLAG_GROUP) };
            Some(Counted {
                instructions: linux::read(leader)?,
                cycles: linux::read(cycles)?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[cfg(target_os = "linux")]
impl Drop for ExecuteCounters {
    fn drop(&mut self) {
        for fd in self.events {
            // SAFETY: the descriptors are this object's own.
            unsafe { libc::close(fd) };
        }
    }
}
