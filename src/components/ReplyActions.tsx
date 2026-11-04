import { useEffect, useRef, useState } from "react";
import { openReplyReader } from "../lib/reply-reader";

export default function ReplyActions({ content }: { content: string }) {
  const [feedback, setFeedback] = useState("");
  const [opening, setOpening] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(timer.current), []);
  function notify(text: string) {
    setFeedback(text);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setFeedback(""), 4000);
  }
  async function copy() {
    try { await navigator.clipboard.writeText(content); notify("已复制"); }
    catch { notify("复制失败，请重试"); }
  }
  async function view() {
    setOpening(true);
    try { await openReplyReader(content); }
    catch { notify("打开失败，请重试"); }
    finally { setOpening(false); }
  }
  const buttonClass = "rounded px-2 py-1 text-xs text-[#646a73] hover:bg-[#f3f4f6] hover:text-[#1f2329] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[#b57520] disabled:opacity-40";
  return <div className="mt-2 flex flex-wrap items-center gap-1 border-t border-[#edf0f3] pt-1.5">
    <button type="button" onClick={() => void copy()} className={buttonClass} title="复制这一条回复的 Markdown 原文">复制</button>
    <button type="button" onClick={() => void view()} disabled={opening} className={buttonClass} title="在独立大窗口中阅读这一条回复">{opening ? "正在打开…" : "单独查看"}</button>
    <span role="status" className="px-1 text-xs text-[#646a73]">{feedback}</span>
  </div>;
}
