use persisting_pchronicle::document::{InputIssue, InputIssueKind};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_invalid_issue_preserves_arbitrary_message(message in any::<String>()) {
        let issue = InputIssue::invalid(message.clone());
        prop_assert_eq!(issue.kind(), InputIssueKind::Invalid);
        prop_assert_eq!(issue.message(), message.as_str());
        prop_assert_eq!(issue.location(), None);
        prop_assert_eq!(issue.to_string(), message);
    }

    #[test]
    fn public_unsupported_issue_preserves_arbitrary_message(message in any::<String>()) {
        let issue = InputIssue::unsupported(message.clone());
        prop_assert_eq!(issue.kind(), InputIssueKind::Unsupported);
        prop_assert_eq!(issue.message(), message.as_str());
        prop_assert_eq!(issue.location(), None);
        prop_assert_eq!(issue.to_string(), message);
    }

    #[test]
    fn public_location_attachment_is_composable(
        message in any::<String>(),
        first in any::<String>(),
        second in any::<String>(),
    ) {
        let issue = InputIssue::invalid(message.clone()).at(first).at(second.clone());
        prop_assert_eq!(issue.kind(), InputIssueKind::Invalid);
        prop_assert_eq!(issue.message(), message.as_str());
        prop_assert_eq!(issue.location(), Some(second.as_str()));
        prop_assert_eq!(issue.to_string(), format!("{message} ({second})"));
    }

    #[test]
    fn public_location_update_never_changes_issue_kind(
        message in any::<String>(),
        location in any::<String>(),
        unsupported in any::<bool>(),
    ) {
        let issue = if unsupported {
            InputIssue::unsupported(message)
        } else {
            InputIssue::invalid(message)
        };
        let expected_kind = issue.kind();
        prop_assert_eq!(issue.at(location).kind(), expected_kind);
    }

    #[test]
    fn public_location_chain_keeps_only_the_last_attached_location(
        message in any::<String>(),
        locations in proptest::collection::vec(any::<String>(), 1..8),
    ) {
        let issue = locations.iter().fold(InputIssue::invalid(message.clone()), |issue, location| {
            issue.at(location.clone())
        });
        prop_assert_eq!(issue.message(), message.as_str());
        prop_assert_eq!(issue.location(), locations.last().map(String::as_str));
    }
}
