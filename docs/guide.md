**English** | [简体中文](guide.zh-CN.md) · [Project overview](../README.md)

# User and developer guide

## Installation

Download a Windows x64 installer, a Windows portable build, or the macOS Apple Silicon app from
[GitHub Releases](https://github.com/bgluminous/my-ai-assistant/releases). Packages are unsigned.
Windows requires [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/). On macOS,
drag the app out of the DMG into a writable Applications folder to enable in-app updates.

Installed copies can update from **About**, which also shows release notes. Automatic checks can be
turned off in **Settings**. For the portable Windows build, download the next version from Releases.

## Account Management

Open **Accounts** and select **+** to add a provider. Each row shows the plan, remaining quota, and
last refresh. Drag a row by its handle to reorder it in both the main window and the tray.

| Provider        | How to add an account                                         | Where usage comes from                                                |
| --------------- | ------------------------------------------------------------- | --------------------------------------------------------------------- |
| Cursor          | Local login or `WorkosCursorSessionToken`                     | Per-account usage events and charges                                  |
| ChatGPT / Codex | Local login, token, or `auth.json` from a file or pasted JSON | Codex session logs under `~/.codex/sessions` by default               |
| Claude          | Browser OAuth, local login, or token                          | Claude Code logs under `~/.claude/projects` by default                |
| OpenCode        | Import a V2 workspace or authorize with OAuth                 | Official requests for the whole workspace, with Owner or Admin access |

A ChatGPT `auth.json` file or pasted JSON must contain `tokens.access_token`.
Use the switch button on a row to change the local login. Cursor launches its client. For
GPT / Codex, Claude, and OpenCode, choose **CLI only**, **Desktop only**, or **Both**.
Existing CLI sessions keep running. Save open work before switching a desktop client. Clients that
share a credential store may pick up the new login the next time they read it. The success dialog
closes after three seconds.

On first use, bind Claude Desktop to the selected account by signing in to the official client when
prompted. Completing or cancelling the binding restores the previous Desktop login. If binding is
interrupted, save your work in Claude Desktop, then select **Restore original login** in the
account editor or switch dialog. Recovery closes Claude Desktop.

Refresh one account or all accounts on demand, or set a schedule globally or per account. Automatic
token renewal can be turned off individually for ChatGPT, Claude, and OpenCode. For OpenCode plan
coverage, authorization, and archive history, see the
[integration guide](opencode-integration.md).

## Usage Statistics

Select one or more sources in **Usage**. Choose a day, week, month, all time, or a custom range.
The charts break down tokens and costs by model, source, and token type.

- **Equivalent API cost** applies model prices per million tokens. The plan multiple divides that
  cost for the selected period by the full monthly fees of known plans. Models without a price
  still contribute to the token count.
- **Raw bills** show individual Cursor charges or OpenCode request costs. Filter by model and
  export matching rows as CSV. Use the official wallet records to check actual OpenCode Go charges.
  You can also save a PNG usage snapshot for one Cursor account. Turning on **Exclude API quota
  usage** for Cursor removes matching local events and filters future syncs.
- **Pricing** is available in the top-right corner. Apply an online update or edit a model's rate.
  Your rates take precedence over online and bundled prices. For default USD rates and the limits
  of long-context, time-based, and multimodal estimates, see the
  [bundled pricing table](../src-tauri/resources/pricing.default.json).

## System Tray

Click the tray icon to check quotas, switch accounts, or see today's usage in **Overview**.
Double-click to reopen the main window. Closing the main window leaves scheduled refresh running.
Choose **Quit** from the tray menu to exit.

## Configuration and Data

- **Local storage.** Data lives in `%USERPROFILE%\.xilore\myaiassistant\` on Windows and
  `~/.xilore/myaiassistant/` on macOS. `settings.json` holds accounts and settings. Cursor and
  Claude tokens are plain text. The app encrypts ChatGPT's full Auth and independent OpenCode
  credentials with an embedded key. Protect this directory, account exports, and backups as you
  would your login credentials.
- **Backups.** Export a full backup from Settings with an optional password. Restoring merges
  accounts by identity and applies the backed-up settings. Deleting a Cursor account keeps its
  statistics by default. Full backups include those records and OpenCode archives. Refresh active
  Cursor accounts to rebuild their archives. Deleting an OpenCode account removes its local archive.
  Claude Desktop bindings stay on the machine where they were created.
- **GPT usage sync.** Usage stays local by default. To share it across devices, open **Settings**
  and select **GPT usage sync**. Enter your HTTPS server and sync key, test the connection, then
  select **Done**. The server can read model names, timestamps, token counts, and session
  identifiers. Use **Copy prompt** on the same page for instructions to host a compatible server.

  - If the sync account is disabled, automatic sync pauses. After the administrator re-enables the
    account, select **Refresh** on the **Usage** page to resume syncing.
  - If the administrator resets the sync key, enter the new key on every device using that sync
    account. Test the connection and select **Done** on each device. The server keeps the account's
    usage history.
- **Network and audit.** Account queries go to each provider. App updates and default price
  updates come from GitHub. The proxy test uses `inf.xil.to`. Choose the system proxy, a direct
  connection, or a custom HTTP or SOCKS proxy. The audit log records refresh failures, account
  switches, and data exports.

## Build and Release

The frontend lives in `src/` and the Tauri 2 Rust backend in `src-tauri/`.
To run from source, install Node.js 20.19+ and Rust. On Windows, install Visual Studio Build Tools
with **Desktop development with C++** and the Windows SDK. On macOS, run `xcode-select --install`.

```bash
npm install
npm run tauri dev
```

`npm run tauri build` creates an installer for the current platform. On Windows,
`npm run release:windows` collects installers and the portable build under `release/`. It clears
`src-tauri/target` after a successful build by default. For cache and archive options, see the
[Windows packaging script](../scripts/build-release-windows.ps1).

Pushing a `Release-X.Y.Z` branch publishes a release. A manual Actions run builds the packages.
Updater signing requires the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
secrets. Publishing also requires `RELEASE_REGISTRY_TOKEN`. Setup and retry instructions accompany
the [release workflow](../.github/workflows/release.yml). Changes by version are in the
[release notes](releases).

## License and Disclaimer

Licensed under GNU GPLv3. When referencing or building on this work, credit BGLuminous and the
[original project](https://github.com/bgluminous/my-ai-assistant). For redistribution terms,
see the [license](../LICENSE).

This project is independent of the providers. It uses some non-public APIs whose fields and
availability can change. Use it with your own accounts in accordance with each provider's terms.
Users are responsible for the consequences of API changes, account restrictions, and conflicts
with service terms.
