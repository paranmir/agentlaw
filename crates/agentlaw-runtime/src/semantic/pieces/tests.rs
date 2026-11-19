use super::*;
use std::io::BufReader;

#[test]
fn windows_are_utf8_safe_and_independent_of_reader_buffering() {
    let fixtures = [
        ("".to_owned(), vec![]),
        ("alpha\nbeta".to_owned(), vec![10]),
        (format!("{}한글", "a".repeat(65_535)), vec![65_535, 6]),
        (
            format!(
                "{}\n{}\t{}",
                "a".repeat(40_000),
                "b".repeat(9_998),
                "a".repeat(25_000)
            ),
            vec![40_001, 34_999],
        ),
        (
            format!("{}\t{}", "a".repeat(49_999), "b".repeat(25_000)),
            vec![50_000, 25_000],
        ),
        (
            format!("{} {}", "a".repeat(49_999), "b".repeat(25_000)),
            vec![50_000, 25_000],
        ),
        (
            format!("{}\n{}", "a".repeat(100), "b".repeat(70_000)),
            vec![65_536, 4_565],
        ),
        (
            format!("{}\n{}", "a".repeat(40_000), "b".repeat(25_535)),
            vec![65_536],
        ),
    ];
    for (text, lengths) in fixtures {
        let mut offset = 0;
        let expected: Vec<_> = lengths
            .into_iter()
            .map(|len| {
                let window = &text[offset..offset + len];
                offset += len;
                window
            })
            .collect();
        assert_eq!(offset, text.len());
        assert_eq!(windows(&text).collect::<Vec<_>>(), expected);
        for capacity in [1, 4096, 1_048_576] {
            let reader = BufReader::with_capacity(capacity, text.as_bytes());
            let actual = read_windows(reader)
                .collect::<io::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(actual, expected, "reader buffer {capacity}");
        }
    }
}
