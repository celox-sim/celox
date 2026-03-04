#[cfg(test)]
mod tests {
    use crate::generator::generate_project;
    use std::path::PathBuf;
    use veryl_analyzer::ir::Ir;
    use veryl_analyzer::{Analyzer, Context};
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    // Custom formatted printing of TokenStream for debugging
    fn tokens_to_string(tokens: proc_macro2::TokenStream) -> String {
        let file: syn::File = syn::parse2(tokens).unwrap();
        prettyplease::unparse(&file)
    }

    #[test]
    fn test_generate_simple_module() {
        let code = r#"
            module ModuleA (
                clk: input clock,
                rst: input reset,
                a  : input logic<32>,
                b  : output logic<64>,
            ) {
                assign b = a as 64;
            }
        "#;

        let parser = Parser::parse(code, &PathBuf::from("test.veryl")).unwrap();

        let metadata = Metadata::create_default("test_project").unwrap();

        let analyzer = Analyzer::new(&metadata);
        let errors = analyzer.analyze_pass1(&metadata.project.name, &parser.veryl);
        assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass1();
        assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");

        let mut ir = Ir::default();
        let mut context = Context::default();
        let errors = analyzer.analyze_pass2(
            &metadata.project.name,
            &parser.veryl,
            &mut context,
            Some(&mut ir),
        );
        assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass2();
        assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

        let generated_tokens = generate_project(&ir);

        let generated_code = tokens_to_string(generated_tokens);

        assert!(generated_code.contains("pub struct ModuleA"));
        assert!(generated_code.contains("pub fn set_a"));
        assert!(generated_code.contains("pub fn get_b"));
    }

    #[test]
    fn test_generate_full_project() {
        use std::env;
        use std::fs;

        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        // CARGO_MANIFEST_DIR is crates/celox-macros
        let target_dir = PathBuf::from(manifest_dir).join("../celox/tests/macro_project");
        dbg!(&target_dir);
        let metadata_path = std::fs::canonicalize(target_dir.join("Veryl.toml")).unwrap();
        let mut metadata = Metadata::load(&metadata_path).unwrap();

        let mut ir = Ir::default();
        let mut context = Context::default();
        let analyzer = Analyzer::new(&metadata);
        analyzer.clear();

        let mut parsed_files = Vec::new();
        let paths = metadata.paths::<PathBuf>(&[], false, true).unwrap();
        for path_set in paths {
            let code = fs::read_to_string(&path_set.src).unwrap();
            parsed_files.push((path_set, code));
        }

        let mut parsers = Vec::new();
        for (path_set, code) in &parsed_files {
            let parser = Parser::parse(code, &path_set.src).unwrap();
            let errors = analyzer.analyze_pass1(&path_set.prj, &parser.veryl);
            assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
            parsers.push(parser);
        }
        let errors = Analyzer::analyze_post_pass1();
        assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");

        for (i, (path_set, _code)) in parsed_files.iter().enumerate() {
            let parser = &parsers[i];
            let errors = analyzer.analyze_pass2(&path_set.prj, &parser.veryl, &mut context, Some(&mut ir));
            assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
        }
        let errors = Analyzer::analyze_post_pass2();
        assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

        let tokens = generate_project(&ir);
        let code = tokens_to_string(tokens);
        insta::assert_snapshot!(code);
    }
}
