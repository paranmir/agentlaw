Agentlaw's Rust memory runtime replaces the previous Python/governance seed.

- One MCP tool and a matching CLI for recall, explicit memory updates, history and project connections.
- Complete Markdown current memory, retained history and rebuildable lexical/vector indexes.
- Recoverable writes, conflict review, explicit Git sharing and a supervised embedding worker.
- Native executables for Windows x64, Linux x64 and macOS (Apple Silicon and Intel).
- Shell and PowerShell installers verify archive checksums. No administrator access is required.

Install with the attached `install.sh` or `install.ps1`, or download the archive
for your platform. See the README for curl, wget, PowerShell and source-build commands.

This is the first Rust development release, not a migration of existing Python
data. Model/tokenizer/ONNX Runtime setup and explicit harness configuration remain
separate. Existing memories and active harness settings are not automatically changed.
The release workflow gates publication on the ordinary test suite for each target;
real-model, live-harness and large-corpus validation have separate limitations in
`docs/verification.md`.
