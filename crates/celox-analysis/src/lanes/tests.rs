use super::*;

fn assign(costs: &[u64], predecessors: &[Vec<usize>], lanes: u32, sync: u64) -> LaneAssignment {
    let groups = (0..costs.len()).collect::<Vec<_>>();
    let group_lanes = vec![None; costs.len()];
    assign_lanes(
        LaneAssignmentInput {
            costs,
            predecessors,
            groups: &groups,
            group_lanes: &group_lanes,
        },
        LaneAssignmentOptions {
            lanes,
            synchronization_cost: sync,
            locality_slack: 0,
        },
    )
    .unwrap()
}

#[test]
fn publication_does_not_hold_a_consumer_behind_unrelated_work() {
    let lanes = [0, 0, 1];
    let predecessors = vec![vec![], vec![], vec![0]];
    let costs = [2, 100, 100];
    let original = form_lane_tasks(2, &lanes, &predecessors).unwrap();
    assert_eq!(estimate_task_makespan(2, &original, &costs, 0, 0), 202);
    for options in [
        TaskFormationOptions {
            costs: &costs,
            max_cost: 50,
            ..TaskFormationOptions::default()
        },
        TaskFormationOptions {
            publish_after: &[true, false, false],
            ..TaskFormationOptions::default()
        },
    ] {
        let tasks = form_lane_tasks_with_options(2, &lanes, &predecessors, &[], options).unwrap();
        assert_eq!(estimate_task_makespan(2, &tasks, &costs, 0, 0), 102);
        verify_lane_tasks(2, &tasks, &predecessors).unwrap();
        assert_eq!(
            tasks[2].waits,
            [LaneWait {
                lane: 0,
                completed_tasks: 1
            }]
        );
    }
}

#[test]
fn publication_and_budget_keep_sparse_classes_separate() {
    let predecessors = vec![vec![], vec![0], vec![1], vec![2]];
    let tasks = form_lane_tasks_with_options(
        1,
        &[0; 4],
        &predecessors,
        &[1, 0, 2, 2],
        TaskFormationOptions {
            costs: &[1; 4],
            max_cost: 100,
            publish_after: &[false, true, false, false],
        },
    )
    .unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.items.clone()).collect::<Vec<_>>(),
        [vec![0, 1], vec![2, 3]]
    );
    verify_lane_tasks(1, &tasks, &predecessors).unwrap();
    assert_eq!(
        form_lane_tasks_with_options(
            1,
            &[0],
            &[vec![]],
            &[],
            TaskFormationOptions {
                max_cost: 1,
                ..TaskFormationOptions::default()
            }
        ),
        Err(LaneError::Shape)
    );
}

#[test]
fn independent_items_spread_across_lanes() {
    let costs = vec![10; 8];
    let predecessors = vec![Vec::new(); 8];
    let result = assign(&costs, &predecessors, 4, 100);
    let mut per_lane = [0; 4];
    for lane in &result.item_lanes {
        per_lane[*lane as usize] += 1;
    }
    assert_eq!(per_lane, [2, 2, 2, 2]);
    assert_eq!(result.estimated_makespan, 20);
    assert_eq!(result.total_cost, 80);
}

#[test]
fn expensive_synchronization_keeps_a_chain_in_one_lane() {
    let costs = vec![10; 4];
    let predecessors = vec![vec![], vec![0], vec![1], vec![2]];
    let result = assign(&costs, &predecessors, 4, 1000);
    assert!(
        result
            .item_lanes
            .iter()
            .all(|lane| *lane == result.item_lanes[0])
    );
    assert_eq!(result.estimated_makespan, 40);
}

#[test]
fn locality_slack_keeps_runs_of_independent_items_together() {
    let costs = vec![10; 8];
    let predecessors = vec![Vec::new(); 8];
    let groups = (0..8).collect::<Vec<_>>();
    let group_lanes = vec![None; 8];
    let result = assign_lanes(
        LaneAssignmentInput {
            costs: &costs,
            predecessors: &predecessors,
            groups: &groups,
            group_lanes: &group_lanes,
        },
        LaneAssignmentOptions {
            lanes: 2,
            synchronization_cost: 100,
            locality_slack: 30,
        },
    )
    .unwrap();
    // A lane keeps taking items until it is more than the slack ahead.
    assert_eq!(result.item_lanes, vec![0, 0, 0, 0, 1, 1, 1, 1]);
    assert_eq!(result.estimated_makespan, 40);
}

