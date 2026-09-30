# Agentlaw 0.3.6

- Groups identical recall clues from lexical and vector discovery into one
  retrieval path within a memory candidate, retaining each actual channel.
- Preserves later discoveries of the same memory across search views, current
  heads, related sources, active Tasks and work targets. Different clues and
  source memory IDs remain distinct; first previews and ordering are preserved.
- Uses the same grouped paths in active Task projections and complete response
  artifacts. Candidate counts/limits, full memory delivery and required
  references keep their existing boundaries.

**Response compatibility:** `retrieval_paths[].via` changes from a string to an
array of strings, including paths with only one channel. Consumers of that
field must accept the new format. Tool requests and persisted memories are
unchanged; no new settings or dependencies are introduced.

A representative candidate JSON decreased from 749 to 651 UTF-8 bytes (13.1%).
This is a fixture byte comparison, not a measured tokenizer count or a promise
for every response. Preserving previously discarded paths can increase other
responses. Real-model retrieval quality, LLM adherence and performance are
outside this change's verified scope.
