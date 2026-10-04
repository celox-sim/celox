// Every group is a script file in the language of `crate::script`.

pub(super) struct Group {
    /// The file name, for diagnostics.
    pub file: &'static str,
    pub text: &'static str,
}

pub(super) const GROUPS: &[Group] = &[
    Group {
        file: "src/sv/cases/always_comb.vtest",
        text: include_str!("always_comb.vtest"),
    },
    Group {
        file: "src/sv/cases/generate.vtest",
        text: include_str!("generate.vtest"),
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
        file: "src/sv/cases/review_regressions.vtest",
        text: include_str!("review_regressions.vtest"),
    },
    Group {
        file: "src/sv/cases/synthesizable.vtest",
        text: include_str!("synthesizable.vtest"),
    },
    Group {
        file: "src/sv/cases/system_functions.vtest",
        text: include_str!("system_functions.vtest"),
    },
    Group {
        file: "src/sv/cases/types.vtest",
        text: include_str!("types.vtest"),
    },
];
