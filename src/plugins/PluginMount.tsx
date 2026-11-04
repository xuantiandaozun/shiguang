// 渲染一个插件挂载项：内置组件 / iframe 页面 / 声明式视图。出错只影响这一块。
import React from "react";
import type { MountedItem } from "./registry";
import { builtinComponent } from "./builtin";
import PluginFrame from "./PluginFrame";
import DeclarativeView from "./DeclarativeView";

class Boundary extends React.Component<{ name: string; children: React.ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(error: unknown) {
    return { error: String((error as Error)?.message ?? error) };
  }
  render() {
    if (this.state.error) {
      return (
        <div className="rounded-lg border border-rose-500/40 p-3 text-sm text-rose-400">
          「{this.props.name}」界面出错：{this.state.error}
        </div>
      );
    }
    return this.props.children;
  }
}

export default function PluginMount({ item, slot, fill = false, params }: { item: MountedItem; slot: string; fill?: boolean; params?: Record<string, unknown> }) {
  let body: React.ReactNode;
  const Builtin = item.plugin.builtin ? builtinComponent(item.component) : null;
  if (Builtin) {
    body = (
      <React.Suspense fallback={<div className="p-4 text-sm text-slate-500">加载中…</div>}>
        <Builtin />
      </React.Suspense>
    );
  } else if (item.entry) {
    body = <PluginFrame plugin={item.plugin} entry={item.entry} slot={slot} fill={fill} params={params} />;
  } else if (item.view) {
    body = <DeclarativeView plugin={item.plugin} view={item.view} params={params} />;
  } else {
    body = <div className="text-sm text-rose-400">插件 {item.plugin.id} 的 {slot} 没有可显示的内容（需要 entry 或 view）</div>;
  }
  return <Boundary name={item.label ?? item.plugin.name}>{body}</Boundary>;
}
