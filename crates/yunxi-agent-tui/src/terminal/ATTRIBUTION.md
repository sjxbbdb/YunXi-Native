# 归属与许可证

本目录下的代码移植自 **Miyu Agent**。

- 来源仓库：<https://github.com/SHORiN-KiWATA/miyu-agent>
- 版本：`0.6.2`
- 固定提交：`04a23ccbfc1ee081ec8e2d82090edfa553552456`
- 本仓库内的只读快照：`references/miyu-agent/`
- 许可证：**MIT License, Copyright (c) 2026 SHORiN-KiWATA**（原 `LICENSE` 文件保留在快照里）

## MIT 许可证声明

```
MIT License

Copyright (c) 2026 SHORiN-KiWATA

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## 逐文件对应关系

| 本目录 | Miyu 源 | 说明 |
|---|---|---|
| `palette.rs` | `crates/miyu-base/src/terminal/palette.rs` | 机制照搬；**配色换成用户原创角色的 240° 蓝紫色阶** |

## 本仓库对参考代码的约束（`references/REFERENCE-SOURCES.md`）

> Miyu code is used to study … It is **not silently linked into YunXi**. Any future
> adaptation must **retain attribution, preserve license notices, pass a YunXi
> adapter boundary, and add behavior/security tests**.

对照落地情况：

| 要求 | 落地 |
|---|---|
| 保留归属 | 本文件 |
| 保留许可证声明 | 本文件 + 各移植文件的头部注释 |
| 过 YunXi 适配层 | `crates/yunxi-agent-tui/src/terminal/` 作为独立模块；Miyu 类型不外泄到既有代码 |
| 加行为/安全测试 | 各移植文件自带 `#[cfg(test)]`，覆盖色深降级、ASCII 兜底、版式度量 |
