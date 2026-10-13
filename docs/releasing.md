# 版本发布与 GitHub 索引

项目使用 GitHub Actions 构建 Windows x64 与 macOS Apple Silicon 安装包，
共用本项目的 `v<版本>` Release。正式公开后，再将各平台的主安装包登记到公共索引。
应用从公共索引发现版本，再通过签名的 Tauri 更新包安装；行为与验收见
[应用更新维护](updating.md)。

## 发布参数

| 参数           | 约定                                       |
| -------------- | ------------------------------------------ |
| 权威版本       | `src-tauri/tauri.conf.json` 的 `version`   |
| 发布分支       | `Release-X.Y.Z`，保留大写 `R`              |
| 项目标识       | `my-ai-assistant`                          |
| 制品仓库       | `bgluminous/my-ai-assistant`               |
| Release 标签   | `vX.Y.Z`，两个平台共用                     |
| 索引仓库与分支 | `bgluminous/open-source-registry` / `main` |
| 登记模式       | `existing`，核实正式 Release 和附件后登记  |
| 强制更新       | 默认 `false`                               |

| 平台标识      | 主安装包                               | 其它 Release 附件 |
| ------------- | -------------------------------------- | ----------------- |
| `windows-x64` | `my-ai-assistant_<版本>_x64-setup.exe` | MSI、便携版 exe、两个安装包的 `.sig`、`updater-windows-x64.json` |
| `macos-arm64` | `my-ai-assistant_<版本>_aarch64.dmg`   | `.app.tar.gz`、对应 `.sig`、`updater-macos-arm64.json` |

非敏感配置集中在根目录 `.version-settings`，本地检查与登记入口共用。
版本从权威源读取，标签和附件名由 `ci/release-platform.sh` 派生，
不在配置里另存版本号、下载地址或令牌。配置按数据解析，不执行其中的命令或变量。

`scripts/version-publish/` 来自规则仓库公共发布模板 `28cb7a7`。
项目适配只有严格的大写分支解析和本地检查中的平台附件映射；
公共 JSON、HTTP 与 GitHub 索引协议实现保持原样。CI 不依赖本机的规则仓库克隆。

## 首次启用前

1. 管理员确认索引仓库公开可读，且 `main` 已有初始提交。
   空仓库或不存在的分支必须先初始化；发布器不会创建仓库或绕过分支保护。
   先将原线上价格表迁入 `resources/my-ai-assistant/pricing.json`，保持内容不变，
   验证 Raw 地址匿名可读后再发布切换来源的客户端。正式发布的 prepare 阶段会检查此条件。
2. 在本项目 GitHub Actions Secrets 配置 `RELEASE_REGISTRY_TOKEN`。
   令牌仅需索引仓库的 Contents 写权限，分支规则须允许该身份推进 `main`。
   项目的 `GITHUB_TOKEN` 不能代替跨仓索引令牌。
