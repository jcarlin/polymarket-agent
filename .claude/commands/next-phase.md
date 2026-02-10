Review all work completed so far against the current phase requirements in CLAUDE.md.

1. Run `cargo build` and `cargo test` — report results
2. Run `cargo clippy` — report any warnings
3. If a Python sidecar exists, run `cd sidecar && python -m pytest` — report results
4. Check that all files listed in the current phase exist and are non-trivial
5. Summarize what is done and what is working

If everything passes:
- Update CLAUDE.md: change the "Current Phase" line at the top to the NEXT phase
- Show me the new phase name and its requirements
- Ask me to confirm before starting any work on the next phase

If tests fail or work is incomplete:
- List exactly what is missing or broken
- Do NOT advance the phase
- Ask if I want to fix the issues first
