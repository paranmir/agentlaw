use sha2::{Digest, Sha256};

/// Retained scan receipts are bound to these bytes. Change this value only when the
/// scanner's behavior changes on purpose, which invalidates those receipts.
const SCANNER_SOURCE_SHA256: &str =
    "09d08ae978d72ab6737ad1fae2b35833121aefc912d82a32ef90a6b08a69fe34";

#[test]
fn scanner_source_is_unchanged() {
    // Compare with LF endings, so a CRLF checkout gives the same answer as a release build.
    let source = String::from_utf8(include_bytes!("git_scan.rs").to_vec())
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(
        format!("{:x}", Sha256::digest(source)),
        SCANNER_SOURCE_SHA256
    );
}
