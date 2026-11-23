# Source

This repository contains the Agentlaw Rust product: its code, tests, installers,
tool contract, and release workflow. Requirements, architecture decisions and
development records are maintained separately from the public product source.

The workspace checkout is not a build dependency. Published source excludes private
memories, local configuration, model downloads, and machine-specific artifacts.
See the [README](README.md) for the user experience, the
[installation guide](docs/installation.md) for source builds and harness setup,
and the [usage guide](docs/usage.md) for supported commands.
