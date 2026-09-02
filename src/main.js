import { setupDesktopGuards } from "./shared.js";
import { initAccounts } from "./accounts.js";
import { initUsage } from "./usage.js";
import { initAudit } from "./audit.js";
import { initPricing } from "./pricing.js";
import { initProxy } from "./proxy.js";
import { initSettings } from "./settings.js";
import { initAbout } from "./about.js";

function setupTheme() {
  const root = document.documentElement;
  let saved = null;
  try { saved = localStorage.getItem("theme"); } catch { /* ignore */ }
  root.dataset.theme = saved === "light" || saved === "dark" ? saved : "dark";
  const toggle = document.querySelector("#theme-toggle");
  if (!toggle) return;
  toggle.addEventListener("click", () => {
    const next = root.dataset.theme === "light" ? "dark" : "light";
    root.dataset.theme = next;
    try { localStorage.setItem("theme", next); } catch { /* ignore */ }
    window.dispatchEvent(new CustomEvent("themechange", { detail: { theme: next } }));
  });
}

function setupTabs() {
  // 只绑定带 data-panel 的主导航 tab。用量页的时间跨度分段同样是 role="tab"
  // （自带 tablist 容器与选中态管理），若一并绑定，点击会以 undefined panelId
  // 调 select——所有面板被隐藏、导航选中态被清空，页面看起来「全部消失」。
  const tabs = [...document.querySelectorAll("[role='tab'][data-panel]")];
  const panels = [...document.querySelectorAll("[role='tabpanel']")];
  function select(panelId) {
    for (const tab of tabs) {
      const selected = tab.dataset.panel === panelId;
      tab.setAttribute("aria-selected", String(selected));
      tab.tabIndex = selected ? 0 : -1;
    }
    for (const panel of panels) panel.hidden = panel.id !== panelId;
    // 通知各模块面板切换（用量统计页借此自动加载数据）
    window.dispatchEvent(new CustomEvent("panelshown", { detail: { id: panelId } }));
  }
  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => select(tab.dataset.panel));
    tab.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      const dir = event.key === "ArrowRight" ? 1 : -1;
      const next = tabs[(index + dir + tabs.length) % tabs.length];
      select(next.dataset.panel);
      next.focus();
    });
  });
}

// 主窗口以隐藏状态创建（tauri.conf.json visible:false + 深色底色，消除启动白闪），
// 首帧渲染完成后再显示窗口；非 Tauri 环境（浏览器直开调试）静默跳过。
// 前端初始化若中途异常，Rust 侧还有超时兜底 show()（见 lib.rs setup）。
function revealWindow() {
  try {
    const w = window.__TAURI__ && window.__TAURI__.window;
    if (!w || typeof w.getCurrentWindow !== "function") return;
    const win = w.getCurrentWindow();
    void win
      .show()
      .then(() => win.setFocus())
      .catch(() => {});
  } catch { /* ignore */ }
}

setupDesktopGuards();
setupTheme();
setupTabs();
initAccounts();
initUsage();
initAudit();
initPricing();
initProxy();
initSettings();
initAbout();
// 两帧 rAF 确保浏览器完成首次布局与绘制后才显示窗口
requestAnimationFrame(() => requestAnimationFrame(revealWindow));
