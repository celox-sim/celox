use std::path::Path;

use celox_frontend_veryl::{FileDependency, capture_file_dependencies};

fn memory_source(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "\\\\");
    format!(
        r#"module Memory (y: output logic<8>) {{
            #[allow(initial_assign)]
            var mem: logic<8>[2];
            initial {{ $readmemh("{path}", mem); }}
            assign y = mem[0];
        }}"#
    )
}

fn assert_memory_is_tracked(dependencies: &[FileDependency], path: &Path) {
    assert!(
        dependencies
            .iter()
            .any(|dependency| dependency.path == path),
        "readmem file was not returned to the caller: {dependencies:?}"
    );
}

#[test]
fn direct_veryl_compile_returns_readmem_dependencies_to_its_caller() {
    let directory = tempfile::tempdir().unwrap();
    let memory = directory.path().join("memory.hex");
    std::fs::write(&memory, "12\n34\n").unwrap();
    let source = memory_source(&memory);
    let (_, dependencies) = capture_file_dependencies(|collector| {
        celox::compile_to_sir_with_file_dependencies(
            &[(&source, Path::new("memory.veryl"))],
            "Memory",
            &[],
            &[],
            false,
            &celox::TraceOptions::default(),
            None,
            None,
            None,
            None,
            &[],
            &celox::OptimizeOptions::default(),
            collector,
        )
        .unwrap()
    });
    assert_memory_is_tracked(&dependencies, &memory);
}

#[cfg(feature = "systemverilog")]
#[test]
fn direct_mixed_compile_returns_veryl_child_readmem_dependencies_to_its_caller() {
    let directory = tempfile::tempdir().unwrap();
    let memory = directory.path().join("memory.hex");
    std::fs::write(&memory, "56\n78\n").unwrap();
    let source = memory_source(&memory);
    let sv = "module Top(output logic [7:0] y); prj_Memory memory(.y(y)); endmodule";
    let (_, dependencies) = capture_file_dependencies(|collector| {
        celox::compile_mixed_to_sir_with_file_dependencies(
            &[(&source, Path::new("memory.veryl"))],
            &[(sv, Path::new("top.sv"))],
            "Top",
            &[],
            &[],
            false,
            &celox::TraceOptions::default(),
            None,
            None,
            None,
            None,
            &[],
            &celox::OptimizeOptions::default(),
            collector,
        )
        .unwrap()
    });
    assert_memory_is_tracked(&dependencies, &memory);
}
