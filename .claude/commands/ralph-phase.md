Read CLAUDE.md and identify the current build phase.

Run the Ralph Wiggum loop to complete this phase autonomously:

/ralph-loop:ralph-loop "Read CLAUDE.md. Identify the current build phase and its requirements. Implement everything listed for this phase. After each change:
1. Run cargo build (fix errors)
2. Run cargo test (fix failures)  
3. Run cargo clippy (fix warnings)
4. If sidecar work is needed, run cd sidecar && python -m pytest
5. Check all phase requirements are met

When ALL requirements for the current phase are implemented, all tests pass, and clippy is clean, output <promise>PHASE_COMPLETE</promise>" --max-iterations 20 --completion-promise "PHASE_COMPLETE"
