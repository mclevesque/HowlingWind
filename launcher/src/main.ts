import { mount } from "svelte";

// If the URL has #debug-window, render the debug console instead of the main app
const isDebugWindow = window.location.hash === "#debug-window";

let app;
if (isDebugWindow) {
  const { default: DebugWindow } = await import("./DebugWindow.svelte");
  app = mount(DebugWindow, {
    target: document.getElementById("app")!,
  });
} else {
  const { default: App } = await import("./App.svelte");
  app = mount(App, {
    target: document.getElementById("app")!,
  });
}

export default app;
