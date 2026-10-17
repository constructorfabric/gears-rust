use github_mirror::domain::error::DomainError;
use github_mirror::domain::scope::SyncScope;

#[test]
fn a_list_turns_on_exactly_the_named_types_with_the_prds_aliases() {
    let scope = SyncScope::parse_list("issues, PRS,gha, ,contributors").unwrap();
    assert!(scope.issues);
    assert!(scope.pull_requests);
    assert!(scope.github_actions);
    assert!(scope.contributors);
    assert!(!scope.commits);
    assert!(!scope.releases);
    assert!(!scope.branches);
    assert!(!scope.labels);
    assert!(!scope.milestones);
    assert!(!scope.security);

    let pulls = SyncScope::parse_list("pulls").unwrap();
    let actions = SyncScope::parse_list("actions").unwrap();
    assert!(pulls.pull_requests && actions.github_actions);
}

#[test]
fn an_unknown_type_is_refused_by_name() {
    let error = SyncScope::parse_list("issues,wiki").unwrap_err();
    assert!(
        matches!(&error, DomainError::Validation { field, message }
            if field == "include" && message.contains("wiki")),
        "{error:?}"
    );
}

#[test]
fn without_turns_off_the_excluded_types_and_leaves_the_rest() {
    let scope = SyncScope::parse_list("issues,prs,commits,releases").unwrap();
    let excluded = SyncScope::parse_list("commits,releases,labels").unwrap();
    let left = scope.without(excluded);
    assert!(left.issues && left.pull_requests);
    assert!(!left.commits && !left.releases && !left.labels);
}
