# OpenCode 接入与验证

本应用统计官方工作区的模型总用量，覆盖所有成员、设备与服务账户，
不收集第三方 Provider 调用。
按[官方角色说明](https://opencode.ai/docs/console/workspaces/)，
Member 只可查看本人数据，因此工作区归档要求 Owner 或 Admin 授权；
成员账户仍可查看自己的套餐额度，但不会把个人记录标成工作区总量。
身份由官方服务地址和工作区 ID 确定，重复授权更新同一账户。
核对日期：2026-11-10。Console 管理接口并非稳定的公共计费 API，
解析失败会保留已知缓存并明确提示，不把未知字段作为零用量。

## 套餐与独立授权

[官方 Go 说明](https://opencode.ai/docs/console/go/)列出的月费为：
Go $10，Go Plus $40，Go Teams 每成员 $20。Teams 使用接口返回的成员数计算，
未提供人数时月费保持未知。Console 按量余额单列，不与套餐额度混合。

应用通过官方设备码流程创建独立会话，使用 `client_id=opencode-cli`：

| 操作 | 请求 | 主要字段 |
| --- | --- | --- |
| 开始 | `POST /console/auth/device/code` | supports_org_scope=true；设备码、授权地址、轮询间隔、期限 |
| 轮询 | `POST /console/auth/device/token` | 设备码 grant；处理 pending、slow_down、拒绝和超时 |
| 续期 | 同上 | refresh_token grant；access_token、refresh_token、expires_in、org_id |
| 身份 | `GET /console/api/orgs/current` | org.id、org.name、userId、hasGoSubscription |
| 用户 | `GET /console/api/user` | email，仅作为账户说明 |

设备码与令牌不发给前端；前端只收到授权地址、用户码、尝试 ID 和脱敏账户视图。
设备码接口的授权地址可能是 `/console/device?...` 相对路径，
应用先补全官方域名，再校验 HTTPS 来源并打开浏览器。
每个账户的续期串行化，多账户互不阻塞。取消与完成竞争时，取消后的响应不能保存账户。
身份查询重试复用已换取的凭据，不重复兑换一次性设备码。

授权保存为 `opencodeAuth` 加密副本，账户 `token` 字段仅保存工作区 ID。
加密沿用项目本机存储机制，不构成对可执行程序逆向的防护；分享备份前应设置备份密码。
请求固定发送到官方 HTTPS 主机，沿用应用代理配置，不跟随 OAuth 重定向。

## 套餐与账户接口

| 请求 | 读取口径 |
| --- | --- |
| `GET /console/api/go/status` | product、memberCount、access.startsAt / endsAt、续费状态 |
| `GET /zen/go/v1/usage` | usage.rolling / weekly / monthly：percent、resetsAt、status |
| `GET /console/api/billing/status` | balanceMicroCents、availableMicroCents、billingMode |

Console 请求使用 Bearer 和 `x-org-id`；额度请求使用 `x-opencode-org-id`。
额度窗口属于当前授权成员，不把多个成员的百分比相加。余额按整个工作区的口径读取。
MicroCents 是美元的亿分之一，十进制字符串在 Rust 端解析后换算；
零余额与负余额都有效，缺失或格式错误不按零处理。

三个读取项分别保留状态、错误与上次成功时间：无订阅、无权限、授权失效、
网络或解析失败分别处理。显式无订阅清除旧套餐周期与旧额度，单纯失败保留旧值。

## 请求明细与本地归档

主来源为 `GET /console/api/request-logs`，参数为 category=inference、
since / until（毫秒）、limit=100 和 cursor。响应包含 items、nextCursor、until、
retentionDays；2026-11-10 验证返回的保留期为 30 天。

只接纳 provider=opencode 且 product 为 standard、go、go-plus 或 go-teams 的推理记录。
归档仅保存记录及请求 ID、请求时间、模型、产品、Token 分项与子项、
官方记录成本，不保存提示词、请求头、响应头、位置、成员或服务账户详情。
未提供模型名的失败记录使用 unknown，避免一条失败请求阻断整页同步。

初次同步补齐近 30 天；后续从已完成水位向前重叠 48 小时，
同时受服务端保留期限制。SQLite 以工作区和记录 ID 幂等更新，
每页落盘，只有完整分页成功后合并覆盖区间并推进水位。
409 游标过期允许重启一次；重复游标、页间 until 改变及中途失败都不推进水位。
手动重同步只重拉近 30 天，不清除旧归档。

输入不包含缓存读取和写入；输出包含 reasoning，不能再累加 reasoningTokens。
请求日志的 cacheWriteTokens 作为缓存写入总数，cacheWrite1hTokens 不重复累加。
汇总接口的 totalCacheWrite5mTokens 与 totalCacheWrite1hTokens 是互斥分项，需相加。
等价费用按现有价格表计算，未收录模型保留 Token 并显示未定价。

图表使用请求 startedAt 按本机时区分小时、日、周、月，范围端点为毫秒。
首次补齐之前、长期离线跨过服务端保留期产生的缺口保持可见；
全量备份携带全部归档和覆盖区间，恢复只补本机缺失记录，已有记录优先。
OpenCode 归档以整批事务恢复，写入失败不改变原有归档。若账户和设置已应用，
界面会提示部分恢复完成、已恢复数量和失败原因，可重试导入同一备份。
CLI 与数据库路径也随全量备份恢复；旧备份缺少路径时回到自动检测。
原生授权正在启动或尚未结束时，手动修改路径和备份导入都会被拒绝；
请先完成或成功取消授权再重试。备份被拒绝时，不写入任何账户、设置或归档。

## 成本与官方汇总核对

[官方费用说明](https://opencode.ai/docs/console/api/inference)区分
billing_source、list_cost、allowance 和 cost；推理响应的 cost 语义不能直接套给管理日志。
已验证 Go 请求日志仍有正数 cost，归档继续保留该官方记录成本，
不会把它或余额差值推算为钱包实扣。用量统计仅展示按官方 API 价折算的等价费用，
单账户与合并视图均不展示记录成本或实扣统计。

手动重同步后，取最近完整 UTC 日读取以下工作区级接口：

| 请求 | 用途 |
| --- | --- |
| `/console/api/usage/summary?from=YYYY-MM-DD&to=YYYY-MM-DD` | 工作区汇总，Token 分项为十进制字符串 |
| `/console/api/usage/models` | 按 page / pageSize 分页，使用 pageInfo.pageCount；只比较官方模型 |
| `/console/api/usage/cost-by-day` | 同一日期范围加 bucket=hour，核对汇总与小时 Token |

模型汇总中的官方 Provider 为 opencode 或 opencode-go；核对不传 userId 或
serviceAccountId，不把成员切片拼成工作区总量。汇总可能存在延迟，
有差异时标注核对日期与结果，不覆盖明细、填补缺口或据此推算扣费。

2026-11-10 的授权只读核对中，同一完整 UTC 日的明细与汇总 Token 总量一致，
明细包含的失败请求使请求条数不同。Go 日志仍出现正数记录成本，
因此请求计数和费用都不能直接套用汇总或钱包扣费口径。

## V2 本机适配

依据[官方凭据说明](https://opencode.ai/docs/cli/providers/)和
[官方 API 参考](https://opencode.ai/docs/api)，写入只使用原生凭据 API：

| 功能 | 方式 |
| --- | --- |
| 版本与路径 | `opencode --version`、`opencode debug paths db` |
| 本机身份导入 | 只读 V2 credential 表的工作区身份，不导入原生 OAuth 会话 |
| 查询与激活 | `opencode api get /api/credential`、`post /api/credential/{id}/activate` |
| 原生授权 | `post /api/integration/opencode/connect/oauth`，methodID=device |
| 轮询与取消 | 同一路径加 attemptID，分别使用 get / delete |

添加弹窗先选择本机导入或新增 OAuth 授权，两种方式都可填写备注。
本机导入先读取工作区列表，确认后仅导入所选项；每个工作区需要本应用自己的独立授权。
手填备注随账户保存，刷新套餐和用量时不会覆盖；留空时优先使用邮箱，
邮箱尚未获取时回退到工作区名称。清空已有备注可恢复自动命名。
账户读取异常统一通过状态列的感叹号悬停查看，包含失败字段与缓存时间，
不再额外占用用量摘要或余额下方的行。
恢复备份后根据工作区重新寻找本机登录；没有对应凭据时使用客户端原生授权。
原生授权完成后核对工作区，选择错误且原来有登录时恢复原登录。
本应用的续期仅修改自己的加密副本，从不将其令牌写入 OpenCode 数据库。

配置支持 CLI 绝对路径和已有数据库的绝对路径，留空使用自动检测。
CLI 调用使用参数数组、有界输出和超时，不执行拼接的 shell 命令。
原生接口返回的完整凭据只在后端内存中过滤，不进入前端、审计日志或归档。

## 定向验证

以下测试不构建完整 Tauri 应用，也不操作真实客户端登录：

```powershell
cargo test --manifest-path scripts/opencode-tests/Cargo.toml --offline
node --experimental-vm-modules --test scripts/test-opencode-cache.mjs scripts/test-subscription-pricing.mjs
```

Rust 测试复用生产解析、归档、OAuth 与客户端适配模块，只替换 IPC、设置与网络 / CLI 传输。
覆盖加密绑定、独立续期并发、取消与超时、三档套餐、纯按量、单项权限失败、
分页重复和中断、迟到记录、官方调用过滤、时区边界、覆盖缺口及备份往返。
客户端夹具使用隔离 SQLite，验证激活、原生授权成功、取消和工作区错误回滚。
JavaScript 测试覆盖月费、等价费用展示、按账户缓存、恢复失效和跨窗口刷新。

真实验证仅执行官方状态与用量 GET；浏览器交互授权和真实客户端写入切换
仍需在运行应用时验收。当前测试不能代替这两项端到端验证。
