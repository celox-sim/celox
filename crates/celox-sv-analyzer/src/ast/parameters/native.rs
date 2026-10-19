//! Ordinary scalar declaration type metadata. Bound construction and width
//! evaluation remain separate, using the caller's existing environment kernel.
use super::*;
use celox_sv_syntax::{DataType, DataTypeKind, ParameterKind};
pub(super) struct Prefix<'a> {
    pub data: &'a DataType,
    pub alias: Option<Type>,
    primitive: Option<Type>,
}
pub(super) fn prefix<'a>(
    parameter: Option<&celox_sv_syntax::Parameter>,
    tree: &'a SyntaxTree,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Prefix<'a>>> {
    let Some(parameter) = parameter else {
        return Ok(None);
    };
    let ParameterKind::Value { data_type, .. } = parameter.kind else {
        return Err(AnalyzerError::Parse(
            "native scalar parameter declaration role disagrees".into(),
        ));
    };
    let data = tree.declarations().data_type(data_type);
    let (alias, primitive) = match &data.kind {
        DataTypeKind::Implicit => (None, None),
        DataTypeKind::Builtin(name) => (
            None,
            Some(
                native_types::builtin_type(name, data.signed)
                    .ok_or_else(|| unsupported("unsupported parameter data type"))?,
            ),
        ),
        DataTypeKind::Named(path) if path.iter().all(|part| part.arguments.is_none()) => {
            let name = path
                .iter()
                .map(|part| instances::normalize_identifier(&part.name.spelling))
                .collect::<Vec<_>>()
                .join("::");
            (aliases.get(&name).cloned(), None)
        }
        _ => return Err(unsupported("unsupported parameter data type")),
    };
    Ok(Some(Prefix {
        data,
        alias,
        primitive,
    }))
}
impl Prefix<'_> {
    pub fn has_declared_type(&self) -> bool {
        !matches!(self.data.kind, DataTypeKind::Implicit)
    }
    pub fn signed(&self) -> bool {
        self.primitive
            .as_ref()
            .map(|ty| ty.is_signed)
            .unwrap_or(self.data.signed.unwrap_or(false))
    }
    pub fn is_2state(&self) -> bool {
        self.alias
            .as_ref()
            .or(self.primitive.as_ref())
            .is_some_and(|ty| ty.kind == TypeKind::Bit)
    }
    pub fn scalar_width(&self, state: &AnalysisState) -> Option<usize> {
        self.primitive
            .as_ref()
            .and_then(|ty| expr_type_from_type(state, ty, &HashMap::default()))
            .map(|ty| ty.width)
    }
    pub fn signed_element_depth(&self) -> Option<usize> {
        let alias = self.alias.as_ref()?;
        let count = self.data.packed_dimensions.len();
        if count > 0 && alias.is_signed {
            Some(count)
        } else {
            alias.signed_element_depth.map(|depth| depth + count)
        }
    }
    pub fn ranges(
        &self,
        tree: &AnalysisContext<'_>,
        env: &HashMap<String, i128>,
        aliases: &HashMap<String, Type>,
    ) -> Converted<Vec<PackedRange>> {
        let mut ranges =
            native_constants::packed_ranges(&self.data.packed_dimensions, tree, env, aliases)?;
        if let Some(base) = self.alias.as_ref().or(self.primitive.as_ref()) {
            ranges.extend(base.packed_ranges.iter().cloned());
        }
        Ok(ranges)
    }
}
