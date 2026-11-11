//! Merging the stores of the branches of a condition into multiplexers, and
//! resolving the multiplexers whose condition the path decides.

use super::*;

impl Comb<'_, '_> {
    /// Merge two branch versions: `then_store` where `condition` holds.
    pub(super) fn merge(
        &mut self,
        condition: &Value,
        then_store: &Store,
        else_store: &Store,
    ) -> Result<Store, sv::AnalyzerError> {
        if let Some(value) = slt_bool(self.arena, &mut self.consts, condition.0) {
            return Ok(if value {
                then_store.fork()
            } else {
                else_store.fork()
            });
        }
        let mut merged = then_store.fork();
        let keys: Vec<SourceVarId> = then_store.differing_keys(else_store).copied().collect();
        for id in keys {
            // A variable written on only one side keeps its entry value on
            // the other.
            let width = self.m.var(id).width;
            let unwritten = RangeStore::new(None, width);
            let then_range = then_store.get(&id).unwrap_or(&unwritten);
            let else_range = else_store.get(&id).unwrap_or(&unwritten);
            if then_range == else_range {
                continue;
            }
            let parts = then_range.aligned_parts(else_range).map_err(range_error)?;
            let parts: Vec<_> = parts
                .into_iter()
                .map(
                    |(access, (then_value, then_access), (else_value, else_access))| {
                        (
                            access,
                            then_value.clone(),
                            then_access,
                            else_value.clone(),
                            else_access,
                        )
                    },
                )
                .collect();
            let mut range = RangeStore {
                ranges: std::collections::BTreeMap::new(),
            };
            for (access, then_value, then_access, else_value, else_access) in parts {
                let value = if then_value.is_none() && else_value.is_none() {
                    None
                } else {
                    let (then_node, then_sources) =
                        self.materialize(id, access, then_value, then_access)?;
                    let (else_node, else_sources) =
                        self.materialize(id, access, else_value, else_access)?;
                    // Earlier merges on the same (or the complementary)
                    // condition are decided in each arm.
                    let (then_node, then_sources) =
                        self.simplify(then_node, then_sources, condition.0, true)?;
                    let (else_node, else_sources) =
                        self.simplify(else_node, else_sources, condition.0, false)?;
                    let mut sources = then_sources;
                    sources.extend(else_sources);
                    if then_node == else_node {
                        Some((then_node, sources))
                    } else {
                        sources.extend(condition.1.iter().copied());
                        let node = self.alloc(SLTNode::Mux {
                            cond: condition.0,
                            then_expr: then_node,
                            else_expr: else_node,
                        })?;
                        Some((node, sources))
                    }
                };
                range
                    .ranges
                    .insert(access.lsb, (value, access.msb - access.lsb + 1, access.lsb));
            }
            merged.insert(id, range);
        }
        Ok(merged)
    }

    /// Resolve the multiplexers of `node` whose condition is known when
    /// `condition` has `value`.
    pub(super) fn simplify(
        &mut self,
        node: NodeId,
        sources: Sources,
        condition: NodeId,
        value: bool,
    ) -> Result<Value, sv::AnalyzerError> {
        let simplified = self.simplify_node(node, condition, value)?;
        if simplified == node {
            return Ok((node, sources));
        }
        match self.node_sources(simplified) {
            Some(sources) => Ok((simplified, sources)),
            None => Ok((node, sources)),
        }
    }

    pub(super) fn simplify_node(
        &mut self,
        node: NodeId,
        condition: NodeId,
        value: bool,
    ) -> Result<NodeId, sv::AnalyzerError> {
        match self.arena.get(node).clone() {
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let known = if cond == condition {
                    Some(value)
                } else if self.complementary(cond, condition) {
                    Some(!value)
                } else {
                    None
                };
                match known {
                    Some(true) => self.simplify_node(then_expr, condition, value),
                    Some(false) => self.simplify_node(else_expr, condition, value),
                    None => Ok(node),
                }
            }
            SLTNode::Slice { expr, access } => {
                let inner = self.simplify_node(expr, condition, value)?;
                if inner == expr {
                    Ok(node)
                } else {
                    self.slice(inner, access)
                }
            }
            _ => Ok(node),
        }
    }

    /// The module-state reads of a tree, or `None` when it contains a fold.
    pub(super) fn node_sources(&self, node: NodeId) -> Option<Sources> {
        let mut sources = Sources::default();
        let mut stack = vec![node];
        let mut seen = HashSet::default();
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            match self.arena.get(node) {
                SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    if index.is_empty() {
                        sources.insert(VarAtomBase::new(*variable, access.lsb, access.msb));
                    } else {
                        let width = self.m.var(*variable).width;
                        sources.insert(VarAtomBase::new(*variable, 0, width - 1));
                        stack.extend(index.iter().map(|entry| entry.node));
                    }
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(left, _, right) => stack.extend([*left, *right]),
                SLTNode::Unary(_, inner)
                | SLTNode::Slice { expr: inner, .. }
                | SLTNode::Capture { expr: inner, .. } => stack.push(*inner),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => stack.extend([*cond, *then_expr, *else_expr]),
                SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
            }
        }
        Some(sources)
    }

    pub(super) fn materialize(
        &mut self,
        id: SourceVarId,
        absolute: BitAccess,
        value: Option<Value>,
        relative: BitAccess,
    ) -> Result<Value, sv::AnalyzerError> {
        match value {
            Some((node, sources)) => Ok((self.slice(node, relative)?, sources)),
            None => {
                let node = self.alloc(SLTNode::Input {
                    variable: id,
                    signed: false,
                    index: Vec::new(),
                    access: absolute,
                })?;
                Ok((
                    node,
                    [VarAtomBase::new(id, absolute.lsb, absolute.msb)]
                        .into_iter()
                        .collect(),
                ))
            }
        }
    }
}
