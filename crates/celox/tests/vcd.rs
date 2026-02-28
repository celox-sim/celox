#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use celox::{IOContext, SimulatorBuilder};

    #[test]
    fn test_vcd_generation() {
        let code = r#"
        module Top (
            a: input logic<8>,
            b: output logic<8>,
        ) {
            assign b = a;
        }
        "#;

        let vcd_path = "test_output.vcd";
        let mut sim = SimulatorBuilder::new(code, "Top")
            .vcd(vcd_path)
            .build()
            .unwrap();

        let a = sim.signal("a");
        sim.modify(|ctx: &mut IOContext| {
            ctx.set(a, 8u8);
        })
        .unwrap();

        sim.dump(0);
        sim.dump(10);

        assert!(Path::new(vcd_path).exists());
        let content = fs::read_to_string(vcd_path).unwrap();
        assert!(content.contains("$var wire 8"));
        assert!(content.contains("#0"));
        assert!(content.contains("#10"));

        fs::remove_file(vcd_path).unwrap();
    }
}



