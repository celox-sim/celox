//! Locations of module nodes, without borrowing from the owned syntax tree.

use super::*;

#[derive(Clone, Copy)]
enum Location {
    Description(usize),
    // Preserve support for nested declarations and unusual source forms.
    Nested(usize),
}

#[derive(Default)]
pub(crate) struct ModuleIndex {
    names: Vec<String>,
    locations: HashMap<String, Vec<Location>>,
    pub interfaces: ModuleInterfaces,
}

fn source_text(tree: &SyntaxTree) -> Option<&sv_parser::SourceText> {
    tree.into_iter().find_map(|node| match node {
        RefNode::SourceText(source) => Some(source),
        _ => None,
    })
}

fn module_node(declaration: &sv_parser::ModuleDeclaration) -> Option<RefNode<'_>> {
    match declaration {
        sv_parser::ModuleDeclaration::Ansi(module) => Some(RefNode::ModuleDeclarationAnsi(module)),
        sv_parser::ModuleDeclaration::Nonansi(module) => {
            Some(RefNode::ModuleDeclarationNonansi(module))
        }
        _ => None,
    }
}

fn identifier_offset(node: &RefNode<'_>) -> Option<usize> {
    let identifier = match node {
        RefNode::ModuleDeclarationAnsi(module) => &module.nodes.0.nodes.3,
        RefNode::ModuleDeclarationNonansi(module) => &module.nodes.0.nodes.3,
        _ => return None,
    };
    identifier_locate(RefNode::ModuleIdentifier(identifier)).map(|locate| locate.offset)
}

impl ModuleIndex {
    pub fn new(tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        let mut index = Self::default();
        let mut descriptions = HashMap::default();
        if let Some(source) = source_text(tree) {
            for (i, description) in source.nodes.2.iter().enumerate() {
                if let sv_parser::Description::ModuleDeclaration(declaration) = description
                    && let Some(node) = module_node(declaration)
                    && let Some(offset) = identifier_offset(&node)
                {
                    descriptions.insert(offset, i);
                }
            }
        }
        for node in tree {
            let Some(offset) = identifier_offset(&node) else {
                continue;
            };
            let name = module_name_from_node(node.clone(), tree)?;
            index.names.push(name.clone());
            let location = descriptions
                .get(&offset)
                .map_or(Location::Nested(offset), |&i| Location::Description(i));
            index
                .locations
                .entry(name.clone())
                .or_default()
                .push(location);
            if matches!(node, RefNode::ModuleDeclarationAnsi(_)) {
                index
                    .interfaces
                    .insert(name, module_interface_from_node(node, tree)?);
            }
        }
        Ok(index)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn nodes<'a>(&self, tree: &'a SyntaxTree, name: &str) -> Vec<RefNode<'a>> {
        let Some(locations) = self.locations.get(name) else {
            return Vec::new();
        };
        locations
            .iter()
            .filter_map(|location| match location {
                Location::Description(i) => {
                    let sv_parser::Description::ModuleDeclaration(declaration) =
                        &source_text(tree)?.nodes.2[*i]
                    else {
                        return None;
                    };
                    module_node(declaration)
                }
                Location::Nested(offset) => tree
                    .into_iter()
                    .find(|node| identifier_offset(node) == Some(*offset)),
            })
            .collect()
    }
}
