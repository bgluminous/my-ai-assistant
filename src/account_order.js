// 卡片顺序是界面偏好；各窗口读取同一份顺序，组内账户继续沿用套餐排序。
export const ACCOUNT_GROUP_ORDER_KEY = "accountGroupOrder";
const ORDER_CHANGED = "account-group-order-changed";
const DEFAULT_ORDER = ["cursor", "codex", "claude", "opencode"];

export function normalizeAccountGroupOrder(value) {
  return [...new Set([...(Array.isArray(value) ? value : []), ...DEFAULT_ORDER])]
    .filter(kind => DEFAULT_ORDER.includes(kind));
}

export function getAccountGroupOrder() {
  try { return normalizeAccountGroupOrder(JSON.parse(localStorage.getItem(ACCOUNT_GROUP_ORDER_KEY))); }
  catch { return [...DEFAULT_ORDER]; }
}

export function setAccountGroupOrder(value) {
  const order = normalizeAccountGroupOrder(value);
  localStorage.setItem(ACCOUNT_GROUP_ORDER_KEY, JSON.stringify(order));
  window.dispatchEvent(new CustomEvent(ORDER_CHANGED));
  return order;
}

/** 指针拖动和键盘上下键共用提交逻辑；取消拖动恢复开始时的顺序。 */
export function initAccountCardOrder() {
  const panel = document.querySelector("#accounts-panel");
  if (!panel || panel.dataset.orderReady) return;
  panel.dataset.orderReady = "true";
  const cards = new Map([...panel.querySelectorAll("[data-account-kind]")].map(card => [card.dataset.accountKind, card]));
  const current = () => [...panel.children].map(card => card.dataset.accountKind).filter(kind => cards.has(kind));
  const apply = order => { for (const kind of normalizeAccountGroupOrder(order)) if (cards.has(kind)) panel.append(cards.get(kind)); };
  const announce = document.createElement("span");
  announce.className = "sr-only"; announce.setAttribute("role", "status");
  panel.after(announce);
  let drag = null, frame = 0;

  function commit(order, kind) {
    try {
      setAccountGroupOrder(order);
      announce.textContent = `${cards.get(kind)?.querySelector("h3")?.textContent || "卡片"}已移至第 ${order.indexOf(kind) + 1} 位`;
    } catch {
      apply(getAccountGroupOrder());
      announce.textContent = "无法保存排序，请检查本地存储。";
    }
  }
  function place(y) {
    if (!drag?.moving) return;
    const card = cards.get(drag.kind);
    const target = [...panel.children].find(other => {
      if (other === card || !other.dataset.accountKind) return false;
      const rect = other.getBoundingClientRect();
      return y < rect.top + rect.height / 2;
    });
    if (target) { if (card.nextElementSibling !== target) panel.insertBefore(card, target); }
    else if (card !== panel.lastElementChild) panel.append(card);
  }
  function scroll() {
    if (!drag?.moving) return;
    const edge = 60;
    const delta = drag.y < edge ? -14 : drag.y > window.innerHeight - edge ? 14 : 0;
    if (delta) { window.scrollBy(0, delta); place(drag.y); }
    frame = window.requestAnimationFrame(scroll);
  }
  function finish(cancelled) {
    if (!drag) return;
    const previous = drag;
    drag = null;
    window.cancelAnimationFrame(frame);
    try { panel.releasePointerCapture?.(previous.pointerId); } catch { /* 已自动释放 */ }
    cards.get(previous.kind).classList.remove("account-card-moving");
    document.body.classList.remove("account-cards-dragging");
    if (cancelled) apply(previous.order);
    else if (previous.moving) commit(current(), previous.kind);
    previous.handle.focus();
  }
  for (const [kind, card] of cards) {
    const heading = card.querySelector(".section-heading h3");
    const handle = document.createElement("button");
    handle.type = "button"; handle.className = "account-card-handle";
    const label = heading.textContent.trim();
    handle.title = `拖拽调整${label}顺序，也可使用上下方向键`;
    handle.setAttribute("aria-label", handle.title);
    handle.innerHTML = '<svg viewBox="0 0 16 20" fill="currentColor" aria-hidden="true"><circle cx="5" cy="4" r="1.3"/><circle cx="11" cy="4" r="1.3"/><circle cx="5" cy="10" r="1.3"/><circle cx="11" cy="10" r="1.3"/><circle cx="5" cy="16" r="1.3"/><circle cx="11" cy="16" r="1.3"/></svg>';
    heading.prepend(handle);
    handle.addEventListener("pointerdown", event => {
      if (event.button !== 0 || drag) return;
      event.preventDefault(); handle.focus();
      panel.setPointerCapture?.(event.pointerId);
      drag = { kind, handle, pointerId: event.pointerId, start: event.clientY, y: event.clientY, order: current(), moving: false };
    });
    handle.addEventListener("dragstart", event => event.preventDefault());
    handle.addEventListener("keydown", event => {
      if (event.key === "Escape" && drag) { event.preventDefault(); finish(true); return; }
      if (drag || !["ArrowUp", "ArrowDown"].includes(event.key)) return;
      event.preventDefault();
      const order = current(), index = order.indexOf(kind), next = index + (event.key === "ArrowUp" ? -1 : 1);
      if (next < 0 || next >= order.length) return;
      [order[index], order[next]] = [order[next], order[index]];
      apply(order); commit(order, kind); handle.focus();
    });
  }
  window.addEventListener("pointermove", event => {
    if (!drag || event.pointerId !== drag.pointerId) return;
    drag.y = event.clientY;
    if (!drag.moving && Math.abs(drag.y - drag.start) >= 5) {
      drag.moving = true;
      cards.get(drag.kind).classList.add("account-card-moving");
      document.body.classList.add("account-cards-dragging");
      frame = window.requestAnimationFrame(scroll);
    }
    place(drag.y);
  });
  window.addEventListener("pointerup", event => { if (drag?.pointerId === event.pointerId) finish(false); });
  window.addEventListener("pointercancel", event => { if (drag?.pointerId === event.pointerId) finish(true); });
  window.addEventListener("blur", () => finish(true));
  const restore = () => { finish(true); apply(getAccountGroupOrder()); };
  window.addEventListener(ORDER_CHANGED, restore);
  window.addEventListener("storage", event => { if (event.key === ACCOUNT_GROUP_ORDER_KEY || event.key === null) restore(); });
  apply(getAccountGroupOrder());
}
