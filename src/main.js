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
  const tabs = [...document.querySelectorAll("[role='tab']")];
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
