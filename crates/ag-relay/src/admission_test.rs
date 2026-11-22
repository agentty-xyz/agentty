use super::{AdmissionPolicy, AdmitAll, InstanceAllowList};

/// Identifier on the allow list in these tests, as an operator may type it.
const LISTED: &str = "66687AADF862BD776C8FC18B8E9F8E20089714856EE233B3902A591D0D5F2925";
/// Identifier missing from the allow list.
const UNLISTED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn allow_list_admits_listed_instances_in_any_case_and_refuses_others() {
    // Arrange
    let allow_list = InstanceAllowList::new([LISTED.to_string()]);

    // Act
    let listed = allow_list.admit(&LISTED.to_ascii_lowercase()).await;
    let unlisted = allow_list.admit(UNLISTED).await;

    // Assert
    assert_eq!(listed, Ok(true));
    assert_eq!(unlisted, Ok(false));
}

#[tokio::test]
async fn default_policy_and_empty_allow_list_are_the_two_extremes() {
    // Arrange
    let empty = InstanceAllowList::new(Vec::new());

    // Act
    let by_default = AdmitAll.admit(UNLISTED).await;
    let by_empty_list = empty.admit(UNLISTED).await;

    // Assert
    assert_eq!(by_default, Ok(true));
    assert_eq!(by_empty_list, Ok(false));
}
