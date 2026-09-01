<p align="center">
  <img src="app-icon.png" width="96" height="96" alt="my-ai-assistant">
</p>

# my-ai-assistant

**my-ai-assistant** 是一个本地桌面程序，用于管理 Cursor 与 ChatGPT 账户、查询套餐与额度，并按模型汇总 token 用量与费用。

程序基于 [Tauri](https://tauri.app/) 2，前端为原生 HTML / CSS / JavaScript，图表使用 [Chart.js](https://www.chartjs.org/)，网络请求由 Rust 侧 `reqwest` 发出。当前面向 **Windows** 与 **macOS**。

HTTP 请求只发往 Cursor / OpenAI 官方域名。账户凭据保存在本机，不经过第三方服务。

> 用量与账户状态依赖 Cursor、OpenAI 的非公开接口，字段与可用性可能随时变化。使用前请自行核对各服务条款。

## 目录

- [功能](#功能)
- [运行要求](#运行要求)
- [构建](#构建)
- [配置](#配置)
- [价格表](#价格表)
- [数据来源](#数据来源)
- [仓库布局](#仓库布局)
- [声明](#声明)

## 功能

### 账户管理

- 分别维护 Cursor 与 ChatGPT 账户：添加、编辑、删除、刷新；可按类型导出/导入 JSON，也可从本机已登录客户端导入（仅导入当前类型）。
- 列表展示存活状态、套餐与有效期、额度摘要、按需消费（Cursor）或 Credits（ChatGPT），以及上次刷新时间。
- 支持按间隔定时刷新全部账户，也可手动刷新单个或一组账户。
- 可将本机 Cursor / ChatGPT（或 Codex）客户端切换到指定账户并启动。客户端路径可手动指定，也可自动搜索常见安装位置。
- ChatGPT 账户可保存 `refresh_token`。access token 临近过期，或请求返回 401 / 403 时自动续期，并写回本机凭据。

### 用量统计

- 数据来自已保存的账户，进入页面后自动拉取；结果有短期缓存（与托盘总览共用），可随时手动刷新。
- 托盘总览优先复用用量页的按日数据：今日数字与来源饼图取当天切片，柱状图展示近 7 日 Token。
- 用量页的自动更新间隔与账户状态刷新间隔相互独立。
- **总览**：逐账户列出总 token、实际支出与按官方 API 价折算的等价费用；按日 Token 堆叠柱状图（按账户分段），并按模型绘制柱状图、环形图与明细表。
- **单个 Cursor 账户**：按时间范围拉取用量，按模型聚合输入 / 输出 / 缓存读 / 缓存写 token，同时给出实扣金额与等价费用。
- **本机 ChatGPT 会话**：扫描 Codex CLI 会话日志（默认 `~/.codex/sessions`），按模型聚合 token 并折算等价费用。解析按文件修改时间增量进行。

### 其它

- **审计日志**：记录账户增删改、导入导出、存活状态变化、刷新失败、ChatGPT 续期结果、定时间隔变更等。日志为 JSON Lines，超限后轮转并保留最近约 2000 条。页面可按类型筛选或清空。
- **系统托盘**：关闭主窗口时转入托盘，定时刷新继续运行。左键单击打开账户面板，双击打开主窗口；面板失焦或再次单击后隐藏。托盘菜单提供显示主窗口与退出；只有「退出」结束进程。
- **显示**：深色 / 浅色主题；token 数量可在完整数字、K·M·B、万·亿之间切换。
- **网络代理**：系统代理、直连或自定义 HTTP / SOCKS 代理，保存后立即生效。
- **单实例**：重复启动只唤出已有主窗口，避免并发读写配置文件。

## 运行要求

- [Node.js](https://nodejs.org/) 18 或更高（用于 `@tauri-apps/cli`）。
- [Rust](https://rustup.rs/) 工具链（`rustup`）。
- 平台链接器：
  - **Windows（推荐）**：Visual Studio Build Tools，勾选「使用 C++ 的桌面开发」（MSVC 与 Windows SDK）。默认目标为 `stable-x86_64-pc-windows-msvc`。
  - **Windows（GNU）**：见下文 [Windows GNU 工具链](#windows-gnu-工具链)。
  - **macOS**：`xcode-select --install`。
- **Windows** 需要 [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/) 运行时。Windows 11 通常已预装。

## 构建

```bash
npm install
npm run tauri dev      # 开发运行
npm run tauri build    # 打包当前平台安装包
```

Windows 发布归档（校验 `package.json` / `tauri.conf.json` / `Cargo.toml` 版本一致后构建，收集便携版 / NSIS / MSI 并打成 7z，随后删除 `src-tauri/target`）：

```powershell
npm run release:windows
```

重新生成应用图标：

```bash
npm run icons          # 等价于 tauri icon app-icon.png
```

### Windows GNU 工具链

在不便安装 MSVC 时，可使用 `stable-x86_64-pc-windows-gnu`。需要另备 MinGW-w64，并保证 `dlltool.exe`、`gcc.exe`、`windres.exe` 在 `PATH` 中（仅安装 rustup 的 GNU 工具链不够：`windows-sys` 等 crate 会调用 `dlltool`）。

PowerShell 示例（MinGW 路径按实际解压位置修改）：

```powershell
$env:Path = "$env:LOCALAPPDATA\mingw64\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"
npm run tauri dev      # 或 npm run tauri build
```

GNU 链接阶段可能出现 `.rsrc merge failure: multiple non-default manifests`，一般不影响生成可执行文件。MSVC 工具链无此告警。

## 配置

用户数据目录：

| 平台 | 路径 |
| --- | --- |
| Windows | `%USERPROFILE%\xilore\myaiassistant\` |
| macOS / 其它 | `~/xilore/myaiassistant/` |

| 文件 | 内容 |
| --- | --- |
| `settings.json` | 账户、刷新间隔、代理、价格覆盖、Cursor / ChatGPT 客户端路径 |
| `audit.jsonl` | 审计日志 |

应用内可复制上述路径。

`settings.json` 以临时文件写入后原子替换。启动时若读取失败（例如文件被短暂占用）会重试，不会用空数据覆盖原文件；JSON 解析失败时先备份为 `settings.json.bad`。

**凭据以明文 JSON 存放在本机。** 不要把该目录提交到版本库，也不要分享 `settings.json`。审计日志只记录备注或打码后的 token，不含完整凭据。

主题与 token 显示单位保存在 WebView 的 `localStorage` 中，不进入 `settings.json`。

## 价格表

- 内置默认表：`src-tauri/resources/pricing.default.json`。
- 单价单位为美元 / 每百万 token，字段为 `input`、`output`、`cacheRead`、`cacheWrite`。
- 用户改价只把与默认不同的条目写入 `settings.json` 的 `pricing` 字段；未改动的模型仍随内置表更新。重置覆盖不会删除整个设置文件。
- 表中没有的模型标记为「未定价」。
- 默认价格为折算参考，使用前请对照 Anthropic、OpenAI、Google、xAI 等厂商的现行价目。部分厂商对超长上下文的加价规则未写入默认表。

## 数据来源

| 来源 | 鉴权 | 用途 |
| --- | --- | --- |
| Cursor | `WorkosCursorSessionToken`（支持 `user_xxx::<jwt>`，`%3A%3A` 会归一为 `::`） | 用量摘要、计费周期、按事件聚合、Grok 周额度、账户邮箱、本机会话切换 |
| ChatGPT | `Authorization: Bearer` + `ChatGPT-Account-Id` | 额度窗口、订阅信息；续期走 OAuth `refresh_token` |
| 本机会话 | 无网络 | 读取 Codex CLI 的 `sessions/**/*.jsonl`，按轮次 token 与模型归属聚合 |

Cursor 套餐有效期取自当期计费周期起止；到期后由官方侧续期并重置额度。ChatGPT 套餐有效期来自订阅接口（JWT 通常不含该字段）。

## 仓库布局

```
my-ai-assistant/
├── package.json              # npm 脚本与 @tauri-apps/cli、chart.js
├── app-icon.png              # 图标源图
├── scripts/                  # 图标生成、JS 语法检查、Windows 发布打包
├── src/                      # 前端（Tauri frontendDist）
│   ├── index.html            # 主窗口
│   ├── tray.html             # 托盘面板
│   └── vendor/               # Chart.js UMD
└── src-tauri/                # Rust 后端
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── capabilities/
    ├── resources/pricing.default.json
    └── src/                  # 账户、用量、审计、托盘、代理、价格、本机客户端
```

## 声明

本程序调用 Cursor 与 OpenAI 的非公开接口，仅供在本机查询自己的账户与用量。接口变更、账号限制或服务条款冲突导致的任何后果由使用者自行承担。本仓库与 Cursor、OpenAI、Anthropic、Google、xAI 无附属关系。
