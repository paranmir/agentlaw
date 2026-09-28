# Source

This repository contains the Agentlaw Rust product: its code, tests, installers,
tool contract, and release workflow. Requirements and architecture decisions are
maintained in the separate [Agentlaw Workspace](https://github.com/paranmir/agentlaw-workspace).

The workspace checkout is not a build dependency. Published source excludes private
memories, local configuration, model downloads, and machine-specific artifacts.
See the [README](README.md) for the user experience and
[verification](docs/verification.md) for tested behavior.
