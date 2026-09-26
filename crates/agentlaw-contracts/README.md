# Agentlaw contracts (Rust rewrite)

Implements all four action input shapes from design input schema v0.1. `parse_request`
is the external ingress: it rejects duplicate JSON object keys, verifies the exact
bundled schema, then deserializes typed requests. Direct serde deserialization does
not perform cross-field shape validation and is not an ingress API.

The schema interpreter covers only the checked-in schema vocabulary and rejects
unknown schema keywords instead of silently treating new constraints as accepted.
Its tests consume all checked-in positive and negative design examples. Numeric
counts use checked `u32` decoding; shape-valid values exceeding runtime bounds are
reported as invalid input rather than clamped.

Shared internal values are owned snapshots, context, resolved applicability, read
sets, and prepared intents. Portable memory references are explicit two-key
objects; store binding is separate. Prepared intents do not authorize publication
without storage's final all-head/read-set revalidation.

This crate does not claim semantic correctness, durability, harness support,
complete history response implementation, or closure of design HOLD decisions.
