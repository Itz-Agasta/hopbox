# hopbox

Rust CLI that syncs a repo to a remote machine over SSH, runs a command there, and streams the result back. VISION.md is the accept/resist test for every feature; check a proposal against it before building.

## Session protocol

1. **Start**: read `notes/handover.md`, then whatever it points at (`notes/plan.md`, `notes/design.md`, `notes/research/`).
2. **End**: rewrite `notes/handover.md` (state, next step, open questions) and append a dated entry to `notes/log.md`.

`notes/` is private: excluded through `.git/info/exclude`, never committed, never linked or named from public files. Plans, research, design rationale, and handovers live there. `docs/` is public user documentation only.

## Build loop

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run
```

All three pass before every commit. Work test-first with the superpowers TDD skill: the test goes red, then green, then refactor.

## Code rules

- Shell out to the system `ssh` and `rsync` through `std::process::Command`, passing arguments as a vector. Build remote shell strings in one module with one quoting function, and unit-test it.
- Synchronous code with threads for streaming. Add an async runtime only after an ADR-level discussion in `notes/decisions.md`.
- `thiserror` for library error types, `anyhow` at the binary edge.
- Tests never rent paid compute. Provider tests use recorded `vastai --raw` JSON fixtures.
- Integration tests run against a throwaway sshd container, never against a real host.

## Git and review

- `main` holds the initial commit only; all work goes through branches named `feat/…`, `fix/…`, `docs/…` and a pull request.
- An AI reviewer comments on each PR. Handle its comments with the `check-pr-comments` skill, and push fixes to the same branch.
- Merging is the user's call.
- Commit messages and PR bodies end with their own content: no `Co-Authored-By: Claude` trailer, no "Generated with Claude Code" line.

## Public docs

Write `docs/` pages in Vercel's technical-writer voice: lead with the answer, verify every flag and default against the code, active voice, sentence-case headings, no em dashes. Document only shipped behavior. Run the `writing-guidelines` skill on changed pages before opening a docs PR.
