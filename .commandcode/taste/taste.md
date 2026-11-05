# Taste

- Does not want `make check` run on every change: the inner loop is nextest for the crates the diff touches (`./tools/ai-test.sh`), and `make check` is the deliberate outer loop when a change needs the whole suite. Confidence: 0.85 (matches `AGENTS.md` §Build and verification)
- Wants tightly scoped context gathering: read the named doc/files only, not a blind sweep of the repo ("do NOT explore the whole repo blindly"). Confidence: 0.6
- Expects repo-wide search/inventory work to be delegated to a subagent ("use subagent to help you") instead of done inline. Confidence: 0.55
