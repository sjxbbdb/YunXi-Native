Use the `patch` tool for precise file edits. The provider-facing tool protocol is constrained JSON: send one object with `op` (`write`, `delete`, or `move`) and a workspace-relative `path`; include `content` for `write` and `from` for `move`.

Examples:

```json
{"op":"write","path":"notes.txt","content":"hello"}
```

```json
{"op":"delete","path":"notes.txt"}
```

```json
{"op":"move","from":"draft.txt","path":"notes.txt"}
```

Do not emit `*** Begin Patch` / `*** End Patch` Lark text in a provider tool call. YunXi keeps a compatibility parser for legacy local patch messages, but constrained JSON is the only documented provider format.
