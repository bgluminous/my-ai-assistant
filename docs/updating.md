# 应用更新与价格表维护

发布作业、附件及登记步骤见[版本发布说明](releasing.md)。
首次安装含更新模块的版本仍需手动下载安装包，之后才支持应用内升级。

## 检查与安装

启动 30 秒后检查一次，此后每 24 小时检查一次；关闭自动检查不影响「关于」页手动检查。
计时由 Rust 后端负责，关闭主窗口、切换页面或仅保留托盘不会重复创建计时器。
应用退出后停止计时，下次启动重新计时。更新开关随设置备份导出和导入；
没有此字段的旧备份按默认开启处理。

后台发现更新或失败时保留主窗口与托盘内的可见状态，不弹窗、不打开浏览器，
静默启动也不会因此唤醒窗口。用户打开「关于」后可查看版本与更新说明，
确认后才下载安装。下载显示进度并允许取消；未知文件大小时显示已下载字节数。

| 运行方式 | 更新方式 |
| -------- | -------- |
| Windows x64 NSIS 安装版 | 下载同类型 NSIS 安装包，验证签名后交给安装器 |
| Windows x64 MSI 安装版 | 下载同类型 MSI 安装包，验证签名后交给安装器 |
| Windows 便携版或开发构建 | 提供对应版本的 GitHub Release 页面，手动替换 |
| macOS Apple Silicon 已安装 `.app` | 安装位置可写时使用签名的 `.app.tar.gz`，完成后重启 |
| macOS DMG、隔离运行或不可写位置 | 引导移到 Applications 或从 Release 手动安装 |

安装前关闭新的业务入口，等待已开始的数据写入完成，最多等待 30 秒。
下载取消、等待取消、超时或安装返回错误时解除暂停；不会清理账户、用量或设置。
进入安装阶段后不能取消。Windows 由 Tauri 在成功启动安装器后退出应用，
安装器负责后续安装和重启；操作系统拒绝或安装器失败时，按其提示手动恢复。
macOS 若恢复原安装位置失败，界面会指出旧应用保留的位置，供手动恢复。

普通更新可稍后处理。确认索引中较新版本的 `forceUpdate=true` 后，
限制旧版业务操作，但仍保留更新、代理配置与备份导出入口；安装依然需要用户确认。
该要求缓存在本机，重启、关闭自动检查或导入旧备份不会清除它。
旧 CDN 索引不能使已知的强制更新倒退；升级到满足要求的版本后恢复使用。

## 协议与失败处理

客户端按平台匿名读取公共仓库 `main` 分支中的：

```text
projects/my-ai-assistant/<平台>/latest.json
```

平台为 `windows-x64` 或 `macos-arm64`。先核对 `projectCode`、`platformCode`、
规范版本与 `versionCode`，再比较整数编码，禁止降级。
编码为 `10000000 + X×131072 + Y×1024 + Z×8 + 1`，各版本段范围为 0～99。

索引的 `extra.updater.manifestUrl` 必须指向本项目对应版本、对应平台的清单：

```text
https://github.com/bgluminous/my-ai-assistant/releases/download/v<版本>/updater-<平台>.json
```

清单的版本、平台目标、下载地址必须一致。Windows 目标分别为
`windows-x86_64-nsis`、`windows-x86_64-msi`，macOS 为 `darwin-aarch64`。
后端复核用户确认的版本，并使用官方 Tauri updater 校验更新包签名、安装。
旧索引没有更新清单时，只提供手动下载。前端不能指定任意更新地址或直接调用安装插件。

检查、下载和安装共用单一操作锁，重复请求复用正在进行的状态，不启动第二次操作。
事件带递增序号，前端忽略迟到的旧事件；更新说明按纯文本展示。
网络使用现有的系统代理、直连或自定义代理设置，重试时读取最新配置。
索引和清单检查超时为 20 秒，下载超时为 15 分钟；失败不会显示为已是最新版本。

失败界面提供原因、重试、代理设置及 GitHub Releases 入口。
已确认目标版本时打开其标签页，否则打开本项目 Releases 列表；浏览器打开失败可复制地址。
签名失败不安装，取消下载不改动应用文件，错误信息不包含代理密码或远端完整响应。

## 签名密钥

应用公钥保存在 `src-tauri/tauri.conf.json`，签名构建额外合并
`src-tauri/tauri.updater.conf.json`。普通本地开发不要求私钥。
这套签名用于 Tauri 更新包验证，不代替 Windows Authenticode 或 Apple 代码签名。

