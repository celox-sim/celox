use super::*;

sv_backends! {
    fn sscanf_string_format(sim) { @case "scan::sscanf_string_format_expression"; }
    fn sscanf_discarded_return(sim) { @case "scan::sscanf_string_inputs_and_discarded_return"; }
    fn sscanf_real_fields(sim) { @case "scan::sscanf_real_fields_width_and_suppression"; }
    fn sscanf_fields(sim) { @case "scan::sscanf_integral_fields_and_failure_counts"; }
    fn sscanf_outputs(sim) { @case "scan::sscanf_multiple_outputs_and_packed_input"; }
    fn sscanf_unknowns(sim) { @case "scan::sscanf_radix_unknowns_and_unknown_input"; }
    fn sscanf_class_property_outputs(sim) { @case "scan::sscanf_class_property_outputs"; }
    fn sscanf_inputs_before_property_outputs(sim) { @case "scan::sscanf_inputs_before_property_outputs"; }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn sscanf_strings_native_image() {
    for case in [
        "scan::sscanf_string_inputs_and_discarded_return",
        "scan::sscanf_string_format_expression",
    ] {
        crate::suite::run_case(case, "native-image");
    }
}

/// `$sscanf` assigns as many outputs as it returns, in order (IEEE 1800-2023
/// 21.3.4.3): a later output that names the same class property and is not
/// assigned leaves the value of the earlier one. Simulators differ on what a
/// conversion that does not match writes, so this is not a shared case.
#[test]
fn sscanf_unassigned_property_outputs_keep_earlier_values() {
    let source = r#"
    module Top(output int offset, output int count);
        class Setting; int offset = 7; endclass
        Setting setting;
        initial begin
            setting = new;
            count = $sscanf("42 q", "%d %d", setting.offset, setting.offset);
            offset = setting.offset;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let offset = sim.signal("offset");
    let count = sim.signal("count");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(count), BigUint::from(1u8));
    assert_eq!(sim.get(offset), BigUint::from(42u8));
}

/// The object of a scanned class property is the one its selector names
/// before another output of the call changes the selector, as for any
/// output (IEEE 1800-2023 21.3.4.3). xezim 0.11.4 writes the object the
/// changed selector names, so this is not a shared case.
#[test]
fn sscanf_property_outputs_take_their_objects_before_the_call() {
    let source = r#"
    module Top(output int first, output int second);
        class Setting; int offset = 7; endclass
        Setting settings[2];
        int i;
        initial begin
            settings[0] = new;
            settings[1] = new;
            i = 0;
            void'($sscanf("1 42", "%d %d", i, settings[i].offset));
            first = settings[0].offset;
            second = settings[1].offset;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let first = sim.signal("first");
    let second = sim.signal("second");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(first), BigUint::from(42u8));
    assert_eq!(sim.get(second), BigUint::from(7u8));
}

/// A scan evaluates its text before its format, and both before its
/// outputs (IEEE 1800-2023 21.3.4.3): a format that changes the text does
/// not change the text scanned, and one that changes what selects an output
/// changes the output written. Verilator 5.052 requires a string literal
/// format, so this is not a shared case.
#[test]
fn sscanf_formats_are_evaluated_after_the_text() {
    let source = r#"
    module Top(output int offset, output int level, output int count);
        class Setting; int offset = 7; int level = 3; endclass
        Setting settings[2];
        int i;
        string text;
        function automatic string next_format();
            text = "99 6";
            i = 1;
            return "%d %d";
        endfunction
        initial begin
            settings[0] = new;
            settings[1] = new;
            text = "42 5";
            i = 0;
            count = $sscanf(text, next_format(), settings[0].offset, settings[i].level);
            offset = settings[0].offset;
            level = settings[1].level;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let signals = ["offset", "level", "count"].map(|name| sim.signal(name));
    sim.run_until(0).unwrap();
    let values = signals.map(|signal| sim.get(signal));
    assert_eq!(values, [42u8, 5, 2].map(BigUint::from));
}

/// A real or shortreal class property, and an active member of a tagged
/// union in a packed structure property, are scan outputs as variables of
/// their types are (IEEE 1800-2023 21.3.4.3); an inactive member stops the
/// run (7.3.2).
#[test]
fn sscanf_outputs_of_real_and_tagged_member_properties() {
    let source = r#"
    module Top(output bit pass, output int count);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        class Setting; real value; shortreal ratio; record_t record; endclass
        Setting setting;
        initial begin
            setting = new;
            setting.record.item = tagged Full 8'h00;
            count = $sscanf("1.25 0.5 42", "%f %f %d", setting.value, setting.ratio,
                            setting.record.item.Full);
            pass = setting.value == 1.25 && setting.ratio == 0.5
                && setting.record.item.Full == 42;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let pass = sim.signal("pass");
    let count = sim.signal("count");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(count), BigUint::from(3u8));
    assert_eq!(sim.get(pass), BigUint::from(1u8));
    let inactive = r#"
    module Top;
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        class Setting; record_t record; endclass
        initial begin
            automatic Setting setting = new;
            setting.record.item = tagged Empty;
            void'($sscanf("42", "%d", setting.record.item.Full));
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(inactive, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(0).unwrap_err().to_string();
    assert!(error.contains("active tag"), "{error}");
}

/// The checks of a scanned place read the value at the place the scan
/// captured, before an earlier output changed what selects it (IEEE
/// 1800-2023 7.3.2, 21.3.4.3): `settings[i]` is `settings[0]`, whose member
/// is active, although the scan sets `i` to 1, whose member is not.
#[test]
fn sscanf_place_checks_read_the_captured_place() {
    let source = r#"
    module Top(output bit [7:0] full, output int i);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        class Setting; record_t record; endclass
        Setting settings[2];
        initial begin
            settings[0] = new;
            settings[1] = new;
            settings[0].record.item = tagged Full 8'h00;
            settings[1].record.item = tagged Empty;
            i = 0;
            void'($sscanf("1 42", "%d %d", i, settings[i].record.item.Full));
            full = settings[0].record.item.Full;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build()
        .unwrap();
    let full = sim.signal("full");
    let i = sim.signal("i");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(full), BigUint::from(42u8));
    assert_eq!(sim.get(i), BigUint::from(1u8));
}
