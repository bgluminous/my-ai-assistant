import { resetError } from "./shared.js";

const errors = {
  sync_invalid_url: "请填写 HTTPS 服务器地址，不包含账号、密码、查询参数或片段。",
  sync_key_required: "请填写同步密钥，或清空服务器地址以停止同步。",
  sync_invalid_key: "同步密钥须为不含空格的可见 ASCII 字符，最长 4096 字符。",
  sync_invalid_device: "设备名称无效，最多 128 个 UTF-8 字节且不能包含控制字符。",
  sync_auth: "同步密钥无效或没有访问权限，请检查配置。",
  sync_unauthorized: "同步密钥无效或已被重置。请从服务端管理员处取得当前密钥，并更新该账户所有机器的配置。",
  sync_forbidden: "同步账户已停用。请联系服务端管理员启用账户，再到统计页点击“刷新”。本机历史仍保留。",
  sync_network: "无法连接同步服务器或请求超时，已保留本机数据。",
  sync_server: "同步服务器暂时不可用，将在后续刷新时重试。",
  sync_rate_limited: "同步服务器限制了请求频率，将在后续刷新时重试。",
  sync_protocol_version: "服务器协议版本不兼容，需要 v1 协议。",
  sync_protocol_invalid: "服务器响应不符合同步协议，已暂停自动同步。",
  sync_response_too_large: "服务器返回的数据超过协议限制，已暂停自动同步。",
  sync_invalid_record: "收到无效用量记录，未保存该页数据。",
  sync_reset_required: "服务器数据代次或游标已改变，请重新同步。",
  sync_not_configured: "尚未配置同步服务器和密钥。",
  sync_cancelled: "配置或缓存已更改，本轮同步已取消。",
  sync_task_failed: "同步任务已中断，请点击刷新重试。",
};
export function syncError(error) {
  const code = resetError(error);
  if (code.startsWith("sync_store_failed")) return "用量存档读写失败，请检查磁盘空间与文件权限。";
  return errors[code] || code;
}