#[test]
fn ownership_groups_share_a_lane_and_pins_are_respected() {
    let costs = vec![10, 10, 10, 10];
    let predecessors = vec![Vec::new(); 4];
    let groups = vec![0, 1, 0, 2];
    let group_lanes = vec![None, Some(3), None];
    let result = assign_lanes(
        LaneAssignmentInput {
            costs: &costs,
            predecessors: &predecessors,
            groups: &groups,
            group_lanes: &group_lanes,
        },
        LaneAssignmentOptions {
            lanes: 4,
            synchronization_cost: 1,
            locality_slack: 0,
        },
    )
    .unwrap();
    assert_eq!(result.item_lanes[0], result.item_lanes[2]);
    assert_eq!(result.item_lanes[1], 3);
}

#[test]
fn non_topological_input_is_rejected() {
    let costs = vec![1, 1];
    let predecessors = vec![vec![1], vec![]];
    let groups = vec![0, 1];
    let group_lanes = vec![None, None];
    assert_eq!(
        assign_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes: 2,
                synchronization_cost: 0,
                locality_slack: 0,
            },
        ),
        Err(LaneError::NotTopological {
            item: 0,
            predecessor: 1
        })
    );
}

#[test]
fn tasks_split_only_at_new_cross_lane_dependencies() {
    // lane 0: 0 -> 2 -> 4 -> 6, lane 1: 1 -> 3 (depends on 0) -> 5
    // (depends on 2, already covered) -> 7 (depends on 6).
    let lanes = [0, 1, 0, 1, 0, 1, 0, 1];
    let predecessors = vec![
        vec![],
        vec![],
        vec![0],
        vec![1, 0],
        vec![2],
        vec![3, 2],
        vec![4],
        vec![5, 6],
    ];
    let tasks = form_lane_tasks(2, &lanes, &predecessors).unwrap();
    verify_lane_tasks(2, &tasks, &predecessors).unwrap();
    let lane0 = tasks
        .iter()
        .filter(|task| task.lane == 0)
        .collect::<Vec<_>>();
    // The consumer of item 0 closes the task holding items 0 and 2.
    assert_eq!(lane0[0].items, vec![0, 2]);
    assert_eq!(lane0[1].items, vec![4, 6]);
    let lane1 = tasks
        .iter()
        .filter(|task| task.lane == 1)
        .collect::<Vec<_>>();
    assert_eq!(lane1.len(), 3);
    assert_eq!(lane1[0].items, vec![1]);
    assert!(lane1[0].waits.is_empty());
    assert_eq!(lane1[1].items, vec![3, 5]);
    assert_eq!(
        lane1[1].waits,
        vec![LaneWait {
            lane: 0,
            completed_tasks: 1
        }]
    );
    assert_eq!(lane1[2].items, vec![7]);
    assert_eq!(
        lane1[2].waits,
        vec![LaneWait {
            lane: 0,
            completed_tasks: 2
        }]
    );
}

#[test]
fn list_scheduling_runs_critical_work_first() {
    // Item 0 is independent; item 1 feeds a chain on another lane whose
    // synchronization makes it the longer path. With fixed lanes, lane 0
    // should run 1 before 0.
    let costs = vec![6, 1, 1, 1];
    let predecessors = vec![vec![], vec![], vec![1], vec![2]];
    let groups = vec![0, 0, 1, 1];
    let group_lanes = vec![Some(0), Some(1)];
    let plan = list_schedule_lanes(
        LaneAssignmentInput {
            costs: &costs,
            predecessors: &predecessors,
            groups: &groups,
            group_lanes: &group_lanes,
        },
        LaneAssignmentOptions {
            lanes: 2,
            synchronization_cost: 5,
            locality_slack: 0,
        },
    )
    .unwrap();
    assert_eq!(plan.item_lanes, vec![0, 0, 1, 1]);
    let position = |item| plan.order.iter().position(|&x| x == item).unwrap();
    assert!(position(1) < position(0));
    // Lane 0 runs 1 then 0 (finishing at 7); lane 1 starts the chain
    // after the synchronization and finishes at 1 + 5 + 2.
    assert_eq!(plan.estimated_makespan, 8);
    for (item, predecessors) in predecessors.iter().enumerate() {
        for &predecessor in predecessors {
            assert!(position(predecessor) < position(item));
        }
    }
}

#[test]
fn task_makespan_charges_task_boundaries_and_waits() {
    let tasks = vec![
        LaneTask {
            lane: 0,
            sequence: 0,
            items: vec![0],
            waits: Vec::new(),
        },
        LaneTask {
            lane: 1,
            sequence: 0,
            items: vec![1, 2],
            waits: vec![LaneWait {
                lane: 0,
                completed_tasks: 1,
            }],
        },
    ];
    // Lane 1 starts at 10 + 5 (wait) and runs 2 + 3 + 1 (overhead).
    assert_eq!(estimate_task_makespan(2, &tasks, &[9, 2, 3], 5, 1), 21);
}

