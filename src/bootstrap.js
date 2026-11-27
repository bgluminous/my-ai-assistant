import { initializeLanguage } from "./i18n.js";

await initializeLanguage();
if (document.getElementById("tray-list")) await import("./tray_panel.js");
else await import("./main.js");
