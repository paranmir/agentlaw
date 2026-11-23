//! Compatibility entry retained for installed 0.4.x updaters.
fn main() {
    eprintln!("agentlaw-worker is no longer used by Agentlaw; the runtime runs inside agentlaw.");
    std::process::exit(1);
}
