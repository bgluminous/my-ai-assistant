// 所有主窗口弹窗共用关闭策略与层级；编辑窗需在 2 秒内再次按 Esc 或再次点击遮罩。
const stack = [];
const inertBefore = new Map();
const top = () => stack[stack.length - 1];
const focusableSelector = "button, input, select, textarea, a[href], [tabindex]";

function canFocus(node) {
  return node?.isConnected && !node.disabled && !node.closest("[hidden], [inert]")
    && (!node.getClientRects || node.getClientRects().length > 0);
}

function focusables(root) {
  return [...root.querySelectorAll(focusableSelector)].filter(node => node.tabIndex >= 0 && canFocus(node));
}

function focusInside(root) {
  const target = focusables(root)[0] || root;
  if (target === root) root.tabIndex = -1;
  target.focus();
}

function updateLayers() {
  for (const [node, inert] of inertBefore) node.toggleAttribute("inert", inert);
  inertBefore.clear();
  for (const [index, modal] of stack.entries()) {
    modal.reset();
    modal.root.style.zIndex = String(100 + index);
  }
  // 保留原有 inert，逐层隔离弹窗外的内容，兼容原生 dialog 与已有 div 弹窗。
  for (let node = top()?.root; node?.parentElement; node = node.parentElement) {
    for (const sibling of node.parentElement.children) {
      if (sibling === node) continue;
      inertBefore.set(sibling, sibling.hasAttribute("inert"));
      sibling.setAttribute("inert", "");
    }
    if (node.parentElement === document.body) break;
  }
}

document.addEventListener("keydown", event => {
  const modal = top();
  if (!modal) return;
  if (event.key === "Escape") {
    // 阻止原生 dialog 再触发 cancel；长按不计为第二次，也不延长确认时间。
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!event.repeat) modal.escape();
    return;
  }
  modal.reset();
  if (event.key === "Tab") {
    const items = focusables(modal.root);
    const index = items.indexOf(document.activeElement);
    if (!items.length) { event.preventDefault(); focusInside(modal.root); }
    else if (index < 0 || (event.shiftKey ? index === 0 : index === items.length - 1)) {
      event.preventDefault();
      items[event.shiftKey ? items.length - 1 : 0].focus();
    }
  }
}, true);

document.addEventListener("focusin", event => {
  const modal = top();
  if (modal && !modal.root.contains(event.target)) focusInside(modal.root);
});

export function closeAllModals() {
  for (const modal of [...stack].reverse()) modal.close();
}

export function createModal(root, {
  mode = () => "edit", isBusy = () => false, onRequestClose, onClose,
  closeSelector = "[data-close]",
} = {}) {
  const native = root.tagName === "DIALOG";
  const hint = document.createElement("p");
  hint.className = "modal-close-hint";
  hint.setAttribute("role", "status");
  hint.setAttribute("aria-live", "polite");
  hint.innerHTML = '<span class="modal-close-hint-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="8.5"/><path d="M12 11v5m0-8v.01"/></svg></span><span></span>';
  const hintText = hint.lastElementChild;
  hint.hidden = true;
  root.append(hint);
  let session = 0, active = false, trigger, armed = null, timer, pointerTarget = null, saving = null;
  const originalZIndex = root.style.zIndex;

  function reset() {
    armed = null;
    pointerTarget = null;
    hint.hidden = true;
    clearTimeout(timer);
  }
  function showHint(text) {
    hintText.textContent = text;
    hint.hidden = false;
    clearTimeout(timer);
    timer = setTimeout(reset, 2000);
  }
  function blocked() {
    if (!saving && !isBusy()) return false;
    reset();
    showHint("正在保存，请稍候。");
    return true;
  }
  function requestClose() {
    if (top() !== api || blocked()) return;
    reset();
    if (onRequestClose) return onRequestClose();
    api.close();
  }
  function confirmClose(source) {
    if (top() !== api || blocked()) return;
    if (mode() !== "edit" || (armed?.source === source && Date.now() - armed.at < 2000)) {
      void requestClose();
      return;
    }
    reset();
    armed = { source, at: Date.now() };
    showHint(source === "escape" ? "再次按 Esc 关闭" : "再次点击关闭");
  }
  function isBackdrop(event) {
    if (!native) return event.target === root.querySelector(".modal-backdrop");
    if (event.target !== root) return false;
    // 原生 dialog 的 ::backdrop 事件也以 dialog 为 target；面板内部留白不算遮罩。
    const rect = root.getBoundingClientRect?.();
    return !rect || event.clientX < rect.left || event.clientX > rect.right
      || event.clientY < rect.top || event.clientY > rect.bottom;
  }

  const api = {
    root, reset, requestClose,
    get session() { return session; },
    isCurrent(value) { return active && session === value; },
    get busy() { return !!saving || isBusy(); },
    open() {
      if (active) return;
      trigger = document.activeElement;
      active = true;
      session += 1;
      root.hidden = false;
      stack.push(api);
      updateLayers();
      if (native) root.showModal();
      if (!root.contains(document.activeElement)) focusInside(root);
    },
    close() {
      if (!active) return;
      const wasTop = top() === api;
      saving?.finish();
      active = false;
      session += 1;
      reset();
      stack.splice(stack.indexOf(api), 1);
      if (native) root.close();
      else root.hidden = true;
      root.style.zIndex = originalZIndex;
      updateLayers();
      onClose?.();
      if (wasTop) {
        if (canFocus(trigger) && (!top() || top().root.contains(trigger))) trigger.focus();
        else focusInside(top()?.root || document.querySelector("main") || document.body);
      }
    },
    escape() {
      confirmClose("escape");
    },
    // 每次保存绑定打开时的会话；旧请求完成不得关闭或解锁后来打开的表单。
    beginSave(buttons = []) {
      if (!active || top() !== api || blocked()) return null;
      const id = session;
      const controls = new Set([...buttons, ...root.querySelectorAll("input, select, textarea")]);
      const disabled = [...controls].map(button => [button, button.disabled]);
      for (const [button] of disabled) button.disabled = true;
      reset();
      const operation = {
        current: () => api.isCurrent(id),
        finish() {
          if (saving !== operation) return;
          saving = null;
          for (const [button, value] of disabled) button.disabled = value;
          reset();
        },
      };
      saving = operation;
      return operation;
    },
  };

  root.addEventListener("pointerdown", event => {
    if (top() !== api) return;
    pointerTarget = isBackdrop(event) ? event.target : null;
    if (!pointerTarget) reset();
  });
  root.addEventListener("pointercancel", reset);
  root.addEventListener("click", event => {
    const backdrop = pointerTarget === event.target && isBackdrop(event);
    pointerTarget = null;
    if (top() !== api) return;
    if (backdrop) {
      confirmClose("backdrop");
      return;
    }
    reset();
    if (event.target.closest(closeSelector) && !isBackdrop(event)) void requestClose();
  });
  for (const type of ["input", "change"]) root.addEventListener(type, reset);
  root.addEventListener("cancel", event => {
    event.preventDefault();
    if (top() === api) api.escape();
  });
  return api;
}
