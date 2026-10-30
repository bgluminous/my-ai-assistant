// 随应用打包的离线搭建提示词；契约对应 src-tauri/src/codex_store.rs 与 codex_sync.rs。
// 修改协议时同步检查 src-tauri/src/tests/codex_*.rs 的哈希向量与传输回归。
// 只复制固定协议内容，不拼接本机设置、同步密钥或用量记录。
export const GPT_SYNC_SERVER_PROMPT = String.raw`# 为我搭建 MAA GPT 用量同步服务端

请为我的多台机器搭建兼容 MAA（my-ai-assistant）的 GPT 用量同步中心。
下面是完整的 v1 实施契约，不需要访问 MAA 源码或另行取得附件。
请先读取目标仓库的协作规则，沿用已有技术栈；没有现有项目时选择简单、
可维护且支持数据库事务的方案。不要只给示例或内存测试替身。

请完成源码、数据库迁移、隔离测试、无真实密钥的配置示例和部署说明。
根据我提供的服务器环境配置 HTTPS 和持久存储；若部署位置、域名或权限尚未提供，
先完成本地可运行项目，再列出所需信息。最后告诉我 MAA 应填写的 HTTPS 基础地址，
并说明如何安全取得账户同步密钥。以下域名、空间、设备和记录均为示例。

## 一、用途、账户和连接

- 中心只保存与分发会话标识、父分叉关系、模型、UTC 时间、Token 数及来源设备。
  不接收对话正文、工具参数、路径、采集目录、价格、费用或 GPT 登录凭据，
  不调用 OpenAI 接口，不做会话恢复。v1 不提供同步数据删除接口。
- 费用计算、日期归属、会话谱系去重和设备筛选由 MAA 完成，服务端保留原始记录。
  不按日期聚合，不自行合并不同会话记录，不要求父会话或 session 先于 usage 到达。
- 每个同步账户对应稳定的 spaceId 和一个当前有效的同步密钥。
  管理员创建账户时安全随机生成密钥（建议至少 32 随机字节），数据库存摘要，
  明文只在创建或重置时显示一次。提供受管理员鉴权保护的账户管理入口或 CLI，
  支持创建、停用、启用与重置密钥；管理凭据与同步密钥分开，不开放公共注册。
- 同一账户所有机器使用同一同步密钥，各自使用独立 deviceId。
  密钥决定授权空间，再核对请求中的 spaceId。设备 ID 仅表示来源，不能作为认证凭据。
  停用返回 403，重置后旧密钥返回 401；启停与换钥保留 spaceId、epoch 及全部历史。
- 三个同步接口都验证 Authorization: Bearer <账户同步密钥>，
  接受 Accept: application/json，push 使用 Content-Type: application/json。
  客户端不依赖 Cookie、浏览器 CORS、账号登录页或额外的自定义请求头。
- MAA 填写 HTTPS 基础地址，可以带路径前缀，例如 https://sync.example.com/maa；
  客户端会追加 /api/v1/codex-usage/handshake、push 或 changes。
  入口必须直接返回 JSON，不能重定向、要求验证码或使用后台统一响应包装。
  可以由反向代理终止 TLS，但应使用客户端信任的证书。
- 所有成功响应使用 HTTP 200。客户端单次超时 40 秒，不支持异步任务票据。
  请求和响应的未压缩 UTF-8 JSON 各自最多 1048576 字节（1 MiB）。
- 日志和追踪必须屏蔽 Authorization、明文密钥、游标和请求正文；
  同时检查反向代理的查询串日志，不让 changes 的 cursor 进入访问日志。
- spaceId 与 epoch 均为 1～256 UTF-8 字节、不含控制字符的持久字符串。
  epoch 表示数据代次，正常重启与换钥保持不变；数据回滚或重建会使既有确认、
  游标失效时才更换，并让旧 epoch 的请求返回 409。

## 二、数据结构与内容哈希

所有字段使用 camelCase。以下结构中的字段都必填，可空字段必须显式写 null。
字符串原样保存，不 trim、不改变大小写、不做 Unicode 归一化；
数据库比较和唯一约束须区分大小写。拒绝非法 UTF-8、孤立代理项和控制字符。

- Device：{"deviceId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"工作电脑"}。
  deviceId 为客户端生成的 32 个小写十六进制字符；
  name 为 1～128 UTF-8 字节，不能全为空白。
- sessionId、非 null 的 forkedFromId、model 为 1～512 UTF-8 字节，
  不能全为空白。旧日志可能使用 legacy:<deviceId>:<随机文件ID> 作为会话 ID，
  必须原样保留。未知模型可以是 unknown。
- createdAt、timestampMs 为 null 或 0..8210266876799999 的 UTC Unix 毫秒整数。
  未知时间不能替换成服务器接收时间。
- Token 均为 0..9007199254740991 的整数。
  totalUsage 为 null 或恰好五个 Token 整数组成的数组，顺序固定为
  [input_tokens,cached_input_tokens,output_tokens,reasoning_output_tokens,total_tokens]。
- inputTokens 已排除缓存命中部分；cacheReadTokens 单独传输；
  outputTokens 已包含推理输出，不得再累加推理 Token。
  累计计数器是原日志快照，不用单次明细重算或修正。
- occurrence 为 0..4294967295 的整数。有累计计数器时必须为 0；
  没有累计值时，它区分内容相同但实际重复发生的事件，不得丢弃或合并。
- JSON 数字原文必须为非负十进制整数，不接受数值字符串、小数、指数或负零。
  普通 JSON.parse 会丢失数字原文，应在解析阶段完成严格校验。
- session、usage、Device、WireRecord、RemoteRecord 与 push 请求拒绝未知字段；
  不要往记录里添加数据库主键或服务器时间。握手、确认与分页响应的最外层对象
  可以增加不改变原语义的字段。

SessionRecord 示例：

~~~json
{"kind":"session","sessionId":"thread-1","forkedFromId":null,"createdAt":1791504000000}
~~~

UsageRecord 示例：

~~~json
{"kind":"usage","sessionId":"thread-1","timestampMs":1791504000000,"model":"gpt-example","inputTokens":80,"cacheReadTokens":20,"outputTokens":10,"totalUsage":[100,20,10,0,110],"occurrence":0}
~~~

WireRecord 包装为 {"recordId":"<64位小写十六进制哈希>","record":<SessionRecord或UsageRecord>}。
recordId 按以下固定顺序构造 JSON 数组，再对其 UTF-8 字节计算 SHA-256：

~~~text
[1,"session",sessionId,forkedFromId,createdAt]
[1,"usage",sessionId,timestampMs,model,inputTokens,cacheReadTokens,outputTokens,totalUsage,occurrence]
~~~

数组使用紧凑 JSON，无 BOM、额外空白或末尾换行；整数为十进制，不转义斜杠 /
或非 ASCII 字符，双引号和反斜杠使用标准 JSON 转义，空值写 null。
不要对 HTTP 原文、record 对象的键排序 JSON 或数据库 JSON 输出求哈希。
哈希不含来源设备 ID（legacy 会话 ID 自带的设备部分仍须保留）。
服务端必须重新计算并核对每条 recordId。

跨语言校验向量：每个数组是完整一行哈希输入，紧接着是 SHA-256 结果。

~~~text
[1,"session","thread-1",null,1791504000000]
fd3127c801e7b05afb5d900c6f091806b8e01e5f64a5b3ce70fe22fba89d5f67

[1,"usage","thread-1",1791504000000,"gpt-example",80,20,10,[100,20,10,0,110],0]
ba432543859b5784bca4c5293c5c41afdb1ba4be71677028f0d664854620df5f

[1,"usage","会话/\"1",null,"模型\\A",1,0,0,null,2]
8301484be9dc31c5b241658355f3e68a5efde92ac2e716a158783cf1974dcb22
~~~

## 三、固定的三个 v1 接口

### GET /api/v1/codex-usage/handshake

仅验证密钥并返回所属空间、数据代次及协议版本，无必填查询参数。
不要求预先登记设备，不要求 deviceId 或协议版本请求头。
MAA 的“测试连接”只调用该接口，不能因此创建设备。

~~~json
{"protocolVersion":1,"spaceId":"space-example","epoch":"epoch-example","capabilities":["exclude-device-records-v1"]}
~~~

capabilities 是可选字符串数组，缺省或空数组表示仅支持原有完整下载。
本次实现须声明 exclude-device-records-v1，表示支持下述本机记录过滤与空中间页。
客户端忽略未知能力；旧客户端忽略新增响应字段。该字段不属于 push 请求。

### POST /api/v1/codex-usage/push

最多 500 条 WireRecord，整个请求不超过 1 MiB；同批重复 recordId 返回 400。
records 可以为空，用于登记设备或更新名称。完整请求示例：

~~~json
{
  "protocolVersion": 1,
  "spaceId": "space-example",
  "epoch": "epoch-example",
  "device": {"deviceId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"工作电脑"},
  "records": [
    {
      "recordId": "fd3127c801e7b05afb5d900c6f091806b8e01e5f64a5b3ce70fe22fba89d5f67",
      "record": {"kind":"session","sessionId":"thread-1","forkedFromId":null,"createdAt":1791504000000}
    },
    {
      "recordId": "ba432543859b5784bca4c5293c5c41afdb1ba4be71677028f0d664854620df5f",
      "record": {"kind":"usage","sessionId":"thread-1","timestampMs":1791504000000,"model":"gpt-example","inputTokens":80,"cacheReadTokens":20,"outputTokens":10,"totalUsage":[100,20,10,0,110],"occurrence":0}
    }
  ]
}
~~~

成功响应：

~~~json
{
  "spaceId": "space-example",
  "epoch": "epoch-example",
  "acknowledgedIds": [
    "fd3127c801e7b05afb5d900c6f091806b8e01e5f64a5b3ce70fe22fba89d5f67",
    "ba432543859b5784bca4c5293c5c41afdb1ba4be71677028f0d664854620df5f"
  ]
}
~~~

acknowledgedIds 必须恰好包含本批全部 ID，每个一次，顺序不限，
包括已经存在的记录。空 records 返回空 acknowledgedIds。
完整校验后在一个数据库事务中提交设备、内容、来源和变更流，
持久提交成功后才响应；任何一条失败都回滚整批，包括设备改名。
以 (spaceId,deviceId,recordId) 保证幂等；重复请求、并发重传和响应丢失后的重试
都是成功的空操作，不增加记录或变更。相同内容由 A、B 两台设备上传时，
必须保留两个来源，分别进入变更流，不能只保留第一个上传设备。

### GET /api/v1/codex-usage/changes

查询参数：必填 spaceId、epoch；可选 limit（1～500，默认 500）及 cursor。
cursor 是非空、不透明字符串，最多 4096 UTF-8 字节；省略表示从头下载。
可选 excludeDeviceId 为 32 个小写十六进制字符；缺省返回所有来源。
客户端仅在握手声明 exclude-device-records-v1 时发送，参数非法返回 400。
传入时只省略该设备的 records，devices 仍返回，同内容的其它设备来源不得过滤。
它只用于筛选，不作为鉴权依据；不要求提供设备注册或账号 ID。

~~~http
GET /api/v1/codex-usage/changes?spaceId=space-example&epoch=epoch-example&limit=500
Authorization: Bearer <账户同步密钥>
Accept: application/json
~~~

响应示例（session 可以出现在另一页，但必须完整下载）：

~~~json
{
  "spaceId": "space-example",
  "epoch": "epoch-example",
  "records": [
    {
      "deviceId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "item": {
        "recordId": "ba432543859b5784bca4c5293c5c41afdb1ba4be71677028f0d664854620df5f",
        "record": {"kind":"usage","sessionId":"thread-1","timestampMs":1791504000000,"model":"gpt-example","inputTokens":80,"cacheReadTokens":20,"outputTokens":10,"totalUsage":[100,20,10,0,110],"occurrence":0}
      }
    }
  ],
  "devices": [{"deviceId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"工作电脑"}],
  "nextCursor": "opaque-checkpoint-token",
  "hasMore": false
}
~~~

records 中每项严格使用 deviceId + item，item 内才是 recordId + record。
records 和 devices 必须同时存在；没有数据也返回两个空数组、
非空 nextCursor 和 hasMore:false。只有设备改名时，records 为空也是有效页。
未指定 excludeDeviceId 时，下载必须包含当前设备自身上传的来源，保持旧客户端行为。
过滤时允许两个数组都为空且 hasMore:true，但 nextCursor 必须推进。
读取不是消费，多台设备的游标互不影响。

## 四、持久存储、分页与恢复

建议持久实体：账户及密钥摘要、空间及 epoch、设备、不可变记录、
记录来源关系、不可变变更流。记录唯一键为 (spaceId,recordId)，
来源唯一键为 (spaceId,deviceId,recordId)，设备为 (spaceId,deviceId)。
sessionId 不能作为 session 内容的唯一键，元信息修正会产生新 recordId。
用数据库唯一约束和事务保障幂等，不能仅用进程内状态。

每个空间的变更序号按可见的提交顺序单调递增。建议用空间行锁保护序号分配、
epoch 复核和整批提交，锁持有到事务结束；普通自增序列不保证提交顺序。
否则序号 11 先提交、10 后提交时，下载可能永久跳过 10。
序号不要经过 JavaScript 不安全整数转换。
只有新增来源关系或设备真实变化才追加变更，设备变更保存当时名称快照。

必须实现固定上界分页：

1. 无游标从 0 开始；检查点游标从上轮结束位置之后开始，
   两者均捕获当前已提交最大序号作为本轮上界。
2. 中间分页游标保留当前位置和本轮上界，后续页不得重新抬高上界。
3. 按序扫描当前位置与上界之间的连续变更前缀，每页最多扫描 limit 个原始变更。
   records.length + devices.length 不超过 limit，整个响应不超过 1 MiB，
   字节数包含包装、游标和 JSON 转义。下一条超限时留给下一页，
   仅明确匹配 excludeDeviceId 的记录可以省略并推进游标；其它未返回条目不得跳过。
   不能截断 JSON 或一条记录，过滤不改变本轮固定上界。
4. 上界内尚有内容时 hasMore:true，游标必须前进。
   只有携带 excludeDeviceId 的请求允许空中间页；未过滤时至少一个数组非空。
   各数组保留变更顺序；设备名称采用历史快照，不能把上界之后的改名提前返回。
5. 到达上界时 hasMore:false，返回检查点游标。下一轮从该检查点捕获新上界，
   不能让旧上界永久冻结增量。最终空页允许重复原检查点。
6. 游标绑定授权空间、epoch、位置和上界，使用持久随机令牌或服务端签名防伪；
   签名密钥跨重启保留。无效、错空间或失效游标返回 409，不静默跳过。
   不设置常规过期时间，保留完整历史和变更流；正常重启后可继续分页。
7. 过滤请求使用带 excludeDeviceId 的 v2 签名游标载荷，旧请求继续使用 v1 载荷。
   过滤游标必须匹配同一过滤设备，省略或改变参数返回 409；旧游标保持可读。
   旧检查点允许开启过滤，旧中间页须先按原条件读完，不能中途改变条件。

MAA 仅在确认完整上传响应后保存上传确认；下载数据、设备、游标和下载模式同事务落盘。
上传确认不能推进下载游标，服务器必须允许重读同一页。
支持过滤时，首次接入、清理缓存或同步重置先完整下载历史，完成后才开启本机过滤，
再上传本机待同步记录并过滤下载本轮增量。完整下载中断后重启须继续原模式与游标。
epoch 改变或游标失效时，重新握手、清除确认和游标，再按上述顺序恢复并上传本机历史。
旧服务端维持先上传再完整下载；能力消失时仅重置下载游标和模式，保留上传确认再回退。
计数只累计实际收到并落库的 records，设备信息不计数；恢复与增量下载在同轮累计。
不要把设备缓存当作服务端备份：下载内容不会被当作本机记录重新上传。
请提供数据库一致性备份、恢复步骤，以及会使旧确认失效时更换 epoch 的操作方式。
保留空间信息、幂等关系、变更序号和游标签名材料；重启测试必须使用真实持久数据。

客户端清理缓存只是本机操作，后续下载可恢复；删除 Codex 原日志不要求中心删除数据。
更换服务器、密钥或空间会隔离客户端缓存。服务端启停或换钥不清空历史，
已送达的请求按其有效密钥和空间完成事务，不需要撤回接口。

## 五、错误与兼容

错误正文使用 {"code":"invalid_request","message":"Invalid sync request"} 这类简洁对象，
不回显请求、密钥或堆栈。不要把存储暂时失败映射为 400。

| HTTP | code | 约定 |
| --- | --- | --- |
| 400 | invalid_request | 字段、哈希、协议版本无效；客户端暂停自动同步 |
| 401 | unauthorized | 密钥缺少、错误或已重置；客户端提示更换所有机器的密钥 |
| 403 | forbidden | 账户停用；启用后用户可手动重试 |
| 409 | reset_required | 空间、epoch 不符或游标失效；重新握手并重放历史 |
| 413 | payload_too_large | 请求超 1 MiB 或上传超 500 条 |
| 429 | rate_limited | 返回 Retry-After；客户端按固定退避重试 |
| 5xx | server_error | 暂时故障；客户端保留数据并重试 |

网络、超时、429 和 5xx 按 5/10/20/30 分钟退避，上限 30 分钟。
不要依赖客户端按 Retry-After 精确调度；409 当轮最多自动恢复一次。
认证、协议或响应校验错误会暂停自动同步，用户修正配置或手动操作后再试。
v1 的字段、Token 口径、哈希、来源幂等键与删除语义固定；
不兼容变更必须使用新协议版本和新路径，不能仍返回 protocolVersion:1。

## 六、验收与交付

用隔离数据库、临时密钥和合成记录测试，不能读取真实 GPT 凭据或会话日志。
至少覆盖下列场景，通过真实 HTTP 入口验证，交付测试命令和实际结果：

- 三个哈希向量、中文与转义、最大整数、字节长度、未知字段、非法数字和超限请求。
- 空中心、无日志设备、空 push 登记设备；仅 handshake 测试连接不登记设备。
- 两个独立设备首次全量、增量、相同记录双来源、分叉跨设备及祖先晚到。
- 并发重传、上传已提交但响应丢失、批次错误和数据库中途失败的完整回滚。
- 超过 500 个变更、1 MiB 字节分页、持续新增时固定上界、下一轮可见新增数据。
- 分页中断重试、服务进程和数据库连接重启后续传、延迟提交不丢较小序号。
- 仅设备改名、分页期间再次改名、同名重复 push 不产生额外变更。
- 两个空间隔离、错钥 401、停用 403、换钥保留历史、游标伪造和 epoch 变化返回 409。
- MAA 本机清理缓存后能重新下载，原日志删除后中心历史保留；
  有桌面运行环境时验证两台 MAA 的总览、托盘与设备筛选。

双设备复制记录的最小断言：A、B 都上传上述 usage 时，中心保留两个来源，
MAA 全设备总计仍为输入 80、缓存读取 20、输出 10；按 A 或 B 均可看到该事件。
无累计值而 occurrence 不同的合法事件必须分别保留。

交付可运行源码、依赖锁文件、迁移、配置模板、测试、OpenAPI 3.1 JSON、
启动/升级/备份/恢复说明，以及 HTTPS 反向代理配置和受保护的账户管理方式。
为密钥和数据库使用持久且受权限保护的存储，示例配置只放占位符。
将管理员操作与三个公开同步接口分开；同步成功响应不能被后台中间件改写。

最后给出用户操作步骤：
1. 启动服务并创建同步账户，安全取得当前同步密钥。
2. 在各台 MAA 的“设置 → GPT 用量同步”填写相同 HTTPS 基础地址和账户同步密钥，
   每台机器保留独立设备身份，并填写便于辨认的设备名称。
3. 点击“测试连接”，再点击设置底部的“完成”保存配置并同步一次。
4. 检查另一台 MAA 能查看同步历史；换钥后更新该账户所有机器的密钥。

请明确区分已通过的验证与尚待部署环境验证的事项，不要把仅有示例响应当作联调通过。
`;
