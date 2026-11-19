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

#[test]
fn split_rechecks_distinct_utf8_ends_after_byte_fallback() {
    use tokenizers::models::bpe::BPE;
    use tokenizers::normalizers::replace::{Replace, ReplacePattern};

    let vocab: tokenizers::models::bpe::Vocab = [
        ("[CLS]", 0),
        ("a", 1),
        ("b", 2),
        ("<0xED>", 3),
        ("<0x95>", 4),
        ("<0x9C>", 5),
    ]
    .into_iter()
    .map(|(text, id)| (text.to_owned(), id))
    .collect();
    let model = BPE::builder()
        .vocab_and_merges(vocab, vec![])
        .byte_fallback(true)
        .build()
        .unwrap();
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_normalizer(Some(
        Replace::new(ReplacePattern::Regex("\u{D55C}$".into()), "aaaa").unwrap(),
    ));
    tokenizer.with_post_processor(Some(
        TemplateProcessing::builder()
            .try_single("[CLS] $A")
            .unwrap()
            .special_tokens(vec![("[CLS]", 0)])
            .build()
            .unwrap(),
    ));
    let window = format!("{}\u{D55C}{}", "a".repeat(508), "b".repeat(12));
    let whole = tokenizer.encode(&*window, false).unwrap();
    assert_eq!(&whole.get_ids()[508..511], &[3, 4, 5]);
    assert_eq!(&whole.get_offsets()[508..511], &[(508, 511); 3]);
    assert_eq!(tokenizer.encode(&window[..511], true).unwrap().len(), 513);
    let ranges = split(&tokenizer, &window).unwrap();
    assert_eq!(ranges, [(0, 508), (508, 523)]);
    for (start, end) in ranges {
        assert!(
            tokenizer
                .encode(&window[start as usize..end as usize], true)
                .unwrap()
                .len()
                <= 512
        );
    }
}

#[test]
fn prefix_disables_tokenizer_automatic_truncation() {
    let mut tokenizer = scalar_tokenizer();
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: 8,
            ..Default::default()
        }))
        .unwrap();
    let query = "ab".repeat(300);
    assert_eq!(tokenizer.encode(query.as_str(), true).unwrap().len(), 8);
    assert_prefix(&tokenizer, &query, 601, true);
}

#[cfg(windows)]
#[test]
#[ignore = "observes the native CPU session builder with the pinned ONNX Runtime library"]
fn cpu_builder_observes_thread_and_batch_settings() {
    const DLL_SHA: &str = "579b636403983254346a5c1d80bd28f1519cd1e284cd204f8d4ff41f8d711559";
    let source =
        PathBuf::from(std::env::var_os("AGENTLAW_ORT_LIBRARY").expect("set AGENTLAW_ORT_LIBRARY"));
    let tmp = tempfile::tempdir().unwrap();
    let dll = tmp.path().join("onnxruntime.dll");
    assert_eq!(
        format!("{:x}", Sha256::digest(std::fs::read(&source).unwrap())),
        DLL_SHA
    );
    std::fs::copy(source, &dll).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(std::fs::read(&dll).unwrap())),
        DLL_SHA
    );
    ort::init_from(dll.to_string_lossy()).commit().unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let cores: usize = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(cores > 0);
    let (_, settings) = cpu_session_builder().unwrap();
    assert_eq!(
        settings,
        CpuSessionSettings {
            intra: cores.min(4),
            inter: Some(1),
            batch: 1
        }
    );
}
