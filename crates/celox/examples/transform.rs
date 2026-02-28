use std::{
    error::Error,
    fs::File,
    io::{BufWriter, Write},
};

use celox::SimulatorBuilder;

fn main() -> Result<(), Box<dyn Error>> {
    // Create output file (generated in the current directory)
    let file = File::create("simulation_dump.log")?;
    let mut writer = BufWriter::new(file);

    // Helper macro: It would be complex to write to both file and terminal,
    // so we focus only on writing to the file.
    macro_rules! log {
        ($($arg:tt)*) => {
            writeln!(writer, $($arg)*)?;
        };
    }

    let cases = vec![
        /*(
            "Dependency Chain Demo",
            r#"
            module Top(
                a: input logic<4>,
                b: input logic<4>,
                c: input logic<4>,
                y: output logic<4>,) {
                var w1: logic<4>;
                var w2: logic<4>;
                assign y = ~w2;
                assign w2 = w1 ^ c;
                assign w1 = a & b;
            }
        "#,
        ),
        (
            "Dynamic Indexing Demo",
            r#"
            module Top(
                x: input logic<8>,
                i: input logic<8>,
                y: output logic<8>,) {
               always_comb {
                    y = 0;
                    y[i+2] = x[i];
               }
            }
        "#,
        ),*/
        (
            "Condition Shadding",
            r#"
            module Top (a: input logic<8>, b: input logic<8>,c: input logic,o: output logic<8>) {
                var tmp: logic<8>;
                always_comb {
                    tmp = a & b;
                    if (c) {
                        tmp=~tmp;
                    }
                    o = tmp;
                }
            }
            "#,
        ),
        /*(
            "SLT SSA",
            r#"
            module Top (a: input logic<8>, c: input logic,o: output logic<8>) {
                var x: logic<8>;
                always_comb {
                    x = a;
                    x = x + 8'd1;  // At this point, x is a
                    if (c) {
                        x = x << 1;    // At this point, x is a+1
                    }
                     // At this point, x is (a+1) << 1 or a+1
                    o = x;
                }
            }
            "#,
        ),*/
    ];

    for (name, code) in cases {
        log!("================================================================================");
        log!(" CASE: {}", name);
        log!("================================================================================");

        let result = SimulatorBuilder::new(code, "Top")
            .trace_sim_modules()
            .trace_post_optimized_sir()
            .trace_post_optimized_clif()
            .build_with_trace();

        let trace = result.trace;

        log!("\n--- [1] Simulation Logic Tree (SLT) ---");
        log!("{}", trace.format_slt().unwrap());

        log!("\n--- [2] Simulator IR (SIR) ---");
        log!("{}", trace.format_program().unwrap());

        log!("\n--- [3] Cranelift IR (CLIF) ---");
        log!("{}", trace.post_optimized_clif.as_deref().unwrap_or(""));

        log!("\n\n");
    }

    // Flush buffer to ensure writing
    writer.flush()?;
    println!("Dumping complete. Please check 'simulation_dump.log'.");

    Ok(())
}
