# YunXi Native Linux 打包入口

`packaging/arch/yunxi-native/PKGBUILD` 是面向 Arch Linux 的源码构建包，当前定位为
可审计的发行工程骨架，不是已经发布到 AUR 的二进制包。

用户级 service 默认对 daemon 自身设置保守的资源边界：`MemoryHigh=1536M`、
`MemoryMax=2G`、`TasksMax=128`、`LimitNOFILE=4096`，并在越界触发 OOM 时停止该服务。
这些限制只约束 YunXi daemon，不会替外部 Provider 进程设限；需要调整时应在本机复制
service 到用户配置目录后显式覆盖，不修改打包文件中的安全基线。

在没有 Arch `makepkg` 的环境中，可以先运行静态打包验收，检查源码 pin、安装路径、
user service 安全项以及“不自动启用服务”的边界：

```bash
bash packaging/arch/yunxi-native/package-smoke.sh
```

在具备 `systemd-analyze` 的 Linux 环境中，还可以验证 user service 的完整语法和
绝对 `ExecStart` 路径；脚本只使用临时 root，不启动服务：

```bash
bash packaging/arch/yunxi-native/systemd-unit-smoke.sh
```

## 构建与安装

在 Arch Linux 上准备 `base-devel`、`git` 和 `cargo` 后执行：

```bash
cd packaging/arch/yunxi-native
makepkg -si --syncdeps
```

PKGBUILD 固定 `_commit`，构建只产出 `yunxi-linux`，安装到 `/usr/bin/yunxi-linux`，并把
用户级服务安装到 `/usr/lib/systemd/user/yunxi-linux.service`。服务以当前登录用户运行，
不创建 root daemon、不开放 TCP 端口，也不会在安装时自动启用：

```bash
systemctl --user daemon-reload
systemctl --user enable --now yunxi-linux.service
```

首次使用 fish 接管时，由用户显式安装 hook：

```bash
mkdir -p ~/.config/fish/conf.d
yunxi-linux fish-init --print > ~/.config/fish/conf.d/yunxi.fish
```

如需让 YunXi 接手所有非空提交，可改用 `yunxi-linux fish-init --takeover`；不带该
参数时保持保守分类模式，便于在首次安装后验证和回退。

## 卸载与数据边界

```bash
systemctl --user disable --now yunxi-linux.service
sudo pacman -Rns yunxi-native
```

卸载不会删除 `~/.local/state/yunxi`、工作区 `.yunxi/`、长期记忆或知识库，也不会删除
fish hook；如需清理，必须由用户按路径显式处理。升级沿用 pacman 的包事务，数据迁移和
回滚策略仍在 Phase 6 后续增量中，不把当前 PKGBUILD 宣称为最终发布流水线。

## 明确不包含

此包不包含 Windows/Web/微信/语音运行时，不自动下载模型，不把知识库或个人记忆上传到
远程服务。知识库与长期记忆继续使用独立的 SQLite 文件和权限边界。
