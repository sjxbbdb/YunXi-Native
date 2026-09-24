# YunXi Native development rules

This repository is the Linux-native YunXi product line. The active product code is under `crates/` and `yunxi-agent-linux/`; `references/` contains read-only source snapshots for comparison and must not become runtime dependencies.

- Keep YunXi Persona, Soul, Memory, Approval and Sandbox semantics as the source of truth.
- Keep terminal interception conservative: known shell syntax stays with the shell; natural-language intent goes through the YunXi runtime.
- Never add a destructive system operation without an explicit approval path and a testable dry-run or preview.
- Run `cargo fmt --all --check`, `cargo test --locked`, and `cargo build --release -p yunxi-agent-linux` before claiming a change complete.
- Fish behavior changes require a real fish/PTY regression test on Linux; Rust unit tests alone are insufficient.
- `references/yunxi-agent-windows` and `references/miyu-agent` are technical references, not code to copy without an adapter, license review, and behavior tests.
