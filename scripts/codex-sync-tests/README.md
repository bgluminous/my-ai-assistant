# GPT 同步专项验证

此测试入口直接包含生产环境的采集、SQLite 存档、同步状态机、HTTP 传输和计价源码。
只替换桌面 IPC 宏、事件发送、审计输出和用户设置持久化，不加载 Windows GUI 依赖。
测试使用内存库或随机临时目录，HTTP 替身只监听本机随机端口并随测试关闭。
不会扫描或同步真实用户数据，不提供可部署的中心服务。

在仓库根目录运行：

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
cargo test --manifest-path scripts/codex-sync-tests/Cargo.toml --target-dir src-tauri/target -j 1
npm run check:js
node --experimental-vm-modules scripts/test-codex-cache.mjs
node --experimental-vm-modules scripts/test-codex-sync-settings.mjs
```

生产库的相同单元测试也可通过
`cargo test --manifest-path src-tauri/Cargo.toml --lib codex` 运行。
独立入口不验证 Tauri 命令注册、窗口渲染或真实设置／备份文件读写；
这些集成需在可运行桌面应用的环境中检查。

## 实际服务端联调

`server-integration.mjs` 使用已安装依赖的 xil-information-service 源码，
启动真实同步路由、管理接口和 MySQL mapper。客户端复用本仓库的采集、SQLite、
HTTP 与同步状态机。其它服务端业务依赖沿用服务端测试替身，不启动生产入口。

需要 Node.js 24+、Rust、PowerShell 7，以及已获授权的开发 MySQL 8。
连接必须显式指定 `/dev`；只操作本轮随机前缀表，不读取 `application.json`。
密钥和数据库连接不能放进命令参数、示例实值或测试输出。

```powershell
$serverRepo = 'D:/path/to/xil-information-service'
$env:TEST_MYSQL_URL = 'mysql://<user>:<encoded-password>@<host>:<port>/dev'
try {
  node scripts/codex-sync-tests/server-integration.mjs $serverRepo
} finally {
  Remove-Item Env:TEST_MYSQL_URL
}
```

启动器只编译隔离 Rust 测试入口，不构建桌面应用或整个服务端。
它生成临时 CA 与服务器证书，客户端仅在本测试内信任该 CA；
通过带 `/maa` 前缀的本机 HTTPS 反向代理访问服务端，不关闭证书验证或安装系统证书。
此用例在普通 Rust 测试中默认忽略，必须由启动器提供临时服务与隔离目录后显式运行。

场景包含空客户端、跨机器分叉与祖先晚到、复制日志、超过 500 条的上传与分页、
上传成功后响应丢失、分页中断、SQLite 重开与服务端 HTTP 实例重建后的续传，
以及翻页时新增记录、
设备改名、缓存恢复、原日志删除、账户启停与换钥、不同账户隔离、epoch 与 409 恢复，
以及真实 MySQL 写入失败和客户端 SQLite 写入失败的回滚。

启动器结束时关闭进程和监听端口，删除测试表并核对无残留，
移除临时 SQLite、模拟日志及证书。联调不覆盖桌面窗口、托盘渲染或正式域名部署。

## 主要验证场景

| 场景       | 验证点                                                         |
| ---------- | -------------------------------------------------------------- |
| 两台客户端 | 首次全量、新增明细增量、无本机日志下载、空中心                 |
| 复制与分叉 | 来源集合、全设备去重、跨机器父子关系、祖先晚到、深层谱系       |
| 断点与失败 | 响应丢失重传、分页中断续传、SQLite 重启、写入失败回滚          |
| 协议       | Bearer 认证、路径和 JSON、游标转义、版本、超限及错误状态       |
| 配置       | 空地址、HTTPS 约束、换配置取消旧写入、空间隔离、密钥保留或清空 |
| 账户       | 停用与启用、同账户换钥保留历史、不同账户隔离、下载记录不转传 |
| 恢复       | 409 重新握手一次、epoch 回退恢复、重置事务失败不丢存档与游标 |
| 页面       | 设备与记录合计最多 500 条、测试连接不登记设备、暂停后手动恢复 |
| 数据保留   | 原日志删除、按设备清理后恢复、保留未上传记录、禁止转传下载数据 |
| 并发与缓存 | 触发合并、旧请求作废、设备筛选不污染汇总、清理后只读存档       |
| 计价       | 原始模型名、当前价格表、查看机器本地日期和小时序列             |
