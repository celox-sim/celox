//! Enumeration declarations that IEEE 1800-2023 6.19 forbids.
use super::*;

fn enumeration(declarations: &str) -> Result<Ir, AnalyzerError> {
    let source = format!("module Top; {declarations} initial $display(\"x\"); endmodule");
    analyze_source(&source, Path::new("enums.sv"))
}

#[test]
fn rejects_enumeration_bases_that_are_no_integer_types() {
    // A type name base denotes an integer atom type, without a packed
    // dimension of its own, or an integer vector type (Syntax 6-5, footnote
    // 19).
    for (declarations, detail) in [
        ("typedef logic T[]; enum T {A} e;", "an unpacked type"),
        ("typedef logic T[$]; enum T {A} e;", "an unpacked type"),
        ("typedef logic T[2]; enum T {A} e;", "an unpacked type"),
        ("typedef enum {X} T; enum T {A} e;", "an enumeration"),
        (
            "typedef int T; enum T [1:0] {A} e;",
            "more than one packed dimension",
        ),
        (
            "typedef bit [1:0][1:0] T; enum T {A} e;",
            "more than one packed dimension",
        ),
        ("typedef real T; enum T {A} e;", "a real type"),
        ("typedef string T; enum T {A} e;", "`string`"),
        (
            "typedef struct packed { int x; } T; enum T {A} e;",
            "a packed structure",
        ),
    ] {
        let error = enumeration(declarations).unwrap_err();
        assert!(
            matches!(error.source_error(), AnalyzerError::InvalidEnumeration(text) if text.contains(detail)),
            "{declarations}: {error}"
        );
    }
}

#[test]
fn accepts_enumeration_bases_that_name_integer_types() {
    for declarations in [
        "typedef int T; enum T {A, B} e;",
        "typedef int unsigned T; enum T {A, B} e;",
        "typedef bit [3:0] T; enum T {A, B} e;",
        "typedef logic T; enum T [3:0] {A, B} e;",
        "typedef logic [7:0] B; typedef B T; enum T {A, C} e;",
        "enum integer {A, B} e;",
        "enum logic [3:0] {A, B} e;",
    ] {
        enumeration(declarations).unwrap_or_else(|error| panic!("{declarations}: {error}"));
    }
}

#[test]
fn rejects_enumeration_values_that_are_not_unique_or_increment_the_maximum() {
    for (declarations, detail) in [
        ("enum logic {A, B = A} e;", "have the same value"),
        ("enum {red = 1, green, blue = 2} e;", "have the same value"),
        (
            "enum {first = 2, second = 1, third} e;",
            "have the same value",
        ),
        ("enum {a = 0, b = 7, c, d = 8} e;", "have the same value"),
        (
            "typedef enum logic {C, D = C} T; T e;",
            "have the same value",
        ),
        (
            "enum bit [1:0] {a = 3, b, c} e;",
            "increments the maximum value",
        ),
        (
            "enum bit [0:0] {a, b, c} e;",
            "increments the maximum value",
        ),
        ("enum byte {a = 127, b} e;", "increments the maximum value"),
    ] {
        let error = enumeration(declarations).unwrap_err();
        assert!(
            matches!(error.source_error(), AnalyzerError::InvalidEnumeration(text) if text.contains(detail)),
            "{declarations}: {error}"
        );
    }
}

#[test]
fn accepts_unique_enumeration_values() {
    for declarations in [
        "enum {bronze = 3, silver, gold} e;",
        "enum {a = 3, b = 7, c} e;",
        "enum {a, b = 7, c} e;",
        "enum bit [1:0] {a = 2, b} e;",
        "enum bit [3:0] {bronze = 'h3, silver, gold = 'h5} e;",
        "enum byte {a = -128, b, c = 127} e;",
        "enum integer {IDLE, XX = 'x, S1 = 'b01, S2 = 'b10} e;",
    ] {
        enumeration(declarations).unwrap_or_else(|error| panic!("{declarations}: {error}"));
    }
}