3. 项目 Release 默认使用工作流自己的令牌。
   需要额外发布身份时，单独配置 `RELEASE_TOKEN`，仅授权本项目。
   如果发布提交相对默认分支修改了工作流，该令牌还需要 Workflows 写权限；
   classic PAT 对应 `workflow` scope。详见
   [GitHub 创建 Release 的权限说明](https://docs.github.com/en/rest/releases/releases#create-a-release)。
4. 在本项目配置 `TAURI_SIGNING_PRIVATE_KEY` 和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。
   使用项目专用的已备份私钥；公钥固定在 `src-tauri/tauri.conf.json`。
   不要为重跑构建生成新密钥。两个平台及手动试跑都需要相同的签名 Secrets。
   通过 API 写入 Secrets 的管理员令牌须有本项目的 Secrets 写权限，
   该权限与发布用的 Contents / Workflows 权限分别授予。

缺少索引令牌时，正式发布在创建草稿和编译之前失败。
离线检查不会验证远程仓库可见性、分支保护或令牌权限；通过后仍需满足上述前置条件。

GitHub 保留 `GITHUB_` 前缀，不能用作自定义 Secret 名称。
工作流将 `RELEASE_REGISTRY_TOKEN` 映射为发布脚本的 `GITHUB_REGISTRY_TOKEN` 环境变量；
可选的 `RELEASE_TOKEN` 提供项目 Release 操作身份，未配置时使用工作流令牌。

## 本地检查

Windows 使用 Git Bash；macOS / Linux 使用 POSIX sh。检查只需要 shell、awk
和基础命令，不运行 Tauri、Cargo 或上传命令。以下参数必须是实际计划发布的分支：

```sh
sh scripts/version-publish/check-version.sh --branch Release-1.3.0
```

检查涵盖分支格式、版本整数编码范围、项目配置、两个平台、标签与附件映射。
版本读取入口还检查 `package.json`、`package-lock.json` 的两处版本、
`Cargo.toml` 的 `[package] version` 与权威源完全一致。
`Cargo.lock` 由 Cargo 生成，本地检查不会为同步锁文件启动编译或手工修改它。

缺失配置必须获得当次明确的跳过许可；已有错误不能跳过。
检查读取当前工作区，提交与推送前仍需核对实际目标快照，不能用未提交的版本改动代替。

接入脚本的离线回归测试：

```sh
python scripts/test-version-release.py
```

测试使用临时项目和本地命令桩，禁止真实网络请求；不编译桌面应用，结束后自动清理。
Git for Windows 可将 `TEST_SHELL` 指向安装目录的 `usr/bin/bash.exe` 或 `dash.exe`。

## GitHub Actions 流程

工作流为 `.github/workflows/release.yml`。推送 `Release-*` 分支触发正式发布；
`master` 和其它分支不触发。手动运行只构建，将安装包及各平台完整签名资源保存为
保留 7 天的 Actions artifacts，不创建 Release、不登记索引，也不要求索引令牌。

1. `prepare` 读取联动版本，检查实际分支及配置，确认签名密钥与索引令牌存在，
   并匿名读取公共价格表。
   自动生成下载指引与变更说明，离线生成各平台登记输入，保存为
   `release-registration` artifact，保留 1 天；然后创建或复用草稿 Release。
2. `build` 在 Windows 与 macOS runner 上分别构建并签名，校验公钥、更新包及签名，
   生成对应平台的固定版本清单，再上传全部附件。
   两个平台互不取消，使用 Rust 编译缓存；同名附件只复用相同大小和 SHA-256 的内容，
   不删除、不覆盖已有附件。已公开 Release 拒绝再次上传。
3. `publish` 等两个构建成功后公开共享 Release；任一失败时草稿保留。
   GitHub 在目标提交创建 `v<版本>` 标签。
4. `register` 按平台分别读取之前保存的登记输入，匿名验证正式 Release 的更新清单、
   更新包与签名文件，检查实际下载内容的 SHA-256 和签名，再登记主附件。
   两个平台可分别重跑，彼此不阻断；本阶段仅需本项目 Contents 读权限。

两个平台共用 Release，因此登记必须等共享 Release 正式公开。
公共发布器以 `existing` 模式登记，不再次构建、上传或自动公开草稿。
每个平台的版本对象、最新版本与历史索引在索引仓库同一次提交中更新，
并发冲突重新校验后重试，不强推分支，也不允许旧版本回退最新指针。

变更说明取当前提交可达的最近一个 `v*` 标签之后的非合并提交标题，
按 Conventional Commits 分组：新增、修复、性能与重构、其它及未分类，
附 GitHub 对比链接；没有前一个标签时写首个发布版本。
下载指引同时说明安装包未签名，以及 macOS 首次打开的处理方式。
登记输入保存这份原始说明，登记重跑时不重新生成；之后人工编辑 Release 说明
不会自动改写已登记元数据。

## 失败与重跑

- 只有登记失败：修正权限或索引仓库配置后，在原工作流选择 **Re-run failed jobs**。
  使用原输入和原安装包，只补登记；不要重跑整个构建发布流程。
- 两个平台同时登记时出现并发写入：发布器最多重试三轮，并保留其它平台的内容。
- 构建或上传失败：检查草稿与已有附件后重跑失败 job。同名文件摘要不同会停止；
  不通过删除附件、打开覆盖或重建旧版来绕过冲突，应复用原制品或发布新版本。
- 同版本已正式发布：整个发布流程拒绝再次创建或改写该版本。
- 登记输入 artifact 已过期：停止自动重跑，先恢复原始输入并核对正式 Release；
  不根据可能已修改的说明自动拼出新内容。

登记失败不删除已发布安装包、不回改版本号，也不会自动打开元数据覆盖开关。
更新包具有 Tauri 更新签名；没有 Windows Authenticode 或 Apple 开发者代码签名。
平台范围不含 Intel Mac 和 Linux。

## Windows 本地打包

需要 Node.js 18+、npm 依赖、Rust MSVC 工具链及 Visual Studio Build Tools。
缺少 MSVC 时可选 Rust GNU 工具链，但须另备 MinGW-w64 的 `dlltool.exe`、
`gcc.exe` 与 `windres.exe`；仅安装 rustup 的 GNU 工具链不够。
链接时可能出现 `.rsrc merge failure: multiple non-default manifests` 警告。

```powershell
npm run release:windows
# 复用现有产物，仅重新收集和打包
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release-windows.ps1 -SkipBuild
# CI 保留 Rust 缓存，只收集独立安装包，不生成 7z
npm run release:windows -- -KeepTarget -SkipArchive
# 已设置签名环境变量时，生成并收集更新签名
npm run release:windows -- -KeepTarget -SkipArchive -Updater
```

脚本读取并核对 npm、Tauri 与 Cargo 版本，再执行 Tauri release 构建。
产物收集到 `release/my-ai-assistant-v<版本>-windows-x64/`：便携版 exe、
NSIS 安装包和 MSI。便携版资源已内嵌，但运行仍需 WebView2。
默认使用仓库内置 `tools/7zip/7za.exe`，缺失时回退到 PATH 或系统安装目录的 7-Zip，
以 `-t7z -mx=9` 打包；7z 内保留暂存目录这一层。

默认在成功打包后删除 `src-tauri/target`，`-SkipBuild` 也会清理；
`-KeepTarget` 保留该目录，`-SkipArchive` 跳过 7z。不会清理 npm 依赖或 `src-tauri/gen`。
最终保留暂存目录和未被跳过的 7z，全部位于 Git 忽略的 `release/` 下。
脚本兼容 Windows PowerShell 5.1，读取中文 JSON 使用 UTF-8。

打包后核对 MSI / NSIS 两个 bundle、7z 完整性、便携版 ProductVersion /
FileVersion，以及 Cargo 自动更新后的锁文件版本。未传 `-KeepTarget` 时确认 target 已清理。
构建日志应显示 `Finished 2 bundles`，可用以下命令检查归档和版本元数据：

```powershell
tools/7zip/7za.exe t release/my-ai-assistant-v<版本>-windows-x64.7z
(Get-Item release/my-ai-assistant-v<版本>-windows-x64/my-ai-assistant.exe).VersionInfo
```

归档检查应显示 `Everything is Ok`，ProductVersion / FileVersion 应等于目标版本。
macOS 本地构建使用 `npm run tauri build`；发布用 DMG 由 Actions 构建。
图标命令为 `npm run icons`，前端语法检查为 `npm run check:js`。

## 索引读取与发布收尾

每个平台的索引路径如下，`<平台>` 取上表中的稳定标识：

```text
projects/my-ai-assistant/<平台>/latest.json
projects/my-ai-assistant/<平台>/index.json
projects/my-ai-assistant/<平台>/versions/<版本>.json
```

例如 Windows 最新版本的公开入口为
[Windows 版本索引](https://raw.githubusercontent.com/bgluminous/open-source-registry/main/projects/my-ai-assistant/windows-x64/latest.json)。
首次成功登记后该地址才有对应数据。
客户端应核对项目、平台及整数编码，比较 `versionCode`，再使用记录中的主安装包地址。
Raw/CDN 可能缓存旧结果，不应据此降级。桌面应用已接入该入口，
通过 `extra.updater.manifestUrl` 指向本版本的更新清单，校验平台及安装类型后下载。

正式发布须另获授权。发布完成后核对全部安装包、更新资源与两个平台的索引，
再按项目规则同步 GitHub 创建的版本标签到另一远程；发布分支保留且不再修改。
规则仓库独立维护，不作为公开项目或 CI 的运行依赖。

先在 `master` 完成版本联动、检查、提交并同步两个远程。
获得具体发布与推送授权后，才执行以下对应操作；示例中的版本须替换为目标版本：

```sh
git push origin master:refs/heads/Release-<版本>
git push github master:refs/heads/Release-<版本>
# Actions 正式发布成功并核对后，另按已确认的标签推送范围执行
git fetch github --tags
git push origin v<版本>
```
