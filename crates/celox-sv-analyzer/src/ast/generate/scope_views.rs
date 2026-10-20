//! Materialize each immutable generate scope once per collector and input view.

use super::*;

type Key = [usize; 6];

pub(in crate::ast) struct ScopeViews<'a> {
    dimensions: &'a PackedDimensions,
    literals: &'a HashMap<String, Expr>,
    // Retain borrowers of every keyed snapshot. Pointer identities cannot be
    // recycled while an entry exists, even when nested scopes interrupt siblings.
    views: HashMap<Key, (&'a Item, PackedDimensions)>,
    literal_views: HashMap<Key, (&'a Item, SharedMap<Expr>)>,
}

impl<'a> ScopeViews<'a> {
    pub fn new(dimensions: &'a PackedDimensions) -> Self {
        Self::with_literals(dimensions, &dimensions.parameter_values)
    }

    pub fn with_literals(
        dimensions: &'a PackedDimensions,
        literals: &'a HashMap<String, Expr>,
    ) -> Self {
        Self {
            dimensions,
            literals,
            views: HashMap::default(),
            literal_views: HashMap::default(),
        }
    }

    pub fn dimensions(&mut self, item: &'a Item) -> &PackedDimensions {
        &self
            .views
            .entry(key(item))
            .or_insert_with(|| (item, item.dimensions(self.dimensions)))
            .1
    }

    pub fn get(&mut self, item: &'a Item) -> (&PackedDimensions, &SharedMap<Expr>) {
        let dimensions = &self
            .views
            .entry(key(item))
            .or_insert_with(|| (item, item.dimensions(self.dimensions)))
            .1;
        let literals = &self
            .literal_views
            .entry(key(item))
            .or_insert_with(|| (item, item.parameter_literals(self.literals).into()))
            .1;
        (dimensions, literals)
    }
}

fn key(item: &Item) -> Key {
    [
        item.env.identity(),
        item.literals.identity(),
        item.names.identity(),
        Arc::as_ptr(&item.shadowed) as usize,
        Arc::as_ptr(&item.parameter_dimensions) as usize,
        item.qualified_types.identity(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fmt::Write, path::Path};

    #[test]
    fn builds_one_view_for_a_growing_block_of_siblings() {
        for count in [16, 64, 256] {
            let mut code = String::from("module Top(); if (1) begin : g\n");
            for index in 0..count {
                writeln!(code, "logic [7:0] s{index};").unwrap();
            }
            code.push_str("end endmodule");
            let tree =
                crate::source_tree::parse_scope_fixture(&code, Path::new("scope_scaling.sv"))
                    .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
            let dimensions = PackedDimensions::default();
            let mut views = ScopeViews::new(&dimensions);
            for item in &active {
                views.get(item);
            }
            assert_eq!(active.len(), count);
            assert_eq!(views.views.len(), 1);
            assert_eq!(views.literal_views.len(), 1);
        }
    }

    #[test]
    fn preserves_nested_and_interrupted_scopes_and_detaches_changed_snapshots() {
        let tree = crate::source_tree::parse_scope_fixture(
            "module Top(); if (1) begin : g localparam int P=2; logic [P-1:0] x; if (1) begin : inner localparam int P=3; logic [P-1:0] x; end logic y; end if (1) begin : h localparam int P=4; logic [P-1:0] x; end endmodule",
            Path::new("scope_views.sv"),
        ).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let source = Source::from_syntax(&tree).unwrap();
        let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
        let dimensions = packed_dimensions_from_ports_and_signals(
            source.modules()[0].ports(),
            source.modules()[0].signals(),
            &HashMap::default(),
            &HashMap::default(),
        );
        let inherited = HashMap::from_iter([("P".into(), Expr::Literal("99".into()))]);
        let original = &active[0];
        let mut changed: Vec<_> = (0..6).map(|_| original.clone()).collect();
        changed[0].env.insert("P".into(), 8);
        changed[1]
            .literals
            .insert("extra".into(), Expr::Literal("5".into()));
        changed[2].names.insert("alias".into(), "g.x".into());
        Arc::make_mut(&mut changed[3].shadowed).insert("extra".into());
        Arc::make_mut(&mut changed[4].parameter_dimensions).clear();
        changed[5]
            .qualified_types
            .insert("g.T".into(), Type::implicit());
        let mut views = ScopeViews::with_literals(&dimensions, &inherited);
        for item in active.iter().chain(&changed) {
            let (cached_dimensions, cached_literals) = views.get(item);
            assert_eq!(cached_dimensions, &item.dimensions(&dimensions));
            assert_eq!(&**cached_literals, &item.parameter_literals(&inherited));
        }
        assert!(!views.dimensions(original).type_aliases.contains_key("g.T"));
        assert!(
            views
                .dimensions(&changed[5])
                .type_aliases
                .contains_key("g.T")
        );
        assert_eq!(views.views.len(), 9);
        assert_eq!(views.literal_views.len(), 9);
        // Collector inputs also bound the cache: a different inherited context
        // must never see the materialization from the preceding collector.
        let other = HashMap::from_iter([("P".into(), Expr::Literal("7".into()))]);
        let mut other_views = ScopeViews::with_literals(&dimensions, &other);
        let (_, literals) = other_views.get(original);
        assert_eq!(&**literals, &original.parameter_literals(&other));
        assert_ne!(&**literals, &original.parameter_literals(&inherited));
    }
}
