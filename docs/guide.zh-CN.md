[English](guide.md) | **简体中文** · [项目首页](../README.zh-CN.md)

# 使用与开发指南

## 安装

从 [GitHub Releases](https://github.com/bgluminous/my-ai-assistant/releases)
下载 Windows x64 安装包、便携版或 macOS Apple Silicon 版。安装包未做系统代码签名。
Windows 需要 [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)。
macOS 用户请将应用从 DMG 拖入可写的「应用程序」目录，以便后续更新。

安装版可在「关于」中查看更新日志并升级，自动检查可在「设置」关闭。
Windows 便携版请从 Releases 下载新版。

## 账户管理

点击「账户管理」右上角的 **＋**，选择服务并添加账户。每行显示套餐、额度和上次刷新时间。
拖动左侧手柄即可调整主窗口与托盘中的账户顺序。

| 服务            | 添加方式                                      | 用量统计来源                                       |
| --------------- | --------------------------------------------- | -------------------------------------------------- |
| Cursor          | 本机登录或 `WorkosCursorSessionToken`         | 按账户归档的用量事件和实扣明细                     |
| ChatGPT / Codex | 本机登录、Token、`auth.json` 文件或粘贴 JSON  | Codex 会话日志，默认在 `~/.codex/sessions`         |
| Claude          | 浏览器 OAuth 授权、本机登录或 Token           | Claude Code 会话日志，默认在 `~/.claude/projects`  |
| OpenCode        | 导入 V2 客户端的工作区，或单独进行 OAuth 授权 | 整个工作区的官方请求记录，需要 Owner 或 Admin 权限 |

导入或粘贴 ChatGPT 的 `auth.json` 时，至少需要 `tokens.access_token`。
点击行内切换按钮可更换本机登录。Cursor 会启动客户端。GPT / Codex、Claude 和 OpenCode
可选「仅 CLI」「仅 Desktop」或「两端一起」。已有 CLI 会话会继续运行。
切换 Desktop 时请先保存工作。两端共用凭据时，另一端下次读取登录信息也可能使用新账户。
切换成功后，结果弹窗会在 3 秒后自动关闭。

首次切换 Claude Desktop 账户时，按提示开始绑定，并在官方客户端登录所选账户。
完成或取消绑定时，应用会恢复绑定前的登录。如果绑定中断，请先保存 Claude Desktop 中的工作，
再从该账户的编辑窗口或切换窗口点击「恢复原登录」。恢复时会关闭 Claude Desktop。

可手动刷新单个或全部账户，也可在「设置」设定刷新周期，或为某个账户单独指定周期。
ChatGPT、Claude 和 OpenCode 支持按账户关闭 Token 自动续期。
OpenCode 的套餐、授权及历史覆盖范围见[接入说明](opencode-integration.md)。

## 用量统计

在「用量统计」选择一个或多个来源，按日、周、月、全部或自定义日期查看趋势。
图表可按模型、来源，以及输入、输出、缓存读写拆分。

- **等价费用**按每百万 Token 的模型 API 单价折算。套餐倍数用所选期间的等价费用除以
  已知套餐的完整月费之和。未定价模型仍计入 Token 总数。
- **原始账单**可查看 Cursor 的逐笔实扣或 OpenCode 的官方记录成本，按模型筛选并导出 CSV。
  OpenCode Go 套餐的实际扣款请以官方钱包流水为准。单个 Cursor 账户还可导出 PNG 用量快照。
  为 Cursor 账户开启「不记录 API 额度用量」会清除本机已有的匹配事件，并过滤后续同步。
- **价格表**可在窗口右上角查看、在线更新或手动改价。自定义单价优先于在线表和内置表。
  默认单价采用美元标准档，长上下文、分时价格及多模态附加费用的覆盖范围见
  [内置价格表](../src-tauri/resources/pricing.default.json)。

## 托盘

单击托盘图标即可查看额度、切换账户，或在「总览」查看当天用量。双击返回主窗口。
关闭主窗口后，定时刷新会在托盘继续运行。要结束程序，请在托盘菜单选择「退出」。

## 配置与数据

- **本机存储。** Windows 数据目录为 `%USERPROFILE%\.xilore\myaiassistant\`，
  macOS 为 `~/.xilore/myaiassistant/`。`settings.json` 保存账户及设置。
  Cursor 和 Claude 的 Token 为明文，ChatGPT 的完整 Auth 和 OpenCode 独立授权使用内置密钥加密。
  请像保管登录凭据一样保管该目录、账户导出和备份文件。
- **备份与恢复。** 在「设置」导出全量备份时可设置密码。导入时按身份合并账户并应用备份中的设置。
  删除 Cursor 账户时默认保留统计，这些记录和 OpenCode 归档都随全量备份迁移。
  在用 Cursor 账户的历史可刷新重建。
  删除 OpenCode 账户会一并删除本机归档。Claude Desktop 登录绑定仅保存在当前机器。
- **GPT 跨设备同步。** 默认仅在本机统计。在「设置」的「GPT 用量同步」中填写自己的 HTTPS
  服务器和同步密钥，测试连接后点击「完成」。同步内容限于模型、时间、Token 数和会话标识，
  服务器可读取这些明细。自建服务所需的接口说明可在该页点击「复制提示词」获取。

  - 同步账户被停用时，自动同步会暂停。请管理员重新启用账户，再到「用量统计」点击「刷新」。
  - 密钥重置后，在使用该同步账户的每台设备上填入管理员提供的新密钥，测试连接并点击「完成」。
    服务端会保留原有的用量历史。
- **网络与审计。** 账户查询访问对应服务商，默认价格表和应用更新来自 GitHub，
  代理连接测试使用 `inf.xil.to`。可在系统代理、直连和自定义 HTTP / SOCKS 代理之间切换。
  「审计日志」可查找刷新失败、账户切换和数据导出记录。

## 构建与发布

前端位于 `src/`，Tauri 2 的 Rust 后端位于 `src-tauri/`。
从源码运行需要 Node.js 20.19+ 和 Rust。Windows 需安装 Visual Studio Build Tools 的
「使用 C++ 的桌面开发」组件和 Windows SDK。macOS 需先执行 `xcode-select --install`。

```bash
npm install
npm run tauri dev
```

`npm run tauri build` 构建当前平台的安装包。Windows 也可用 `npm run release:windows`
将安装包和便携版整理到 `release/`。该脚本成功后默认清理 `src-tauri/target`，
保留构建缓存等选项见[打包脚本](../scripts/build-release-windows.ps1)。

推送 `Release-X.Y.Z` 分支会正式发布，手动运行 Actions 仅构建。更新签名需要
`TAURI_SIGNING_PRIVATE_KEY` 和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，
正式发布还需 `RELEASE_REGISTRY_TOKEN`。配置与重跑步骤见
[发布工作流](../.github/workflows/release.yml)，各版本的变更见[更新日志](releases)。

## 声明

本项目采用 GNU GPLv3，引用或衍生请注明 BGLuminous 与
[项目出处](https://github.com/bgluminous/my-ai-assistant)。再分发条款见 [LICENSE](../LICENSE)。

本项目独立于各服务商，部分功能依赖非公开接口，字段和可用性可能变化。
请仅查询自己的账户与用量，并遵守相应服务条款。因接口变化、账户限制或条款冲突产生的后果由使用者承担。
