import React, { useEffect } from "react";
import ReactDOM from "react-dom/client";
import "./index.css";
import "./theme.css";
import "./buttons.css";
import { ipc } from "./lib/ipc";
import { BUILTIN_WINDOWS, builtinComponent } from "./plugins/builtin";
import PluginMount from "./plugins/PluginMount";
import { startPluginRegistry, usePluginStore, usePluginWindows } from "./plugins/registry";

const hash = window.location.hash.replace(/^#\/?/, "");
const route = hash.split("?")[0] || "floatball";
const params = Object.fromEntries(new URLSearchParams(hash.split("?")[1] ?? ""));

// 插件声明的窗口沿用主窗口的浅色主题。
document.body.dataset.window = route in BUILTIN_WINDOWS ? route : "main";

/** 内置窗口（核心插件等声明的 builtin 组件）立即渲染，不等插件清单。 */
function BuiltinWindow({ name }: { name: string }) {
  const Component = builtinComponent(BUILTIN_WINDOWS[name]) ?? builtinComponent(BUILTIN_WINDOWS.floatball)!;
  if (name === "terminal") {
    return (
      <React.Suspense fallback={<div className="h-screen bg-slate-950 p-4 text-slate-300">正在打开终端…</div>}>
        <Component />
      </React.Suspense>
    );
  }
  return <Component />;
}

/** 用户插件声明的独立窗口（ui.windows 里的 route）。 */
function PluginWindow() {
  const windows = usePluginWindows();
  const loaded = usePluginStore((s) => s.list !== null);
  useEffect(() => startPluginRegistry(), []);
  const item = windows.find((w) => w.route === route);
  if (!loaded) return <div className="p-4 text-sm text-slate-500">加载中…</div>;
  if (!item) return <div className="p-4 text-sm text-rose-400">没有找到插件窗口「{route}」，插件可能已停用或删除。</div>;
  return (
    <div className="h-screen p-3">
      <PluginMount item={item} slot="window" fill params={params} />
    </div>
  );
}

function Root() {
  useEffect(() => {
    startPluginRegistry();
    // 界面正常渲染：清除启动标记（连续启动失败会自动进入插件安全模式）。
    const timer = window.setTimeout(() => void ipc.pluginsBootOk().catch(() => {}), 1500);
    return () => window.clearTimeout(timer);
  }, []);
  return route in BUILTIN_WINDOWS ? <BuiltinWindow name={route} /> : <PluginWindow />;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>
);
