fn load_sorter_sources() -> String {
    [
        include_str!("../fixtures/sorter_tree/sorter_item.veryl"),
        include_str!("../fixtures/sorter_tree/dist_entry.veryl"),
        include_str!("../fixtures/sorter_tree/min_reduction_tree.veryl"),
        include_str!("../fixtures/sorter_tree/linear_sorter_pull.veryl"),
        include_str!("../fixtures/sorter_tree/linear_sorter.veryl"),
        include_str!("../fixtures/sorter_tree/sorter_tree.veryl"),
    ]
    .join("\n")
}

pub fn compile(n: u64) -> celox::Simulator {
    let code = load_sorter_sources();
    celox::SimulatorBuilder::new(&code, "SorterTreeDistEntry")
        .param("N", n)
        .param("LEAF_DEPTH", 4)
        .param("OUT_DEPTH", 16)
        .build()
        .expect("sorter tree should compile")
}