#[test]
fn transitive_waits_are_omitted() {
    // 0 (lane 0) -> 1 (lane 1) -> 2 (lane 2); 2 also depends on 0.
    let lanes = [0, 1, 2];
    let predecessors = vec![vec![], vec![0], vec![1, 0]];
    let tasks = form_lane_tasks(3, &lanes, &predecessors).unwrap();
    verify_lane_tasks(3, &tasks, &predecessors).unwrap();
    let last = tasks.iter().find(|task| task.lane == 2).unwrap();
    assert_eq!(
        last.waits,
        vec![LaneWait {
            lane: 1,
            completed_tasks: 1
        }]
    );
}

#[test]
fn separated_classes_never_share_a_task() {
    let lanes = [0, 0, 0, 0];
    let predecessors = vec![vec![], vec![], vec![], vec![]];
    let tasks = form_lane_tasks_separated(1, &lanes, &predecessors, &[1, 0, 2, 2]).unwrap();
    verify_lane_tasks(1, &tasks, &predecessors).unwrap();
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.items.clone())
            .collect::<Vec<_>>(),
        vec![vec![0, 1], vec![2, 3]]
    );
}

#[test]
fn verification_rejects_a_missing_wait() {
    let lanes = [0, 1];
    let predecessors = vec![vec![], vec![0]];
    let mut tasks = form_lane_tasks(2, &lanes, &predecessors).unwrap();
    for task in &mut tasks {
        task.waits.clear();
    }
    assert_eq!(verify_lane_tasks(2, &tasks, &predecessors), Err((0, 1)));
}

#[test]
fn randomized_graphs_form_valid_tasks() {
    // Deterministic pseudo-random DAGs; the verifier is the oracle.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..200 {
        let count = (next() % 40) as usize + 1;
        let lane_count = (next() % 5) as u32 + 1;
        let mut predecessors = vec![Vec::new(); count];
        for (item, row) in predecessors.iter_mut().enumerate() {
            for predecessor in 0..item {
                if next() % 5 == 0 {
                    row.push(predecessor);
                }
            }
        }
        let costs = (0..count).map(|_| next() % 50 + 1).collect::<Vec<_>>();
        let assignment = assign(&costs, &predecessors, lane_count, next() % 40);
        let tasks = form_lane_tasks(lane_count, &assignment.item_lanes, &predecessors).unwrap();
        verify_lane_tasks(lane_count, &tasks, &predecessors).unwrap();
        let random_lanes = (0..count)
            .map(|_| (next() % u64::from(lane_count)) as u32)
            .collect::<Vec<_>>();
        let tasks = form_lane_tasks(lane_count, &random_lanes, &predecessors).unwrap();
        verify_lane_tasks(lane_count, &tasks, &predecessors).unwrap();
        let publish = (0..count).map(|_| next() % 3 == 0).collect::<Vec<_>>();
        let classes = (0..count).map(|_| (next() % 4) as u8).collect::<Vec<_>>();
        let tasks = form_lane_tasks_with_options(
            lane_count,
            &random_lanes,
            &predecessors,
            &classes,
            TaskFormationOptions {
                costs: &costs,
                max_cost: next() % 100 + 1,
                publish_after: &publish,
            },
        )
        .unwrap();
        verify_lane_tasks(lane_count, &tasks, &predecessors).unwrap();
        let mut covered = tasks
            .iter()
            .flat_map(|t| t.items.iter().copied())
            .collect::<Vec<_>>();
        covered.sort_unstable();
        assert_eq!(covered, (0..count).collect::<Vec<_>>());
        for task in &tasks {
            assert!(
                task.items.len() == 1
                    || task
                        .items
                        .iter()
                        .fold(0, |class, &item| class | classes[item])
                        != 3
            );
        }

        // Priority list scheduling: a topological order that keeps
        // ownership groups and fixed lanes.
        let groups = (0..count)
            .map(|_| (next() % count as u64) as usize)
            .collect::<Vec<_>>();
        let group_lanes = (0..count)
            .map(|_| (next() % 3 == 0).then(|| (next() % u64::from(lane_count)) as u32))
            .collect::<Vec<_>>();
        let plan = list_schedule_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes: lane_count,
                synchronization_cost: next() % 40,
                locality_slack: next() % 20,
            },
        )
        .unwrap();
        let mut position = vec![usize::MAX; count];
        for (index, &item) in plan.order.iter().enumerate() {
            assert_eq!(position[item], usize::MAX, "item listed twice");
            position[item] = index;
        }
        for (item, row) in predecessors.iter().enumerate() {
            assert!(
                row.iter()
                    .all(|&predecessor| position[predecessor] < position[item])
            );
            if let Some(lane) = group_lanes[groups[item]] {
                assert_eq!(plan.item_lanes[item], lane);
            }
            let first = groups
                .iter()
                .position(|&group| group == groups[item])
                .unwrap();
            assert_eq!(plan.item_lanes[item], plan.item_lanes[first]);
        }
    }
}
