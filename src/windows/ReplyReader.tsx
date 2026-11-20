import { useEffect, useRef, useState } from "react";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Markdown from "../components/Markdown";
import { save } from "@tauri-apps/plugin-dialog";
import { ipc, onEvent } from "../lib/ipc";
import { isDeepSeekBaseUrl } from "../lib/providers";
import { playBase64WavChunks, stopActiveTtsPlayback } from "../lib/audio";

export default function ReplyReader() {
  const [content, setContent] = useState<string | null>(null);
  const [feedback, setFeedback] = useState("");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [deepSeekEnabled, setDeepSeekEnabled] = useState(false);
  const [ttsState, setTtsState] = useState<"idle" | "loading" | "playing">("idle");
  const stopTtsRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    const syncSettings = () => {
      ipc.getSettings().then((s) => setDeepSeekEnabled(isDeepSeekBaseUrl(s.base_url))).catch(() => {});
    };
    syncSettings();
    let stopSettings: (() => void) | undefined;
    void onEvent("settings-changed", syncSettings).then((u) => { stopSettings = u; });
    return () => {
      stopSettings?.();
      stopTtsRef.current?.();
    };
  }, []);
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    const timer = window.setTimeout(() => setError("未收到回复，请关闭窗口后重新打开。"), 15000);
    void listen<string>("reply-reader-content", (event) => {
      if (!disposed) { clearTimeout(timer); setContent(event.payload); setError(""); }
    }).then(async (unlisten) => {
      if (disposed) { unlisten(); return; }
      stop = unlisten;
      await emitTo("chat", "reply-reader-ready", getCurrentWindow().label);
    }).catch(() => { if (!disposed) setError("无法加载回复，请关闭窗口后重试。"); });
    return () => { disposed = true; clearTimeout(timer); stop?.(); };
  }, []);

  async function saveMarkdown() {
    if (content === null || saving) return;
    setSaving(true);
    setFeedback("");
    try {
      const title = content.split("\n").map(line => line.trim()).find(Boolean)
        ?.replace(/^#{1,6}\s*/, "").replace(/[<>:"/\\|?*\x00-\x1f]/g, "_")
        .replace(/[. ]+$/, "").slice(0, 60) || "AI回复";
      const selected = await save({
        title: "保存 Markdown", defaultPath: `${title}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!selected) return;
      // 不悄悄追加后缀，避免绕过系统对最终目标文件的覆盖确认。
      if (!/\.md$/i.test(selected)) { setFeedback("请使用 .md 扩展名重新保存"); return; }
      await ipc.saveReplyMarkdown(selected, content);
      setFeedback(`已保存至 ${selected}`);
    } catch (e) { setFeedback(`保存失败：${String(e)}`); }
    finally { setSaving(false); }
  }

  async function copy() {
    try { await navigator.clipboard.writeText(content ?? ""); setFeedback("已复制"); }
    catch { setFeedback("复制失败，请选择正文后复制"); }
  }
  async function toggleSpeak() {
    if (!content) return;
    if (ttsState === "playing") {
      stopTtsRef.current?.();
      stopActiveTtsPlayback();
      stopTtsRef.current = null;
      setTtsState("idle");
      return;
    }
    if (ttsState === "loading") return;
    setTtsState("loading");
    setFeedback("");
    try {
      const chunks = await ipc.deepseekTts(content);
      if (!chunks.length) { setTtsState("idle"); setFeedback("无可朗读文本"); return; }
      setTtsState("playing");
      stopTtsRef.current = playBase64WavChunks(
        chunks,
        () => { stopTtsRef.current = null; setTtsState("idle"); },
        (err) => { stopTtsRef.current = null; setTtsState("idle"); setFeedback(err); },
      );
    } catch (e) {
      setTtsState("idle");
      setFeedback(String(e));
    }
  }
  return <div className="flex h-full flex-col bg-white text-[#1f2329]">
    <header className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b border-[#e2e5e9] px-6 py-3">
      <h1 className="text-sm font-semibold">回复详情</h1>
      <div className="flex flex-wrap items-center gap-3">
        {deepSeekEnabled && (
          <button disabled={!content || ttsState === "loading"} onClick={() => void toggleSpeak()} className="rounded-md border border-[#d5d9df] px-3 py-1.5 text-sm hover:bg-[#f3f4f6] disabled:opacity-40">{ttsState === "loading" ? "合成中…" : ttsState === "playing" ? "停止朗读" : "朗读"}</button>
        )}
        <button disabled={content === null || saving} onClick={() => void saveMarkdown()} className="rounded-md border border-[#d5d9df] px-3 py-1.5 text-sm hover:bg-[#f3f4f6] disabled:opacity-40">{saving ? "正在保存…" : "保存 MD"}</button>
        <button disabled={!content} onClick={() => void copy()} className="rounded-md border border-[#d5d9df] px-3 py-1.5 text-sm hover:bg-[#f3f4f6] disabled:opacity-40">复制原文</button>
      </div>
    </header>
    {feedback && <div role="status" className="shrink-0 break-all border-b border-[#e2e5e9] px-6 py-2 text-xs text-[#646a73]">{feedback}</div>}
    <main className="min-h-0 flex-1 overflow-auto scrollbar-thin">
      <article className="reply-reader-content mx-auto max-w-[880px] px-6 py-8 sm:px-10">
        {content !== null ? <Markdown content={content} /> : <p role="status" className="text-sm text-[#646a73]">{error || "正在加载回复…"}</p>}
      </article>
    </main>
  </div>;
}
