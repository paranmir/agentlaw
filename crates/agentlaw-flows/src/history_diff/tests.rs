use super::*;
#[test]
fn disk_diff_matches_in_memory_reference_including_large_lines() {
    let long = "가".repeat(50000);
    for (i, (a, b)) in [
        ("".into(), "x".into()),
        ("x".into(), "".into()),
        ("keep\r\nold\r\nend".into(), "keep\r\nnew\r\nend".into()),
        (format!("{long}\nold\n"), format!("{long}\nnew\n")),
        ("same\n".into(), "same\n".into()),
        ("a\nb\nc\nd\nend\n".into(), "a\nb\nX\nc\nd\nend\n".into()),
    ]
    .into_iter()
    .enumerate()
    {
        let temp = tempfile::tempdir().unwrap();
        let before = temp.path().join("a");
        let after = temp.path().join("b");
        std::fs::write(&before, &a).unwrap();
        std::fs::write(&after, &b).unwrap();
        let mut result = Vec::new();
        write_diff(
            &before,
            &after,
            &temp.path().join(format!("{i}.sqlite")),
            &mut result,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(result).unwrap(),
            crate::history::full_diff(&a, &b)
        );
    }
}
#[test]
fn string_escape_is_valid_and_preserves_utf8_control_bytes() {
    let original = "한글\n\r\t\0\"\\";
    let mut bytes = Vec::new();
    json_string(&mut original.as_bytes(), &mut bytes).unwrap();
    assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), original);
}
