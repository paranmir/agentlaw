use super::*;
#[test]
fn diff_preserves_crlf_and_absent_final_newline() {
    let d = full_diff("keep\r\nold\r\nend", "keep\r\nnew\r\nend");
    assert!(d.contains(" keep\r\n-old\r\n+new\r\n end\n\\ No newline at end of file\n"));
    assert!(full_diff("", "x").contains("@@ -0,0 +1,1 @@\n+x\n\\ No newline"));
    assert!(full_diff("x", "").contains("@@ -1,1 +0,0 @@\n-x\n\\ No newline"));
}
#[test]
fn unchanged_prefix_suffix_are_not_copied_wholesale() {
    let before = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
    let after = before.replace("line 50\n", "changed\n");
    let d = full_diff(&before, &after);
    assert!(d.len() < 200);
    assert!(d.contains("-line 50\n+changed\n"));
    assert!(d.contains("@@ -48,7 +48,7 @@"));
    assert_eq!(full_diff(&before, &before), "");
}