项目私钥放在仓库外的 `%USERPROFILE%\.tauri\my-ai-assistant\`：
`updater.key`、`updater.key.pub` 及各自的 `.backup` 副本。
`password.dpapi` 保存当前 Windows 用户保护的密码，目录仅当前用户可访问。
DPAPI 文件不能直接跨用户或跨机器恢复；迁移前须在安全位置另外备份解密密码和私钥。
不要将这些文件放入 Git、构建 artifacts、日志或聊天。

Actions 使用 `TAURI_SIGNING_PRIVATE_KEY` 与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。
私钥值为密钥文件内容，不能填写本机路径。丢失密码或私钥时不要直接更换应用公钥，
否则既有客户端无法验证后续更新；应先恢复备份，无法恢复时另行设计手动迁移版本。

本机需要签名构建时，在 PowerShell 7 中仅为当前进程设置环境变量：

```powershell
$keyDir = Join-Path $env:USERPROFILE '.tauri/my-ai-assistant'
$secure = ConvertTo-SecureString ((Get-Content (Join-Path $keyDir 'password.dpapi') -Raw).Trim())
try {
    $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content (Join-Path $keyDir 'updater.key') -Raw
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [System.Net.NetworkCredential]::new('', $secure).Password
    npm run release:windows -- -KeepTarget -SkipArchive -Updater
} finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
    $secure.Dispose()
}
```

## 价格表维护

价格表与平台版本索引分开保存，可以独立更新，无需发布应用版本：

- [公共仓库文件](https://github.com/bgluminous/open-source-registry/blob/main/resources/my-ai-assistant/pricing.json)
- [客户端固定 Raw 地址](https://raw.githubusercontent.com/bgluminous/open-source-registry/main/resources/my-ai-assistant/pricing.json)

首次迁移须保留原线上文件的全部内容，包括 `note`、`models`、模型名称和价格字段。
核对下载内容、模型数量及各价格值后，按公共仓库流程建立 `main` 并提交文件，
验证匿名 Raw 读取成功，再发布切换默认来源的客户端。CI 会检查公开来源可用性。

后续在公共仓库直接维护该 JSON。`input`、`output`、`cacheRead`、`cacheWrite`
继续按每百万 tokens 的美元价格表示；值必须有限且非负，不修改字段名称或单位。
编辑后验证 JSON、核对预期差异，再按该仓库规则提交、推送。
发布后执行 `node ci/check-pricing-source.mjs` 验证匿名读取，并在应用中检查、手动应用。
不要把价格表作为安装包附件，也不要修改已发布版本的更新清单。

内置 `src-tauri/resources/pricing.default.json` 继续作为离线快照，应用发版时按需要同步。
客户端覆盖顺序保持「内置表 → 已应用的在线缓存 → 用户自定义」。
检测不会自动应用价格；拉取失败不会清空缓存与自定义价格。

空地址及旧官方默认地址 `https://inf.xil.to/kv/MAA-Price-Table` 自动采用新默认来源；
自定义地址继续保留。载入本机设置和导入旧备份共用同一兼容处理。
失败时可查看原因、重试、打开代理设置或公共仓库文件；不自动跳转浏览器。

## 验证

前端和发布协议的离线检查不需要真实发布凭据，也不会上传或登记版本：

```sh
npm run check:js
node --experimental-vm-modules scripts/test-updater.mjs
node scripts/test-updater-release.mjs
python scripts/test-version-release.py
```

签名测试使用临时测试密钥，覆盖 Windows 两种安装包、macOS 更新包、
内容篡改、缺失签名、版本不一致和禁止覆盖不同清单，结束后清理临时目录。
Rust 定向测试需要本机工具链，使用本地 HTTP 服务与虚拟时间，不访问线上更新源：

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib app_update::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib operations::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib update_protocol::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib pricing::tests
```

正式启用前还须在隔离的 Windows 和 Apple Silicon macOS 环境做连续两版升级验收。
使用独立测试账户、测试密钥、测试仓库与递增版本；测试构建的协议地址及发布配置
须全部指向测试仓库，不改写正式索引或已发布附件。只导入合成数据，不使用真实账户凭据。

| 场景 | 验收内容 |
| ---- | -------- |
| NSIS → NSIS、MSI → MSI、已安装 `.app` → `.app` | 两个连续测试版本都能发现、确认、下载、安装、重启，安装类型不变 |
| 数据保留 | 升级前后核对账户、用量记录、代理、更新开关、价格缓存与自定义价格 |
| 便携版、DMG、不可写目录 | 提示手动安装，链接指向确定版本，不尝试自动覆盖 |
| 失败与取消 | 注入超时、代理失败、404、无效 JSON、版本冲突、签名错误；取消下载后业务可继续 |
| 安装等待 | 有写入时等待，无新业务进入；等待超时或取消后恢复操作 |
| 后台行为 | 静默启动保持隐藏，失败保留状态，不自动打开浏览器；重复点击不重复下载 |
| 强制更新 | 安装仍需确认；失败、重启、关闭自动检查及旧备份均不能解除已知要求 |

记录操作系统、安装类型、源与目标版本、实际结果及失败日志。
离线测试通过不能替代真实安装、跨版本数据保留或平台权限验收。
