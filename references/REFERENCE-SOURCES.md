# Technical reference snapshots

This repository carries two read-only snapshots so Linux development can be reviewed against the source that shaped the design.

## YunXi Agent Windows source

- Path: `references/yunxi-agent-windows/`
- Source repository: `https://github.com/sjxbbdb/YunXi-Agent`
- Snapshot commit: `9e83e9567e2b90c62968125698ea0f88f759e37f`
- Role: understand the shared YunXi Runtime, Persona/Soul, Memory, Provider, Tool, Approval and Sandbox contracts.

The snapshot is not a dependency and is not built by the Linux workspace. Windows-only Web, voice, Weixin and task-scheduler surfaces remain reference material only.

## Miyu Agent source

- Path: `references/miyu-agent/`
- Source repository: `https://github.com/SHORiN-KiWATA/miyu-agent`
- Version: `0.6.2`
- Pinned commit: `04a23ccbfc1ee081ec8e2d82090edfa553552456`
- License: MIT; the original `LICENSE` file is retained in the snapshot.

Miyu code is used to study Linux-native shell takeover, daemon lifecycle, IPC, event replay, XDG paths, sandboxing and Arch-oriented tools. It is not silently linked into YunXi. Any future adaptation must retain attribution, preserve license notices, pass a YunXi adapter boundary, and add behavior/security tests.
