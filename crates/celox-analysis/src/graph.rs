//! Sparse directed-graph analyses which do not require CFG semantics.
//!
//! The SCC decomposition is iterative and uses `O(V + E)` time and space.
//! It is suitable for source dataflow graphs whose depth can exceed the native
//! call stack.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphError {
    InvalidSuccessor {
        node: usize,
        successor: usize,
        node_count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StronglyConnectedComponent {
    pub nodes: Vec<usize>,
    pub cyclic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StronglyConnectedComponents {
    pub components: Vec<StronglyConnectedComponent>,
    pub component_for_node: Vec<usize>,
}

impl StronglyConnectedComponents {
    /// Decompose a dense-ID directed graph using iterative Kosaraju passes.
    pub fn analyze(successors: &[Vec<usize>]) -> Result<Self, GraphError> {
        let node_count = successors.len();
        let mut predecessors = vec![Vec::new(); node_count];
        for (node, outgoing) in successors.iter().enumerate() {
            for &successor in outgoing {
                if successor >= node_count {
                    return Err(GraphError::InvalidSuccessor {
                        node,
                        successor,
                        node_count,
                    });
                }
                predecessors[successor].push(node);
            }
        }

        let mut visited = vec![false; node_count];
        let mut postorder = Vec::with_capacity(node_count);
        for seed in 0..node_count {
            if visited[seed] {
                continue;
            }
            visited[seed] = true;
            let mut stack = vec![(seed, 0usize)];
            while let Some((node, next_successor)) = stack.last_mut() {
                if *next_successor == successors[*node].len() {
                    postorder.push(*node);
                    stack.pop();
                    continue;
                }
                let successor = successors[*node][*next_successor];
                *next_successor += 1;
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            }
        }

        let mut component_for_node = vec![usize::MAX; node_count];
        let mut components = Vec::new();
        for seed in postorder.into_iter().rev() {
            if component_for_node[seed] != usize::MAX {
                continue;
            }
            let component = components.len();
            component_for_node[seed] = component;
            let mut nodes = Vec::new();
            let mut stack = vec![seed];
            while let Some(node) = stack.pop() {
                nodes.push(node);
                for &predecessor in predecessors[node].iter().rev() {
                    if component_for_node[predecessor] == usize::MAX {
                        component_for_node[predecessor] = component;
                        stack.push(predecessor);
                    }
                }
            }
            nodes.sort_unstable();
            let cyclic = nodes.len() > 1
                || nodes
                    .first()
                    .is_some_and(|node| successors[*node].contains(node));
            components.push(StronglyConnectedComponent { nodes, cyclic });
        }

        Ok(Self {
            components,
            component_for_node,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_acyclic_nodes_and_cycles() {
        let graph = vec![vec![1], vec![2], vec![1, 3], Vec::new()];
        let sccs = StronglyConnectedComponents::analyze(&graph).unwrap();

        assert_eq!(sccs.component_for_node[1], sccs.component_for_node[2]);
        assert_ne!(sccs.component_for_node[0], sccs.component_for_node[1]);
        assert_ne!(sccs.component_for_node[3], sccs.component_for_node[1]);
        assert!(
            sccs.components[sccs.component_for_node[1]].cyclic,
            "1 <-> 2 is cyclic"
        );
        assert!(!sccs.components[sccs.component_for_node[0]].cyclic);
    }

    #[test]
    fn handles_deep_graph_without_recursion() {
        let node_count = 100_000;
        let mut graph = vec![Vec::new(); node_count];
        for (node, outgoing) in graph.iter_mut().enumerate().take(node_count - 1) {
            outgoing.push(node + 1);
        }

        let sccs = StronglyConnectedComponents::analyze(&graph).unwrap();
        assert_eq!(sccs.components.len(), node_count);
        assert!(sccs.components.iter().all(|component| !component.cyclic));
    }

    #[test]
    fn rejects_out_of_range_edges() {
        assert_eq!(
            StronglyConnectedComponents::analyze(&[vec![1]]),
            Err(GraphError::InvalidSuccessor {
                node: 0,
                successor: 1,
                node_count: 1,
            })
        );
    }
}
