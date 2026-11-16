use super::*;

#[test]
fn indexed_interface_paths_require_declaration_provenance() {
    let paths = [
        (
            "authored$bus$extra$width",
            ["authored$bus", "extra$width"],
            false,
        ),
        ("lanes$extra$width", ["lanes", "extra$width"], true),
        ("g[3].lanes$W", ["g[3].lanes", "W"], true),
    ]
    .into_iter()
    .map(|(key, segments, arrayed)| {
        (
            key.to_string(),
            sv::InterfaceMemberPath {
                segments: segments.into_iter().map(str::to_string).collect(),
                arrayed,
            },
        )
    })
    .collect();
    for (name, expected) in [
        ("authored$bus$extra$width", ["authored$bus", "extra$width"]),
        ("lanes[1][-2]$extra$width", ["lanes[1][-2]", "extra$width"]),
        ("g[3].lanes[7]$W", ["g[3].lanes[7]", "W"]),
    ] {
        assert_eq!(
            restore_interface_path(name, &paths),
            Some(expected.into_iter().map(str::to_string).collect()),
            "{name}"
        );
    }
    for name in [
        "lanes$not_declared",
        "lanes[foo]$extra$width",
        "authored$bus[0]$extra$width",
        "normal$parameter",
        "g[2].lanes[7]$W",
    ] {
        assert_eq!(restore_interface_path(name, &paths), None, "{name}");
    }
}
