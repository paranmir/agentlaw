use super::*;
use serde_json::json;

#[test]
fn bounded_progress_retains_latest_phase() {
    let id = RequestId::new();
    let mailbox = Mailbox::new(id, 1);
    mailbox.publish_progress("first");
    mailbox.publish_progress("middle");
    mailbox.publish_progress("latest");
    assert_eq!(
        mailbox.receive(Duration::from_secs(1)),
        Some(DataFrame::Progress {
            id,
            phase: "latest".into()
        })
    );
    assert_eq!(mailbox.receive(Duration::from_millis(20)), None);
}

#[test]
fn execution_has_one_terminal_slot() {
    let id = RequestId::new();
    let mailbox = Mailbox::new(id, 1);
    let terminal = DataFrame::Completed {
        id,
        result: json!({"ok": true}),
    };
    assert!(mailbox.publish_terminal(terminal.clone()));
    assert!(!mailbox.publish_terminal(terminal.clone()));
    assert_eq!(mailbox.receive(Duration::from_secs(1)), Some(terminal));
    assert_eq!(mailbox.receive(Duration::from_millis(20)), None);
}
