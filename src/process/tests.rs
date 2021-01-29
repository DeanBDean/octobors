use chrono::Duration;
use models::IssueState;

use super::*;

fn make_context() -> (PR, context::Client, context::Config) {
    let client =
        context::Client::new("token".to_string(), "org".to_string(), "repo".to_string()).unwrap();
    let config = context::Config {
        needs_description_label: Some("needs-description".to_string()),
        required_statuses: vec!["status1"].into_iter().map(String::from).collect(),
        ci_passed_label: "ci-passed".to_string(),
        reviewed_label: Some("reviewed".to_string()),
        block_merge_label: Some("block-merge".to_string()),
        automerge_grace_period: Some(1000),
        merge_method: octocrab::params::pulls::MergeMethod::Rebase,
    };
    let pr = PR {
        id: 13482,
        number: 1,
        commit_sha: "somesha".to_string(),
        draft: false,
        state: models::IssueState::Open,
        updated_at: Utc::now(),
        labels: HashSet::new(),
        has_description: true,
    };
    (pr, client, config)
}

fn make_analyzer<'a>(
    pr: &'a PR,
    client: &'a context::Client,
    config: &'a context::Config,
) -> Analyzer<'a> {
    let mut analyzer = Analyzer::new(pr, client, config);
    analyzer.reviews = RemoteData::Local(vec![
        review(1, ReviewState::Commented),
        review(2, ReviewState::Approved),
        review(3, ReviewState::Commented),
    ]);
    analyzer.statuses = RemoteData::Local(
        vec![
            ("status1".to_string(), StatusState::Success),
            ("status2".to_string(), StatusState::Failure),
        ]
        .into_iter()
        .collect(),
    );
    analyzer
}

#[tokio::test]
async fn ok_pr_actions() {
    let (pr, client, config) = make_context();
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(
        analyzer.required_actions().await.unwrap(),
        *Actions::noop()
            .set_merge(true)
            .set_label("reviewed", Presence::Present)
            .set_label("ci-passed", Presence::Present)
            .set_label("needs-description", Presence::Absent)
    );
}

#[tokio::test]
async fn draft_pr_actions() {
    let (mut pr, client, config) = make_context();
    pr.draft = true;
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(analyzer.required_actions().await.unwrap(), Actions::noop());
}

#[tokio::test]
async fn closed_pr_actions() {
    let (mut pr, client, config) = make_context();
    pr.state = IssueState::Closed;
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(analyzer.required_actions().await.unwrap(), Actions::noop());
}

#[tokio::test]
async fn stale_pr_actions() {
    let (mut pr, client, config) = make_context();
    pr.updated_at = Utc::now() - Duration::minutes(61);
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(analyzer.required_actions().await.unwrap(), Actions::noop());
}

#[tokio::test]
async fn no_description_pr_actions() {
    let (mut pr, client, config) = make_context();
    pr.has_description = false;
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(
        analyzer.required_actions().await.unwrap(),
        *Actions::noop()
            .set_merge(false)
            .set_label("reviewed", Presence::Present)
            .set_label("ci-passed", Presence::Present)
            .set_label("needs-description", Presence::Present)
    );
}

#[tokio::test]
async fn no_description_none_required_pr_actions() {
    let (mut pr, client, mut config) = make_context();
    config.needs_description_label = None;
    pr.has_description = false;
    let analyzer = make_analyzer(&pr, &client, &config);
    assert_eq!(
        analyzer.required_actions().await.unwrap(),
        *Actions::noop()
            .set_merge(true)
            .set_label("reviewed", Presence::Present)
            .set_label("ci-passed", Presence::Present)
    );
}

#[tokio::test]
async fn review_not_required_if_label_not_configured() {
    use ReviewState::{Approved, ChangesRequested, Commented, Pending};
    macro_rules! assert_approved {
        ($approved:expr, $cases:expr) => {{
            let (pr, client, mut config) = make_context();
            config.reviewed_label = None;
            let mut analyzer = make_analyzer(&pr, &client, &config);
            analyzer.reviews = RemoteData::Local($cases);
            assert_eq!(
                analyzer.required_actions().await.unwrap(),
                *Actions::noop()
                    .set_merge($approved)
                    .set_label("ci-passed", Presence::Present)
                    .set_label("needs-description", Presence::Absent)
            );
        }};
    }

    assert_approved!(true, vec![]);
    assert_approved!(true, vec![review(1, Approved)]);
    assert_approved!(true, vec![review(1, Commented)]);
    assert_approved!(false, vec![review(1, ChangesRequested)]);
    assert_approved!(false, vec![review(1, Pending)]);
}

