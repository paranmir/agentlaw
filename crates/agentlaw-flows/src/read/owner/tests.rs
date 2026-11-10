use super::*;
fn hit(id: &str, score: f64) -> Hit {
    Hit {
        memory_id: id.into(),
        change_id: "version".into(),
        score,
    }
}
#[test]
fn full_selection_accepts_a_single_clear_channel_but_not_conflicting_winners() {
    assert_eq!(
        uncontested(&[hit("a", 0.96), hit("b", 0.89)], &[]).as_deref(),
        Some("a")
    );
    assert_eq!(uncontested(&[], &[hit("a", 0.95)]).as_deref(), Some("a"));
    assert_eq!(uncontested(&[hit("a", 0.96)], &[hit("b", 0.96)]), None);
    assert_eq!(uncontested(&[hit("a", 0.94), hit("b", 0.92)], &[]), None);
}
