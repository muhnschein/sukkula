# Shared brief for the review-fix round

Read the original brief too: docs/handoff/brief-common.md (binding: code rules, lints, trailers, no push, report format).

Sukkula is now fully integrated (branch `claude/busy-babbage-7y67po`, your worktree starts from it): four protocol adapters, hub/FFI, Qt/QML UI, CI with a Harbour gate, 16 fuzz targets. An independent review, with every finding checked by skeptical second reviewers, produced the findings in
`docs/handoff/review-2026-09-25.json` (`kept[N]`: title, file, line, description, failure_scenario, suggested_fix, status, corrected_severity, and the verifiers' reasoning in `votes`). Your task names the indices you own. For each one:

1. Re-read the code and the verifiers' reasoning; confirm the defect yourself on the current tree. If you conclude it is not a defect, say why in your report and change nothing for it.
2. Fix it at the root, the smallest change that makes the rule hold; keep comment style (explain why, cite spec IDs / finding titles).
3. Add a regression test that fails without the fix (prove it: temporarily revert, run, restore — never leave the revert in) wherever an automated test is possible; otherwise say why and add/adjust a manual test ID only if your task owns `docs/MANUAL-TESTS.md`.
4. Update the one relevant row/paragraph in `docs/SECURITY.md` only if your task says you own that row; otherwise list the doc change you'd want in your report.

Everything must stay green in your worktree before you finish (with `CARGO_BUILD_JOBS=2`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` (if you changed deps, drop `--locked` once, then commit the lock), the per-feature clippy for any engine feature you touched (`-p sukkula-engine --no-default-features --features <f> --all-targets`), `cargo test --workspace --locked`, and the specific gate scripts your area has (listed in your task). Disk is shared and limited: no release builds unless your task needs them, and run `cargo clean` in your worktree after your final check. Work only on the files your task owns; if a fix truly needs a file outside your set, make the minimal change, mark it `CONTRACT:` and list it in your report. Commit with the trailer lines; report per the brief (per finding: fixed / not a defect / partially, the test that proves it, commands run).
