use crate::campaign::{CampaignCandidate, select_campaign_tasks};

#[test]
fn selects_planned_tasks_in_order_after_counting_active_children() {
    // Arrange
    let candidates = [
        CampaignCandidate {
            key: "active",
            planned: false,
            occupies_slot: true,
            settled: false,
        },
        CampaignCandidate {
            key: "first",
            planned: true,
            occupies_slot: false,
            settled: false,
        },
        CampaignCandidate {
            key: "second",
            planned: true,
            occupies_slot: false,
            settled: false,
        },
    ];

    // Act
    let one_slot = select_campaign_tasks(2, &candidates);
    let full = select_campaign_tasks(1, &candidates);

    // Assert
    assert_eq!(one_slot.selected, ["first"]);
    assert!(!one_slot.all_settled);
    assert_eq!(full.selected, Vec::<&str>::new());
}

#[test]
fn rollup_requires_a_nonempty_fully_settled_campaign() {
    // Arrange
    let candidates = [CampaignCandidate {
        key: 1,
        planned: false,
        occupies_slot: false,
        settled: true,
    }];

    // Act
    let settled = select_campaign_tasks(2, &candidates);
    let empty = select_campaign_tasks::<i32>(2, &[]);

    // Assert
    assert!(settled.all_settled);
    assert_eq!(settled.selected, Vec::<i32>::new());
    assert!(!empty.all_settled);
}
