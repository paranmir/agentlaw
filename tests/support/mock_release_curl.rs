//! Local release endpoint for the old binary's preview/prepare compatibility test.
use std::{env, fs, path::PathBuf};

fn main() {
    let args: Vec<String> = env::args().collect();
    let url = args.last().expect("URL argument");
    if url.ends_with("/releases/latest") {
        println!("{{\"tag_name\":\"v0.3.2\",\"draft\":false,\"prerelease\":false}}");
        return;
    }
    let output = args
        .iter()
        .position(|arg| arg == "--output")
        .and_then(|position| args.get(position + 1))
        .map(PathBuf::from)
        .expect("output argument");
    let source = if url.ends_with("/SHA256SUMS") {
        env::var_os("AGENTLAW_FAKE_SUMS")
    } else {
        env::var_os("AGENTLAW_FAKE_ASSET")
    }
    .expect("fake source path");
    fs::copy(source, output).expect("copy fake release asset");
}
