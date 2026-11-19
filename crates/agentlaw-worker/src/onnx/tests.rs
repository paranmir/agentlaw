use super::*;
#[test]
fn missing_assets_are_unavailable() {
    let p = PathBuf::from("certainly-nonexistent-agentlaw-model.onnx");
    assert!(matches!(
        OnnxProvider::load(&ModelAssets {
            onnx_model: p.clone(),
            tokenizer_json: p.clone(),
            runtime_library: p
        }),
        Err(EmbeddingError::Unavailable(_))
    ));
}

use crate::model_child::Overflow;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::split::{Split, SplitPattern};
use tokenizers::processors::template::TemplateProcessing;
use tokenizers::{SplitDelimiterBehavior, Tokenizer};

fn scalar_tokenizer() -> Tokenizer {
    let model = WordLevel::builder()
        .vocab(
            [("[CLS]", 0), ("[UNK]", 1), ("a", 2), ("b", 3), ("\n", 4)]
                .into_iter()
                .map(|(token, id)| (token.to_owned(), id))
                .collect(),
        )
        .unk_token("[UNK]".into())
        .build()
        .unwrap();
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(
        Split::new(
            SplitPattern::Regex("(?s:.)".into()),
            SplitDelimiterBehavior::Isolated,
            false,
        )
        .unwrap(),
    ));
    tokenizer.with_post_processor(Some(
        TemplateProcessing::builder()
            .try_single("[CLS] $A")
            .unwrap()
            .special_tokens(vec![("[CLS]", 0)])
            .build()
            .unwrap(),
    ));
    tokenizer
}

#[test]
fn reject_counts_special_token_before_inference_and_accepts_next_input() {
    let tokenizer = scalar_tokenizer();
    let mut calls = Vec::new();
    for (body_tokens, expected) in [(510, 511), (511, 512), (512, 0), (1, 2)] {
        let result = embed(
            &tokenizer,
            &"a".repeat(body_tokens),
            Overflow::Reject,
            |encoding| {
                calls.push(encoding.len());
                Ok(vec![1.0])
            },
        );
        if expected == 0 {
            assert_eq!(result, Err(EmbeddingError::InputTooLong));
        } else {
            assert_eq!(result, Ok((vec![1.0], false)));
        }
    }
    assert_eq!(calls, [511, 512, 2]);
}

fn assert_prefix(tokenizer: &Tokenizer, input: &str, tokens: usize, truncated: bool) {
    let mut unbounded = tokenizer.clone();
    unbounded.with_truncation(None).unwrap();
    let full = unbounded.encode(input, true).unwrap();
    assert_eq!(full.len(), tokens);
    let mut calls = Vec::new();
    let result = embed(tokenizer, input, Overflow::Prefix, |encoding| {
        calls.push(encoding.get_ids().to_vec());
        Ok(vec![1.0])
    });
    assert_eq!(result, Ok((vec![1.0], truncated)));
    assert_eq!(calls, [full.get_ids()[..tokens.min(512)].to_vec()]);
}

#[test]
fn prefix_infers_exact_full_or_truncated_token_ids() {
    let tokenizer = scalar_tokenizer();
    assert_prefix(&tokenizer, &format!("{}a", "ab".repeat(149)), 300, false);
    assert_prefix(&tokenizer, &format!("{}a", "ab".repeat(299)), 600, true);
}

fn release_tokenizer() -> Tokenizer {
    let path = std::env::var_os("AGENTLAW_TOKENIZER").expect("set AGENTLAW_TOKENIZER");
    let bytes = std::fs::read(path).expect("read release tokenizer");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f"
    );
    Tokenizer::from_bytes(bytes).unwrap()
}

#[test]
#[ignore = "requires the release tokenizer via AGENTLAW_TOKENIZER"]
fn prefix_preserves_release_token_ids_after_metaspace_and_newline() {
    let tokenizer = release_tokenizer();
    let input = format!("{}\n{}", "\u{2581}".repeat(12_400), "x".repeat(32_000));
    assert_prefix(&tokenizer, &input, 4_402, true);
}

#[test]
#[ignore = "requires the release tokenizer via AGENTLAW_TOKENIZER"]
fn prefix_does_not_truncate_long_space_run_before_added_token() {
    let tokenizer = release_tokenizer();
    let input = format!("A{}<mask>B", " ".repeat(65_536));
    assert_prefix(&tokenizer, &input, 4, false);
}

#[test]
fn split_honors_delimiter_priority_and_keeps_the_short_remainder_whole() {
    let tokenizer = scalar_tokenizer();
    let cases = [
        ("alpha\nbeta".to_owned(), vec![(0, 10)]),
        (
            format!(
                "{}\n{} {}",
                "a".repeat(300),
                "b".repeat(120),
                "a".repeat(160)
            ),
            vec![(0, 301), (301, 582)],
        ),
        (
            format!("{}\t{}", "a".repeat(400), "b".repeat(180)),
            vec![(0, 401), (401, 581)],
        ),
        (
            format!("{} {}", "a".repeat(400), "b".repeat(180)),
            vec![(0, 401), (401, 581)],
        ),
        (
            format!("{}\n{}", "a".repeat(100), "b".repeat(480)),
            vec![(0, 511), (511, 581)],
        ),
        ("".to_owned(), vec![]),
    ];
    for (text, expected) in cases {
        let pieces = split(&tokenizer, &text).unwrap();
        assert_eq!(pieces, expected);
        let mut offset = 0;
        for (start, end) in pieces {
            assert_eq!(start, offset);
            assert!(start < end);
            assert!(
                tokenizer
                    .encode(&text[start as usize..end as usize], true)
                    .unwrap()
                    .len()
                    <= 512
            );
            offset = end;
        }
        assert_eq!(offset as usize, text.len());
    }
}
