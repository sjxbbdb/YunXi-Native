//! 引导（OOBE）界面。
//!
//! 版式与构件来自 Miyu 的 `src/oobe/ui`（见 `terminal/ATTRIBUTION.md`）。
//! [`compat`] 是适配层：移植过来的代码按 Miyu 的类型名书写，这里提供同名的
//! YunXi 定义，于是移植只需改导入路径。
//!
//! **当前状态**：`compat` 已接入编译；`ui/*.rs` 与 `vendor_oobe_mod.rs` 是
//! 照搬过来的原样备料，尚未接入模块树（它们还依赖尚未镜像的后端面）。

pub mod compat;

// ↓ 照搬中的模块。**暂时不接入编译**：还缺 Miyu 的 provider 目录、shell
// 安装器、模型缓存那几块后端面（见 compat.rs 的错误清单）。硬接会留下一个
// 编译不过的工作区，所以先留在盘上备料，逐个补齐后再打开。
//
// mod apply;
// mod probe;
// mod providers;
// mod ui;
