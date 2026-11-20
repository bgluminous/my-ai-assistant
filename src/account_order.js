import { invoke } from "./shared.js";

// 静默启动时托盘可能先打开，两端都将旧卡片顺序交给后端做一次性迁移。
export function loadAccountList() {
  let groups = [];
  try {
    const saved = JSON.parse(localStorage.getItem("accountGroupOrder"));
    if (Array.isArray(saved)) groups = saved.filter(kind => typeof kind === "string");
  } catch { /* 使用旧版默认顺序 */ }
  return invoke("accounts_list", { legacyGroups: groups });
}

export function orderedAccounts(accounts, order) {
  const remaining = new Map(accounts.map(account => [account.id, account]));
  const result = [];
  for (const id of order || []) {
    if (remaining.has(id)) result.push(remaining.get(id));
    remaining.delete(id);
  }
  return result.concat([...remaining.values()]);
}

/** 两端共用后端的顺序修订号，延迟的命令响应不能覆盖后来收到的移动。 */
export function trackAccountOrder() {
  let revision = -1, order = [];
  function apply(view) {
    const next = view?.orderRevision ?? 0;
    if (!Array.isArray(view?.accountOrder) || next <= revision) return false;
    revision = next;
    order = view.accountOrder;
    return true;
  }
  return {
    apply,
    rows: accounts => orderedAccounts(accounts, order),
    move: async (id, beforeId) => { apply(await invoke("accounts_move", { id, beforeId })); },
  };
}

export function orderHandle(label) {
  const handle = document.createElement("button");
  handle.type = "button";
  handle.className = "account-order-handle";
  handle.title = "拖动或使用上下方向键调整「" + label + "」的位置";
  handle.setAttribute("aria-label", handle.title);
  handle.innerHTML = '<svg viewBox="0 0 16 20" fill="currentColor" aria-hidden="true"><circle cx="5" cy="4" r="1.4"/><circle cx="11" cy="4" r="1.4"/><circle cx="5" cy="10" r="1.4"/><circle cx="11" cy="10" r="1.4"/><circle cx="5" cy="16" r="1.4"/><circle cx="11" cy="16" r="1.4"/></svg>';
  return handle;
}

/** 在动态列表上委托事件，松手后只提交被移动账户和目标账户的相对位置。 */
export function initRowOrder(list, { ids, save, render, onError, scrollBox }) {
  let drag = null, frame = 0, saving = false;
  const rows = () => [...list.children].filter(node => node.dataset.id);
  const rowFor = id => rows().find(row => row.dataset.id === id);
  const focus = id => rowFor(id)?.querySelector(".account-order-handle")?.focus();
  const announce = document.createElement("span");
  announce.className = "sr-only";
  announce.setAttribute("role", "status");
  (list.closest("table") || list).after(announce);

  async function commit(id, before) {
    saving = true;
    try {
      await save(id, before);
      announce.textContent = "账户已移至第 " + (ids().indexOf(id) + 1) + " 位";
    } catch (error) {
      onError(error);
    } finally {
      saving = false;
      render();
      focus(id);
    }
  }
  function release() {
    const previous = drag;
    drag = null;
    window.cancelAnimationFrame(frame);
    previous.row.classList.remove("account-order-moving");
    document.body.classList.remove("account-order-dragging");
    try { list.releasePointerCapture?.(previous.pointerId); } catch { /* 指针捕获已释放 */ }
    return previous;
  }
  function finish(cancelled) {
    if (!drag) return;
    const { row, moving, initial } = release();
    const id = row.dataset.id;
    const after = rows().map(node => node.dataset.id);
    if (moving && !cancelled && after.indexOf(id) !== initial.indexOf(id)) {
      void commit(id, after[after.indexOf(id) + 1] ?? null);
    } else {
      render();
      focus(id);
    }
  }
  function place() {
    const { row, y } = drag;
    const target = rows().find(other => {
      if (other === row) return false;
      const box = other.getBoundingClientRect();
      return y < box.top + box.height / 2;
    });
    if (target && row.nextElementSibling !== target) list.insertBefore(row, target);
    else if (!target && row !== list.lastElementChild) list.append(row);
  }
  function scroll() {
    if (!drag?.moving) return;
    const rect = scrollBox?.getBoundingClientRect();
    const top = rect?.top ?? 0, bottom = rect?.bottom ?? window.innerHeight;
    const delta = drag.y < top + 40 ? -12 : drag.y > bottom - 40 ? 12 : 0;
    if (delta) { (scrollBox || window).scrollBy(0, delta); place(); }
    frame = window.requestAnimationFrame(scroll);
  }
  list.addEventListener("pointerdown", event => {
    const handle = event.target.closest?.(".account-order-handle");
    if (!handle || saving || drag || event.button !== 0) return;
    event.preventDefault();
    handle.focus();
    list.setPointerCapture?.(event.pointerId);
    drag = { row: handle.closest("[data-id]"), initial: ids(), pointerId: event.pointerId,
      start: event.clientY, y: event.clientY, moving: false };
  });
  window.addEventListener("pointermove", event => {
    if (!drag || event.pointerId !== drag.pointerId) return;
    drag.y = event.clientY;
    if (!drag.moving && Math.abs(drag.y - drag.start) >= 5) {
      drag.moving = true;
      drag.row.classList.add("account-order-moving");
      document.body.classList.add("account-order-dragging");
      frame = window.requestAnimationFrame(scroll);
    }
    if (drag.moving) place();
  });
  window.addEventListener("pointerup", event => { if (drag?.pointerId === event.pointerId) finish(false); });
  window.addEventListener("pointercancel", event => { if (drag?.pointerId === event.pointerId) finish(true); });
  list.addEventListener("lostpointercapture", () => finish(true));
  window.addEventListener("blur", () => finish(true));
  list.addEventListener("keydown", event => {
    if (event.key === "Escape" && drag) { event.preventDefault(); finish(true); return; }
    const handle = event.target.closest?.(".account-order-handle");
    if (!handle || saving || drag || !["ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const id = handle.closest("[data-id]").dataset.id;
    const order = ids(), index = order.indexOf(id);
    if (event.key === "ArrowUp" && index > 0) void commit(id, order[index - 1]);
    if (event.key === "ArrowDown" && index >= 0 && index < order.length - 1) void commit(id, order[index + 2] ?? null);
  });
  return {
    // 状态刷新延至松手时重绘，其他窗口的增删或移动则立即结束旧预览。
    canRender() {
      if (!drag) return true;
      const current = ids();
      if (current.length === drag.initial.length && current.every((id, index) => id === drag.initial[index])) return false;
      release();
      return true;
    },
  };
}