#[tokio::test]
async fn changes_requested_still_blocks_if_label_not_configured() {
    let (pr, client, mut config) = make_context();
    config.reviewed_label = None;
    let mut analyzer = make_analyzer(&pr, &client, &config);
    analyzer.reviews = RemoteData::Local(vec![Review {
        user_id: 1,
        state: ReviewState::ChangesRequested,
    }]);
    assert_eq!(
        analyzer.required_actions().await.unwrap(),
        *Actions::noop()
            .set_merge(false)
            .set_label("ci-passed", Presence::Present)
            .set_label("needs-description", Presence::Absent)
    );
}

#[tokio::test]
async fn required_ci_not_passed_pr_actions() {
    macro_rules! assert_ci_failed_actions {
        ($cases:expr) => {{
            let (pr, client, mut config) = make_context();
            config.required_statuses = vec!["required1".to_string(), "required2".to_string()];
            let mut analyzer = make_analyzer(&pr, &client, &config);
            analyzer.statuses = RemoteData::Local(
                $cases
                    .into_iter()
                    .map(|(a, b): (&str, StatusState)| (a.to_string(), b))
                    .collect(),
            );
            assert_eq!(
                analyzer.required_actions().await.unwrap(),
                *Actions::noop()
                    .set_merge(false)
                    .set_label("reviewed", Presence::Present)
                    .set_label("ci-passed", Presence::Absent)
                    .set_label("needs-description", Presence::Absent)
            );
        }};
    }

    // No passes
    assert_ci_failed_actions!(vec![]);

    // One pass, other missing
    assert_ci_failed_actions!(vec![("required1", StatusState::Success)]);
    assert_ci_failed_actions!(vec![("required2", StatusState::Success)]);

    // One passed, other failed

    assert_ci_failed_actions!(vec![
        ("required2", StatusState::Success),
        ("required1", StatusState::Error)
    ]);
    assert_ci_failed_actions!(vec![
        ("required2", StatusState::Success),
        ("required1", StatusState::Failure)
    ]);
    assert_ci_failed_actions!(vec![
        ("required2", StatusState::Success),
        ("required1", StatusState::Pending)
    ]);
    assert_ci_failed_actions!(vec![
        ("required1", StatusState::Success),
        ("required2", StatusState::Error)
    ]);
    assert_ci_failed_actions!(vec![
        ("required1", StatusState::Success),
        ("required2", StatusState::Failure)
    ]);
    assert_ci_failed_actions!(vec![
        ("required1", StatusState::Success),
        ("required2", StatusState::Pending)
    ]);

    // Failing statuses with a non-required pass

    assert_ci_failed_actions!(vec![("not-required", StatusState::Success)]);

    assert_ci_failed_actions!(vec![
        ("not-required", StatusState::Success),
        ("required1", StatusState::Error),
        ("required2", StatusState::Success),
    ]);

    assert_ci_failed_actions!(vec![
        ("required1", StatusState::Failure),
        ("not-required", StatusState::Success),
        ("required2", StatusState::Success),
    ]);

    assert_ci_failed_actions!(vec![
        ("required1", StatusState::Pending),
        ("not-required", StatusState::Success),
        ("required2", StatusState::Success),
    ]);
}

#[tokio::test]
async fn review_approval_pr_actions() {
    use ReviewState::{Approved, ChangesRequested, Commented, Pending};
    macro_rules! assert_approved {
        ($approved:expr, $cases:expr) => {{
            let (pr, client, config) = make_context();
            let mut analyzer = make_analyzer(&pr, &client, &config);
            analyzer.reviews = RemoteData::Local($cases);
            assert_eq!(
                analyzer.required_actions().await.unwrap(),
                *Actions::noop()
                    .set_merge($approved)
                    .set_label("reviewed", Presence::should_be_present($approved))
                    .set_label("ci-passed", Presence::Present)
                    .set_label("needs-description", Presence::Absent)
            );
        }};
    }

    // No reviews
    assert_approved!(false, vec![]);

    // One non-approving review
    assert_approved!(false, vec![review(1, Pending)]);
    assert_approved!(false, vec![review(1, ChangesRequested)]);
    assert_approved!(false, vec![review(1, Commented)]);

    // One person approved, another disapproves
    assert_approved!(false, vec![review(1, Approved), review(1, Pending)]);
    assert_approved!(
        false,
        vec![review(1, Approved), review(2, ChangesRequested)]
    );

    // One person approves, another comments
    assert_approved!(true, vec![review(1, Approved), review(2, Commented)]);

    // One person approves
    assert_approved!(true, vec![review(1, Approved)]);

    // One person disapproves and then later approves
    assert_approved!(true, vec![review(1, ChangesRequested), review(1, Approved)]);
}

fn review(user_id: i64, state: ReviewState) -> Review {
    Review { user_id, state }
}