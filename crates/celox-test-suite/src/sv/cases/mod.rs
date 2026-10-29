// Every group is a script file in the language of `crate::script`.

pub(super) struct Group {
    /// The file name, for diagnostics.
    pub file: &'static str,
    pub text: &'static str,
}

pub(super) const GROUPS: &[Group] = &[
    Group {
        file: "src/sv/cases/concurrent_assertions.vtest",
        text: include_str!("concurrent_assertions.vtest"),
    },
    Group {
        file: "src/sv/cases/containers.vtest",
        text: include_str!("containers.vtest"),
    },
    Group {
        file: "src/sv/cases/array_locators.vtest",
        text: include_str!("array_locators.vtest"),
    },
    Group {
        file: "src/sv/cases/increment_decrement.vtest",
        text: include_str!("increment_decrement.vtest"),
    },
    Group {
        file: "src/sv/cases/streaming.vtest",
        text: include_str!("streaming.vtest"),
    },
    Group {
        file: "src/sv/cases/fork_deferred.vtest",
        text: include_str!("fork_deferred.vtest"),
    },
    Group {
        file: "src/sv/cases/events.vtest",
        text: include_str!("events.vtest"),
    },
    Group {
        file: "src/sv/cases/fork_control.vtest",
        text: include_str!("fork_control.vtest"),
    },
    Group {
        file: "src/sv/cases/fork.vtest",
        text: include_str!("fork.vtest"),
    },
    Group {
        file: "src/sv/cases/tagged_unions.vtest",
        text: include_str!("tagged_unions.vtest"),
    },
    Group {
        file: "src/sv/cases/container_formals.vtest",
        text: include_str!("container_formals.vtest"),
    },
    Group {
        file: "src/sv/cases/references.vtest",
        text: include_str!("references.vtest"),
    },
    Group {
        file: "src/sv/cases/unions.vtest",
        text: include_str!("unions.vtest"),
    },
    Group {
        file: "src/sv/cases/virtual_interfaces.vtest",
        text: include_str!("virtual_interfaces.vtest"),
    },
    Group {
        file: "src/sv/cases/real_extensions.vtest",
        text: include_str!("real_extensions.vtest"),
    },
    Group {
        file: "src/sv/cases/realtime.vtest",
        text: include_str!("realtime.vtest"),
    },
    Group {
        file: "src/sv/cases/real_math.vtest",
        text: include_str!("real_math.vtest"),
    },
    Group {
        file: "src/sv/cases/aggregate_regressions.vtest",
        text: include_str!("aggregate_regressions.vtest"),
    },
    Group {
        file: "src/sv/cases/floating_point.vtest",
        text: include_str!("floating_point.vtest"),
    },
    Group {
        file: "src/sv/cases/binary_file_io.vtest",
        text: include_str!("binary_file_io.vtest"),
    },
    Group {
        file: "src/sv/cases/string_format.vtest",
        text: include_str!("string_format.vtest"),
    },
    Group {
        file: "src/sv/cases/scan.vtest",
        text: include_str!("scan.vtest"),
    },
    Group {
        file: "src/sv/cases/distributions.vtest",
        text: include_str!("distributions.vtest"),
    },
    Group {
        file: "src/sv/cases/string.vtest",
        text: include_str!("string.vtest"),
    },
    Group {
        file: "src/sv/cases/class.vtest",
        text: include_str!("class.vtest"),
    },
    Group {
        file: "src/sv/cases/random_plusargs.vtest",
        text: include_str!("random_plusargs.vtest"),
    },
    Group {
        file: "src/sv/cases/program.vtest",
        text: include_str!("program.vtest"),
    },
    Group {
        file: "src/sv/cases/latch.vtest",
        text: include_str!("latch.vtest"),
    },
    Group {
        file: "src/sv/cases/always_comb.vtest",
        text: include_str!("always_comb.vtest"),
    },
    Group {
        file: "src/sv/cases/edges.vtest",
        text: include_str!("edges.vtest"),
    },
    Group {
        file: "src/sv/cases/generate.vtest",
        text: include_str!("generate.vtest"),
    },
    Group {
        file: "src/sv/cases/hierarchical_references.vtest",
        text: include_str!("hierarchical_references.vtest"),
    },
    Group {
        file: "src/sv/cases/hierarchy.vtest",
        text: include_str!("hierarchy.vtest"),
    },
    Group {
        file: "src/sv/cases/indexed_select.vtest",
        text: include_str!("indexed_select.vtest"),
    },
    Group {
        file: "src/sv/cases/interfaces.vtest",
        text: include_str!("interfaces.vtest"),
    },
    Group {
        file: "src/sv/cases/literals.vtest",
        text: include_str!("literals.vtest"),
    },
    Group {
        file: "src/sv/cases/operators.vtest",
        text: include_str!("operators.vtest"),
    },
    Group {
        file: "src/sv/cases/packed_structs.vtest",
        text: include_str!("packed_structs.vtest"),
    },
    Group {
        file: "src/sv/cases/unpacked_structs.vtest",
        text: include_str!("unpacked_structs.vtest"),
    },
    Group {
        file: "src/sv/cases/procedural.vtest",
        text: include_str!("procedural.vtest"),
    },
    Group {
        file: "src/sv/cases/review_regressions.vtest",
        text: include_str!("review_regressions.vtest"),
    },
    Group {
        file: "src/sv/cases/synthesizable.vtest",
        text: include_str!("synthesizable.vtest"),
    },
    Group {
        file: "src/sv/cases/file_core.vtest",
        text: include_str!("file_core.vtest"),
    },
    Group {
        file: "src/sv/cases/file_io.vtest",
        text: include_str!("file_io.vtest"),
    },
    Group {
        file: "src/sv/cases/system_functions.vtest",
        text: include_str!("system_functions.vtest"),
    },
    Group {
        file: "src/sv/cases/timing.vtest",
        text: include_str!("timing.vtest"),
    },
    Group {
        file: "src/sv/cases/types.vtest",
        text: include_str!("types.vtest"),
    },
];
