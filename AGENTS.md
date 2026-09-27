# YunXi Native development rules

This repository is the Linux-native YunXi product line. The active product code is under `crates/` and `yunxi-agent-linux/`; `references/` contains read-only source snapshots for comparison and must not become runtime dependencies.

- Keep YunXi Persona, Soul, Memory, Approval and Sandbox semantics as the source of truth.
- **Hard storage boundary:** the long-term memory vector store and the knowledge-base vector store are separate systems. They must not share a database file, table, index namespace, migration, or write path; memory records must never be indexed as knowledge, and knowledge chunks must never be written as memory.
- Any shared vector abstraction must carry an explicit store/domain type and have isolation tests covering paths, schemas, retrieval filters, deletion and generation rollover.
- Fish is an all-takeover integration: every non-empty submitted buffer must go through the YunXi runtime. Fish may keep line editing, history, completion, empty submissions, and edit-time cancellation, but must not classify or locally execute a non-empty buffer before YunXi receives it.
- Never add a destructive system operation without an explicit approval path and a testable dry-run or preview.
- Run `cargo fmt --all --check`, `cargo test --locked`, and `cargo build --release -p yunxi-agent-linux` before claiming a change complete.
- Fish behavior changes require a real fish/PTY regression test on Linux; Rust unit tests alone are insufficient.
- `references/yunxi-agent-windows` and `references/miyu-agent` are technical references, not code to copy without an adapter, license review, and behavior tests.
