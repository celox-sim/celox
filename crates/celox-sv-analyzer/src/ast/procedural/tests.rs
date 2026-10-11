use std::path::Path;

use super::*;

#[test]
fn declared_shapes_are_independent_of_subroutine_and_parent_lifetimes() {
    let mut expected = None;
    for parent in ["module", "package"] {
        for parent_lifetime in ["static", "automatic"] {
            for lifetime in ["", "static", "automatic"] {
                for ansi in [false, true] {
                    for kind in ["function", "task"] {
                        let return_type = if kind == "function" { "int" } else { "" };
                        let ports = "input logic signed [1:0][3:0] value [2:4]";
                        let declaration = if ansi {
                            format!("{kind} {lifetime} {return_type} sample({ports});")
                        } else {
                            format!("{kind} {lifetime} {return_type} sample; {ports};")
                        };
                        let code = format!(
                            "{parent} {parent_lifetime} scope;\n\
                             {declaration}\n\
                             logic [4:0] scratch [0:1];\n\
                             end{kind}\n\
                             end{parent}"
                        );
                        let tree =
                            crate::syntax::parse_source(&code, Path::new("declared_shapes.sv"))
                                .unwrap();
                        let node = tree
                            .into_iter()
                            .find(|node| {
                                matches!(
                                    node,
                                    RefNode::FunctionDeclaration(_) | RefNode::TaskDeclaration(_)
                                )
                            })
                            .unwrap();
                        let env = HashMap::default();
                        let aliases = HashMap::default();
                        let dimensions =
                            subroutine_declared_dimensions(node.clone(), &tree, &env, &aliases)
                                .unwrap();
                        let (name, parameters) = subroutine_declared_parameter_shapes(
                            node.clone(),
                            &tree,
                            &env,
                            &aliases,
                        )
                        .unwrap();
                        assert_eq!(name, "sample");
                        assert_eq!(dimensions.len(), 2);
                        assert_eq!(parameters, vec![dimensions["value"].clone()]);
                        assert_eq!(parameters[0].packed.len(), 2);
                        assert_eq!(parameters[0].unpacked.len(), 1);
                        assert!(parameters[0].signed);
                        assert_eq!(dimensions["scratch"].packed.len(), 1);
                        assert_eq!(dimensions["scratch"].unpacked.len(), 1);
                        if let Some(expected) = &expected {
                            assert_eq!(&dimensions, expected, "{code}");
                        } else {
                            expected = Some(dimensions);
                        }

                        // Real lowering still supplies the enclosing scope's
                        // default and honors an explicit subroutine lifetime.
                        let parent_node = tree
                            .into_iter()
                            .find(|node| {
                                matches!(
                                    node,
                                    RefNode::ModuleDeclarationAnsi(_)
                                        | RefNode::ModuleDeclarationNonansi(_)
                                        | RefNode::PackageDeclaration(_)
                                )
                            })
                            .unwrap();
                        let default_automatic = module_default_automatic(parent_node);
                        assert_eq!(default_automatic, parent_lifetime == "automatic");
                        let syntax = match node {
                            RefNode::FunctionDeclaration(decl) => {
                                function_syntax(decl, &tree, default_automatic)
                            }
                            RefNode::TaskDeclaration(decl) => {
                                task_syntax(decl, &tree, default_automatic)
                            }
                            _ => unreachable!(),
                        }
                        .unwrap();
                        assert_eq!(
                            syntax.automatic,
                            lifetime == "automatic" || (lifetime.is_empty() && default_automatic),
                            "{code}"
                        );
                    }
                }
            }
        }
    }
}
