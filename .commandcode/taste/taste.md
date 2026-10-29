# Taste

- Does not want `make check` run as part of the workflow; the verification gate is `cargo test` passing instead. Confidence: 0.75
- Wants tightly scoped context gathering: read the named doc/files only, not a blind sweep of the repo ("do NOT explore the whole repo blindly"). Confidence: 0.6
