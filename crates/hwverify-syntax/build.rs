fn main() {
    parol::build::Builder::with_cargo_script_output()
        .grammar_file("src/hwv.par")
        .parser_output_file("hwv_parser.rs")
        .actions_output_file("hwv_grammar_trait.rs")
        .user_type_name("HwvGrammar")
        .user_trait_module_name("hwv_grammar")
        .generate_parser()
        .expect("generate hwv grammar");
    let out = std::env::var("OUT_DIR").unwrap();
    let modules = format!(
        "#[allow(clippy::too_many_arguments)] #[path = {:?}] mod hwv_grammar_trait;\n#[path = {:?}] mod hwv_parser;\n",
        format!("{out}/hwv_grammar_trait.rs"),
        format!("{out}/hwv_parser.rs")
    );
    std::fs::write(format!("{out}/modules.rs"), modules).unwrap();
}
