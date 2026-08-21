import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { BrowserRecipe } from "../lib/ipc";

export default function BrowserRecipesTab() {
  const [recipes, setRecipes] = useState<BrowserRecipe[]>([]);
  const [error, setError] = useState("");
  const reload = useCallback(() => ipc.listBrowserRecipes().then(setRecipes).catch((e) => setError(String(e))), []);
  useEffect(() => { reload(); let off: (() => void) | undefined; onEvent("browser-recipes-changed", reload).then((u) => (off = u)); return () => off?.(); }, [reload]);
  return <div className="max-w-3xl space-y-3">
    <div><div className="text-sm font-medium text-slate-100">浏览器经验</div><p className="mt-1 text-xs text-slate-500">AI 仅在浏览器任务已验证完成后保存这些配方。它们不是任意脚本：运行前会检查当前站点和唯一候选；不匹配就停止并交回 AI。</p></div>
    {error && <div className="text-xs text-rose-400">{error}</div>}
    {recipes.length === 0 ? <div className="py-8 text-center text-sm text-slate-500">暂无已验证经验。完成可重复的网站操作后，可让 AI“保存为浏览器经验”。</div> : <div className="space-y-2">
      {recipes.map((recipe) => { const reliability = recipe.success_count + recipe.failure_count === 0 ? "尚未复用" : `成功 ${recipe.success_count} · 失败 ${recipe.failure_count}`; return <div key={recipe.id} className="rounded-lg border border-slate-700/60 bg-slate-900/45 px-3 py-3"><div className="flex items-start gap-3"><div className="min-w-0 flex-1"><div className="text-sm text-slate-100">{recipe.name}</div><div className="mt-1 text-xs text-slate-400">{recipe.goal}</div><div className="mt-1 text-[11px] text-slate-500">{recipe.site_pattern} · {reliability}{recipe.last_used_at ? ` · 最近 ${recipe.last_used_at}` : ""}</div></div><button onClick={() => { if (confirm(`删除浏览器经验「${recipe.name}」？`)) ipc.deleteBrowserRecipe(recipe.id).then(reload).catch((e) => setError(String(e))); }} className="shrink-0 text-xs text-slate-500 hover:text-rose-400">删除</button></div><details className="mt-2"><summary className="cursor-pointer text-[11px] text-slate-500 hover:text-slate-300">查看受控操作配方</summary><pre className="mt-2 max-h-52 overflow-auto whitespace-pre-wrap rounded bg-slate-950/60 p-2 text-[11px] leading-5 text-slate-300">{recipe.recipe_json}</pre></details></div> })}
    </div>}
  </div>;
}
