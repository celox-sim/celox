/// Diagnostics owned by the simulator facade and runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeDiagnostics {
    pub phase_timing: bool,
    pub optimizer_timing: bool,
    pub tick_timing_every: Option<u64>,
    pub testbench_progress_every: Option<u64>,
    pub address_map_filter: Option<String>,
}

#[cfg(feature = "host-runtime")]
mod host {
    use super::RuntimeDiagnostics;
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};

    /// Complete explicit diagnostics configuration for a simulator build.
    ///
    /// Library code does not install a `tracing` subscriber. Applications that
    /// want diagnostic events must install and configure one at their boundary.
    #[derive(Debug, Clone, Default)]
    pub struct DiagnosticsOptions {
        pub runtime: RuntimeDiagnostics,
        pub sir: crate::optimizer::SirDiagnostics,
        pub cranelift: crate::backend::CraneliftDiagnostics,
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub native: crate::backend::NativeDiagnostics,
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub native_tick_loop: Option<bool>,
    }

    impl DiagnosticsOptions {
        /// Read legacy `CELOX_*` switches once at the application boundary.
        ///
        /// Prefer constructing this type explicitly in reusable library code. This
        /// adapter exists for applications that retain the legacy environment-based
        /// workflow.
        #[allow(clippy::disallowed_methods)]
        pub fn from_env() -> Self {
            Self::from_env_iter(std::env::vars_os())
        }

        /// Parse diagnostics from an environment-like iterator without mutating
        /// process-global state.
        pub fn from_env_iter<K, V>(variables: impl IntoIterator<Item = (K, V)>) -> Self
        where
            K: Into<OsString>,
            V: Into<OsString>,
        {
            let variables = variables
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect::<HashMap<_, _>>();
            let enabled = |name: &str| {
                variables
                    .get(OsStr::new(name))
                    .is_some_and(|value| value != "0")
            };
            let unsigned = |name: &str| {
                variables
                    .get(OsStr::new(name))
                    .and_then(|value| value.to_str())
                    .and_then(|value| value.parse::<u64>().ok())
            };
            let usize_value =
                |name: &str| unsigned(name).and_then(|value| usize::try_from(value).ok());
            let string = |name: &str| {
                variables
                    .get(OsStr::new(name))
                    .map(|value| value.to_string_lossy().into_owned())
            };

            let phase_timing = enabled("CELOX_PHASE_TIMING");
            let pass_timing = enabled("CELOX_PASS_TIMING");
            let runtime = RuntimeDiagnostics {
                phase_timing,
                optimizer_timing: enabled("CELOX_OPT_TIMING"),
                tick_timing_every: unsigned("CELOX_TICK_TIMING"),
                testbench_progress_every: unsigned("CELOX_TESTBENCH_PROGRESS"),
                address_map_filter: enabled("CELOX_ADDR_MAP_DUMP")
                    .then(|| string("CELOX_ADDR_MAP_FILTER").unwrap_or_default()),
            };
            let sir = crate::optimizer::SirDiagnostics {
                pass_timing,
                branchify_stats: enabled("CELOX_BRANCHIFY_STATS"),
                mux_chain_stats: enabled("CELOX_MUX_CHAIN_STATS"),
                verify_boundaries: enabled("CELOX_SIR_VERIFY"),
                verify_passes: enabled("CELOX_SIR_VERIFY_PASSES"),
                branchify_verify: enabled("CELOX_BRANCHIFY_VERIFY"),
                branchify_trace_reg: usize_value("CELOX_BRANCHIFY_TRACE_REG"),
                effect_case_dispatch: enabled("CELOX_EFFECT_CASE_DISPATCH"),
            };
            let cranelift = crate::backend::CraneliftDiagnostics { pass_timing };
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let (native, native_tick_loop) = {
                let native_tick_loop = variables
                    .get(OsStr::new("CELOX_NATIVE_TICK_LOOP"))
                    .map(|value| value != "0");
                let dump = usize_value("CELOX_NATIVE_DUMP_BLOCK").map(|block| {
                    crate::backend::NativeDumpOptions {
                        block,
                        label: string("CELOX_NATIVE_DUMP_LABEL"),
                        stage: string("CELOX_NATIVE_DUMP_STAGE"),
                        dump_sir: variables
                            .get(OsStr::new("CELOX_NATIVE_DUMP_SIR"))
                            .is_none_or(|value| value != "0"),
                        mir_limit: usize_value("CELOX_NATIVE_DUMP_MIR_LIMIT").unwrap_or(64),
                    }
                });
                let native = crate::backend::NativeDiagnostics {
                    phase_timing,
                    regalloc_timing: enabled("CELOX_REGALLOC_TIMING"),
                    regalloc_stats: enabled("CELOX_REGALLOC_STATS"),
                    mir_stats: enabled("CELOX_MIR_STATS"),
                    mir_block_stats: enabled("CELOX_MIR_BLOCK_STATS"),
                    verify_sir: enabled("CELOX_SIR_VERIFY"),
                    verify_mir: enabled("CELOX_MIR_VERIFY"),
                    verify_mir_passes: enabled("CELOX_MIR_VERIFY_PASSES"),
                    verify_regalloc: enabled("CELOX_REGALLOC_VERIFY"),
                    isel_trace_regs: string("CELOX_ISEL_TRACE_REGS")
                        .or_else(|| string("CELOX_ISEL_TRACE_REG"))
                        .into_iter()
                        .flat_map(|value| {
                            value
                                .split(',')
                                .filter_map(|part| part.trim().parse::<usize>().ok())
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                    dump,
                    perf_map: enabled("CELOX_PERF_MAP"),
                };
                (native, native_tick_loop)
            };
            Self {
                runtime,
                sir,
                cranelift,
                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                native,
                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                native_tick_loop,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::DiagnosticsOptions;

        #[test]
        fn parses_supplied_environment_without_global_mutation() {
            let options = DiagnosticsOptions::from_env_iter([
                ("CELOX_PASS_TIMING", "1"),
                ("CELOX_BRANCHIFY_TRACE_REG", "42"),
                ("CELOX_TICK_TIMING", "100"),
                ("CELOX_REGALLOC_VERIFY", "1"),
            ]);
            assert!(options.sir.pass_timing);
            assert_eq!(options.sir.branchify_trace_reg, Some(42));
            assert_eq!(options.runtime.tick_timing_every, Some(100));
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            assert!(options.native.verify_regalloc);
        }

        #[test]
        fn zero_disables_flags_and_invalid_intervals_are_ignored() {
            let options = DiagnosticsOptions::from_env_iter([
                ("CELOX_PASS_TIMING", "0"),
                ("CELOX_TICK_TIMING", "not-a-number"),
            ]);
            assert!(!options.sir.pass_timing);
            assert_eq!(options.runtime.tick_timing_every, None);
        }
    }
}

#[cfg(feature = "host-runtime")]
pub use host::DiagnosticsOptions;
