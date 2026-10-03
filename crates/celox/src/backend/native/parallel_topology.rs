//! CPU admission and placement for the local parallel experiment.
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Placement {
    pub cpus: Vec<usize>,
    pinned: bool,
}

fn core_first(cpus: &[(usize, usize, usize)]) -> Vec<usize> {
    let mut cores = BTreeMap::<(usize, usize), Vec<usize>>::new();
    for &(cpu, socket, core) in cpus {
        cores.entry((socket, core)).or_default().push(cpu);
    }
    for siblings in cores.values_mut() {
        siblings.sort_unstable();
    }
    let mut result = Vec::new();
    for sibling in 0..cores.values().map(Vec::len).max().unwrap_or(0) {
        result.extend(cores.values().filter_map(|v| v.get(sibling)).copied());
    }
    result
}

fn parse_cpus(value: &str, allowed: &[usize]) -> Result<Vec<usize>, String> {
    let cpus = value
        .split(',')
        .map(|v| v.trim().parse::<usize>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "expected a comma-separated CPU list")?;
    if cpus.is_empty() || cpus.iter().copied().collect::<BTreeSet<_>>().len() != cpus.len() {
        return Err("CPU list must be nonempty and contain no duplicates".into());
    }
    if cpus.iter().any(|cpu| !allowed.contains(cpu)) {
        return Err("CPU list includes a CPU outside the inherited affinity mask".into());
    }
    Ok(cpus)
}

impl Placement {
    pub fn discover(config: &crate::diagnostics::ParallelEnvironment) -> Self {
        #[cfg(target_os = "linux")]
        let allowed = unsafe {
            let mut mask: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask),
                0
            );
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &mask))
                .collect::<Vec<_>>()
        };
        #[cfg(not(target_os = "linux"))]
        let allowed =
            (0..std::thread::available_parallelism().map_or(1, usize::from)).collect::<Vec<_>>();
        let pinned = config.get("CELOX_PARALLEL_PIN").is_some_and(|v| v != "0")
            || config.get("CELOX_PARALLEL_CPUS").is_some();
        let cpus = if let Some(value) = config.get("CELOX_PARALLEL_CPUS") {
            parse_cpus(value, &allowed).unwrap_or_else(|e| panic!("CELOX_PARALLEL_CPUS: {e}"))
        } else {
            let topology = allowed
                .iter()
                .map(|&cpu| {
                    let read = |field| {
                        std::fs::read_to_string(format!(
                            "/sys/devices/system/cpu/cpu{cpu}/topology/{field}"
                        ))
                        .ok()
                        .and_then(|s| s.trim().parse::<usize>().ok())
                    };
                    (
                        cpu,
                        read("physical_package_id").unwrap_or(0),
                        read("core_id").unwrap_or(cpu),
                    )
                })
                .collect::<Vec<_>>();
            core_first(&topology)
        };
        assert!(!cpus.is_empty());
        report!("CELOX_PARALLEL_CPUS pinned={pinned} cpus={cpus:?}");
        Self { cpus, pinned }
    }

    pub fn target(&self, worker: usize) -> Option<usize> {
        self.pinned.then_some(self.cpus[worker])
    }
}

pub(super) fn pin(cpu: Option<usize>) {
    let Some(cpu) = cpu else { return };
    #[cfg(target_os = "linux")]
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        assert!(cpu < libc::CPU_SETSIZE as usize);
        libc::CPU_SET(cpu, &mut set);
        assert_eq!(
            libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set),
            0,
            "cannot pin parallel worker to CPU {cpu}"
        );
        // Verify actual affinity rather than trusting only the request.
        let mut actual: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, std::mem::size_of_val(&actual), &mut actual),
            0
        );
        assert_eq!(libc::CPU_COUNT(&actual), 1);
        assert!(libc::CPU_ISSET(cpu, &actual));
    }
    #[cfg(not(target_os = "linux"))]
    panic!("parallel CPU pinning is Linux-only (requested CPU {cpu})");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_cores_before_smt_even_with_restricted_affinity() {
        assert_eq!(
            core_first(&[(3, 0, 1), (0, 0, 0), (2, 0, 1), (1, 0, 0)]),
            vec![0, 2, 1, 3]
        );
        assert_eq!(
            core_first(&[(1, 0, 0), (3, 0, 1), (7, 1, 0)]),
            vec![1, 3, 7]
        );
    }
    #[test]
    fn explicit_cpu_order_is_preserved_and_invalid_placement_rejected() {
        assert_eq!(parse_cpus("2,0", &[0, 2]).unwrap(), vec![2, 0]);
        for value in ["", "0,0", "0,16", "-1", "0,"] {
            assert!(parse_cpus(value, &[0, 2]).is_err(), "{value}");
        }
    }
}
