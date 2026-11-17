import { createModal } from "./modal.js";
import { resetError } from "./shared.js";

let dialogSequence = 0;
export function dialog(title, mode = "edit") {
  const root = document.createElement("dialog");
  root.className = "opencode-dialog";
  root.innerHTML = '<div class="modal-head"><h3></h3><button type="button" class="icon-btn" aria-label="关闭">×</button></div><div class="modal-body"><div class="opencode-content"></div><p class="status" role="status" hidden></p></div><div class="modal-foot"><button type="button" class="secondary">取消</button></div>';
  const heading = root.querySelector("h3");
  heading.id = "opencode-dialog-title-" + (++dialogSequence);
  heading.textContent = title;
  root.setAttribute("aria-labelledby", heading.id);
  const content = root.querySelector(".opencode-content");
  const status = root.querySelector(".status");
  const foot = root.querySelector(".modal-foot");
  const cancel = foot.querySelector("button");
  let closed = false, busy = false, operation = null;
  let onCancel = async () => {};
  const modal = createModal(root, {
    mode: () => mode,
    isBusy: () => busy,
    onRequestClose: requestClose,
    onClose() { closed = true; root.remove(); void onCancel().catch(() => {}); },
  });
  function setStatus(kind, text) {
    status.className = "status" + (kind ? " " + kind : "");
    status.textContent = text;
    status.hidden = !text;
  }
  function close() {
    modal.close();
  }
  async function requestClose() {
    busy = true; cancel.disabled = true;
    try { await onCancel(); close(); }
    catch (error) { setStatus("bad", resetError(error)); }
    finally { busy = false; cancel.disabled = false; }
  }
  root.querySelector(".icon-btn").setAttribute("data-close", "");
  cancel.setAttribute("data-close", "");
  document.body.append(root); modal.open();
  return { root, content, setStatus, foot, cancel, close,
    get closed() { return closed; },
    busy(value) {
      if (value && !busy) operation = modal.beginSave();
      if (!value) { operation?.finish(); operation = null; }
      busy = value;
      modal.reset();
    },
    onCancel(fn) { onCancel = fn; },
  };
}

