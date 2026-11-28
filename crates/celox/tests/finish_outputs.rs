//! `Simulation::finish_outputs` does everything dropping a simulation does
//! outside its memory, so that a host ending the process may leak the
//! simulation instead: the files it leaves are the same either way.
use celox::{SimBackend, Simulation};
use std::path::Path;

/// A design that writes a file it never closes and a waveform it never
/// ends, with a `$dumpoff`/`$dumpon` span an FST file records only when the
/// writer finishes.
fn source(text: &Path, waveform: &Path) -> String {
    format!(
        r#"
        module Top(output logic [3:0] value);
            timeunit 1ns; timeprecision 1ns;
            integer file;
            initial begin
                file = $fopen("{}", "w");
                $dumpfile("{}");
                $dumpvars(0, Top.value);
                value = 4'b0001;
                $fdisplay(file, "value %b", value);
                #1 $dumpoff; value = 4'b0010;
                #1 $dumpon; value = 4'b0100;
                $fdisplay(file, "value %b", value);
                #1 value = 4'b1000;
            end
            final $fdisplay(file, "final %b", value);
        endmodule
    "#,
        text.display(),
        waveform.display()
    )
}

/// Run the design to its end, then drop it or finish its outputs and leak
/// it; the files it left.
fn outputs<B: SimBackend>(
    build: impl Fn(&str) -> Simulation<B>,
    dir: &Path,
    extension: &str,
    leak: bool,
) -> (Vec<u8>, Vec<u8>) {
    let text = dir.join(format!("{leak}.txt"));
    let waveform = dir.join(format!("{leak}.{extension}"));
    let source = source(&text, &waveform);
    let mut simulation = build(&source);
    simulation.run_until(10).unwrap();
    simulation.finish().unwrap();
    if leak {
        simulation.finish_outputs().unwrap();
        std::mem::forget(simulation);
    } else {
        drop(simulation);
    }
    (
        std::fs::read(&text).unwrap(),
        std::fs::read(&waveform).unwrap(),
    )
}

/// `bytes` with the digits of the date a waveform's header records,
/// `YYYY-MM-DD HH:MM:SS`, made zeros: two runs a second apart write other dates.
fn without_date(mut bytes: Vec<u8>) -> Vec<u8> {
    const SHAPE: &[u8] = b"0000-00-00 00:00:00";
    let matches = |window: &[u8]| {
        window.iter().zip(SHAPE).all(|(&byte, &shape)| {
            if shape == b'0' {
                byte.is_ascii_digit()
            } else {
                byte == shape
            }
        })
    };
    if let Some(start) = bytes.windows(SHAPE.len()).position(matches) {
        bytes[start..start + SHAPE.len()].copy_from_slice(SHAPE);
    }
    bytes
}

fn compare<B: SimBackend>(build: impl Fn(&str) -> Simulation<B>) {
    for extension in ["vcd", "fst"] {
        let dir = tempfile::tempdir().unwrap();
        let dropped = outputs(&build, dir.path(), extension, false);
        let leaked = outputs(&build, dir.path(), extension, true);
        assert!(
            !dropped.0.is_empty() && !dropped.1.is_empty(),
            "{extension}"
        );
        assert_eq!(dropped.0, leaked.0, "text file, {extension}");
        assert_eq!(
            without_date(dropped.1),
            without_date(leaked.1),
            "{extension} file"
        );
    }
}

fn builder(source: &str) -> celox::SimulatorBuilder<'_, Simulation> {
    Simulation::from_sv_sources(vec![(source, Path::new("finish.sv"))], "Top")
}

#[test]
fn interpreter_outputs_are_those_of_a_drop() {
    compare(|source| builder(source).build_interpreter().unwrap());
}

#[test]
fn native_outputs_are_those_of_a_drop() {
    compare(|source| builder(source).build_native().unwrap());
}

#[test]
fn tiered_outputs_are_those_of_a_drop() {
    compare(|source| builder(source).build_tiered().unwrap());
}
