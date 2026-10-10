use celox_test_suite::{load_group, no_std_library};

const SCRIPT: &str = r##"(group identity (category combinational))
(case basic (source "t.veryl" #"module Top { var a: logic; }"#) (top Top)
  (modify (set a 1))
  (for i (range 0 2) (do (if (== i 0) (assert_eq a 1) (assert_eq a i)))))"##;

fn identity(text: &str) -> String {
    load_group("identity.vtest", text, no_std_library)[0].script_identity()
}

#[test]
fn comments_spacing_and_case_locations_do_not_change_identity() {
    let original = identity(SCRIPT);
    let moved = format!(
        "; introductory comment\n\n{}",
        SCRIPT
            .replace("(modify", "; case note\n  (modify")
            .replace("(assert_eq", "\n (assert_eq")
    );
    assert_eq!(identity(&moved), original);
}

#[test]
fn identity_changes_for_source_stimulus_assertions_and_options() {
    let original = identity(SCRIPT);
    for changed in [
        SCRIPT.replace("var a: logic", "var a: logic<2>"),
        SCRIPT.replace("set a 1", "set a 2"),
        SCRIPT.replace("assert_eq a 1", "assert_eq a 2"),
        SCRIPT.replace("(top Top)", "(top Top) (four_state)"),
        SCRIPT.replace("(top Top)", "(top Other)"),
    ] {
        assert_ne!(identity(&changed), original);
    }
}
