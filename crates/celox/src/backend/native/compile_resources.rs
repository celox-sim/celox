//! Choose compiler concurrency from currently available host resources.

const MAX_WORKERS: usize = 4;

pub(super) fn workers(blocks: usize, instructions: usize, tasks: usize) -> usize {
    choose_workers(
        blocks,
        instructions,
        tasks,
        std::thread::available_parallelism().map_or(1, usize::from),
        available_memory(),
    )
}

fn choose_workers(
    blocks: usize,
    instructions: usize,
    tasks: usize,
    cpus: usize,
    available: Option<u64>,
) -> usize {
    let maximum = MAX_WORKERS.min(cpus.max(1)).min(tasks.max(1));
    let Some(available) = available else {
        // Retain the conservative large-design fallback when the OS cannot
        // report a usable budget; small designs keep their usual parallelism.
        return if blocks >= 65_536 { 1 } else { maximum };
    };
    // Budget the additional SIR/MIR copies and allocator analyses: the shared
    // input is already deducted from available memory. Three overlapping
    // eight-hart functions peaked at 6.54 GiB for the entire process tree.
    // Account for straight-line designs too, not only CFG size.
    // These are conservative estimates, not reservations or memory limits.
    let per_function = (blocks as u64)
        .saturating_mul(24 * 1024)
        .max((instructions as u64).saturating_mul(4096))
        .max(128 * 1024 * 1024);
    // Leave room for the running simulator and unrelated host activity.
    let budget = available - available / 5;
    (budget / per_function).min(maximum as u64).max(1) as usize
}

#[cfg(not(target_os = "linux"))]
fn available_memory() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let host = meminfo.lines().find_map(|line| {
        let value = line.strip_prefix("MemAvailable:")?;
        value
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })?;
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (root, mount) = mounts.lines().find_map(|line| {
        let (fields, kind) = line.split_once(" - ")?;
        if kind.split_whitespace().next()? != "cgroup2" {
            return None;
        }
        let mut fields = fields.split_whitespace();
        Some((
            unescape_mount(fields.nth(3)?),
            unescape_mount(fields.next()?),
        ))
    })?;
    let relative = std::path::Path::new(path).strip_prefix(&root).ok()?;
    let mount = std::path::PathBuf::from(mount);
    let leaf = mount.join(relative);
    // A cgroup namespace may hide ancestors, but its mounted root still
    // contributes a limit. Walk every visible ancestor, not just the leaf.
    cgroup_available(host, &mount, &leaf)
}

#[cfg(target_os = "linux")]
fn unescape_mount(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
fn cgroup_available(host: u64, mount: &std::path::Path, leaf: &std::path::Path) -> Option<u64> {
    if !leaf.is_dir() || !leaf.starts_with(mount) {
        return None;
    }
    let mut available = host;
    for directory in leaf.ancestors().take_while(|path| path.starts_with(mount)) {
        match std::fs::read_to_string(directory.join("memory.max")) {
            Ok(limit) if limit.trim() == "max" => {}
            Ok(limit) => {
                let limit = limit.trim().parse::<u64>().ok()?;
                let current = std::fs::read_to_string(directory.join("memory.current")).ok()?;
                let current = current.trim().parse::<u64>().ok()?;
                available = available.min(limit.saturating_sub(current));
            }
            // The root of the host cgroup hierarchy has no memory.max.
            Err(error) if directory == mount && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn large_design_uses_host_memory_instead_of_always_serializing() {
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(38 * GIB)), 4);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(5 * GIB)), 1);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(10 * GIB)), 2);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 2, Some(11 * GIB)), 2);
    }

    #[test]
    fn respects_cpu_task_and_straight_line_memory_limits() {
        assert_eq!(choose_workers(1, 1, 5, 1, Some(38 * GIB)), 1);
        assert_eq!(choose_workers(1, 1, 2, 16, Some(38 * GIB)), 2);
        assert_eq!(choose_workers(1, 2_000_000, 5, 16, Some(5 * GIB)), 1);
        assert_eq!(choose_workers(1, 1, 5, 16, Some(0)), 1);
        assert_eq!(
            choose_workers(usize::MAX, usize::MAX, 5, 16, Some(u64::MAX)),
            1
        );
    }

    #[test]
    fn missing_memory_information_preserves_conservative_fallback() {
        assert_eq!(choose_workers(120_000, 1, 5, 16, None), 1);
        assert_eq!(choose_workers(100, 1000, 5, 16, None), 4);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn accounts_for_parent_usage_below_an_unlimited_child() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("memory.max"), (8 * GIB).to_string()).unwrap();
        std::fs::write(parent.join("memory.current"), (3 * GIB).to_string()).unwrap();
        std::fs::write(child.join("memory.max"), "max\n").unwrap();
        assert_eq!(
            cgroup_available(38 * GIB, temp.path(), &child),
            Some(5 * GIB)
        );
        assert_eq!(cgroup_available(GIB, temp.path(), &child), Some(GIB));
        std::fs::write(parent.join("memory.current"), (9 * GIB).to_string()).unwrap();
        assert_eq!(cgroup_available(38 * GIB, temp.path(), &child), Some(0));
        std::fs::remove_file(parent.join("memory.current")).unwrap();
        assert_eq!(cgroup_available(38 * GIB, temp.path(), &child), None);
        assert_eq!(
            cgroup_available(GIB, temp.path(), &child.join("missing")),
            None
        );
    }
}
