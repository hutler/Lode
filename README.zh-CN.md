# Lode

[English](README.md)

<p align="center">
  <img src="docs/logo/lode-banner.png" alt="Lode 字标旁是一位手持青绿色水晶的红发像素矿工" width="592">
</p>

Lode 是一套可自行部署的版本控制系统，适合在同一项目中管理源代码和大型二进制资产的团队。服务器统一记录修订、文件锁以及供评审的搁置变更（shelf）。开发者可以使用命令行，美术人员可以使用 Lode Desk 或 Unreal 编辑器，智能体可以通过 CLI 或 MCP 访问项目。

## 快速开始

从 [GitHub Releases](https://github.com/hutler/Lode/releases) 下载适合当前平台的客户端。如果团队已有服务器和仓库（depot），请先登录，再检出工作区。将以下地址和仓库名替换为团队实际使用的值：

```sh
lode login https://lode.example.com
lode checkout https://lode.example.com/game game
cd game
lode status --remote
```

部署新服务器可参阅 [Linux 部署指南](lode/docs/deploy-linux-for-agents.md) 或[安装与使用手册](lode/docs/user-guide.html)。要导入现有项目，可以用 `lode init` 导入目录中的文件，也可以用 Git 或 SVN 导入工具保留历史记录。[命令参考](lode/docs/CLI-JSON.md)。

## 团队工作流程

- **管理大型项目。** 工作区视图（view）决定检出哪些路径。服务器对内容去重，大文件按块传输变更。团队可以要求在编辑二进制资产前先获取独占锁。
- **评审变更。** 在本地用变更列表（changelist）整理修改，将修改搁置到服务器供评审，再提交已评审的具体版本。仓库策略可要求在提交前通过 CI 和内容校验。
- **集成分支。** 提交是原子操作，各分支共用仓库内的修订号，合并会记录来源。团队可使用流（stream）约束分支间的集成方向。
- **接入自动化。** CLI 输出结构化 JSON。本地 MCP 操作工作区，远程 MCP 可以直接读取仓库并创建搁置变更。智能体使用独立身份和权限，修订记录保留其作者信息。

## 文档导航

详细指南使用英文撰写。可按任务选择入口：

| 任务 | 文档 |
| --- | --- |
| 日常操作 | [用户手册](lode/docs/user-guide.html) · [PDF](lode/docs/Lode-User-Guide.pdf) |
| 智能体在仓库中工作 | [智能体指南与任务导航](docs/agent-guide.md) · [MCP 工具](lode/docs/MCP.md) · [CLI JSON](lode/docs/CLI-JSON.md) |
| 编写提交前检查 | [本地与服务器端校验](docs/agent-validation.md) · [可运行的 Python 示例](lode/examples/validate/) |
| 服务器运维与 CI 集成 | [运维指南](docs/agent-operations.md) · [升级与备份](lode/docs/UPGRADING.md) |
| 桌面客户端与编辑器 | [Desk 指南](desk/docs/guide.md) · [Unreal 指南](unreal/docs/guide.md) |
| 构建与贡献 | [客户端开发与发布](docs/agent-clients.md) · [仓库规则](AGENTS.md) · [系统架构](lode/docs/ARCHITECTURE.md) |

## 从源码构建

[核心组件](lode/)需要 Rust 1.88 或更高版本。在 Windows 上还需要 MSVC C++ 构建工具。

```sh
cd lode
cargo build --workspace
cargo test --workspace
```

[Lode Desk](desk/) 是独立的 Rust 工作区，固定依赖特定的核心提交，有独立的 CI 和发布标签，需要 Rust 1.92 或更高版本。[Unreal 插件](unreal/)调用已安装的 CLI 并读取其 JSON 输出。可下载的版本见[发布页面](https://github.com/hutler/Lode/releases)，实现进度见[任务索引](lode/docs/tasks/README.md)。

Lode 采用 [Apache 2.0 许可证](LICENSE)。
