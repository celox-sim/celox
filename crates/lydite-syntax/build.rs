fn main() {
    parol::build::Builder::with_cargo_script_output()
        .grammar_file("src/lyd.par")
        .parser_output_file("lyd_parser.rs")
        .actions_output_file("lyd_grammar_trait.rs")
        .user_type_name("LydGrammar")
        .user_trait_module_name("lyd_grammar")
        .generate_parser()
        .expect("generate lyd grammar");
    let out = std::env::var("OUT_DIR").unwrap();
    let modules = format!(
        "#[allow(clippy::too_many_arguments)] #[path = {:?}] mod lyd_grammar_trait;\n#[path = {:?}] mod lyd_parser;\n",
        format!("{out}/lyd_grammar_trait.rs"),
        format!("{out}/lyd_parser.rs")
    );
    std::fs::write(format!("{out}/modules.rs"), modules).unwrap();
}
