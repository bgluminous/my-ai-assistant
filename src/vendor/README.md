# 前端离线依赖

Tauri 直接打包 `src/`，不从 CDN 加载脚本。

- `chart.umd.min.js`：Chart.js 的现有 UMD 分发文件。
- `marked.esm.js`：Marked 18.1.1 的 `lib/marked.esm.js`，原样保留上游版权头。
  完整许可见 `marked.LICENSE`，来源和完整性由根目录 npm 锁文件固定。

更新 Marked 时，同时更新 npm 锁文件、ESM 分发文件及许可；测试会核对两份文件一致。
日志渲染只使用 lexer，按允许的元素构建 DOM，不使用 Marked 生成的 HTML。
图片和原始 HTML 作为文字显示；链接仅允许不含凭据的绝对 HTTP/HTTPS 地址。

验证入口：

```sh
node --experimental-vm-modules scripts/test-updater.mjs
npm run check:js
```
