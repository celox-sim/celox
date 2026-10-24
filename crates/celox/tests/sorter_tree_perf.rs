#[path = "test_utils/sorter_tree.rs"]
mod sorter_tree;

#[test]
fn small_sorter_tree_compiles() {
    sorter_tree::compile(4);
}
