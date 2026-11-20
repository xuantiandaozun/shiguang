import { useEffect, useRef, useState } from "react";
import { openReplyReader } from "../lib/reply-reader";
import { ipc } from "../lib/ipc";
import { playBase64WavChunks, stopActiveTtsPlayback } from "../lib/audio";

export default function ReplyActions({
  content,
  dark = false,
  deepSeekEnabled = false,
  canContinue = false,
  onContinue,
  onRegenerate,
  disabled = false,
}: {
  content: string;
  dark?: boolean;
  deepSeekEnabled?: boolean;
  canContinue?: boolean;
  onContinue?: () => void;
  onRegenerate?: () => void;
  disabled?: boolean;
}) {
  const [feedback, setFeedback] = useState("");
  const [opening, setOpening] = useState(false);
  const [ttsState, setTtsState] = useState<"idle" | "loading" | "playing">("idle");
  const stopTtsRef = useRef<(() => void) | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    return () => {
      clearTimeout(timer.current);
      stopTtsRef.current?.();
    };
  }, []);

  function notify(text: string) {
    setFeedback(text);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setFeedback(""), 4000);
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(content);
      notify("已复制");
    } catch {
      notify("复制失败，请重试");
    }
  }

  async function view() {
    setOpening(true);
    try {
      await openReplyReader(content);
    } catch {
      notify("打开失败，请重试");
    } finally {
      setOpening(false);
    }
  }

  async function toggleSpeak() {
    if (ttsState === "playing") {
      stopTtsRef.current?.();
      stopActiveTtsPlayback();
      stopTtsRef.current = null;
      setTtsState("idle");
      return;
    }
    if (ttsState === "loading") return;
    setTtsState("loading");
    try {
      const chunks = await ipc.deepseekTts(content);
      if (!chunks.length) {
        setTtsState("idle");
        notify("无可朗读文本");
        return;
      }
      setTtsState("playing");
      stopTtsRef.current = playBase64WavChunks(
        chunks,
        () => {
          stopTtsRef.current = null;
          setTtsState("idle");
        },
        (err) => {
          stopTtsRef.current = null;
          setTtsState("idle");
          notify(err);
        },
      );
    } catch (e) {
      setTtsState("idle");
      notify(String(e).replace(/^Error:\s*/, ""));
    }
  }

  const buttonClass = dark
    ? "rounded px-2 py-0.5 text-[11px] text-slate-400 hover:bg-slate-700/70 hover:text-slate-100 disabled:opacity-40 transition"
    : "rounded px-2 py-1 text-xs text-[#646a73] hover:bg-[#f3f4f6] hover:text-[#1f2329] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[#b57520] disabled:opacity-40";
  const barClass = dark
    ? "mt-2 flex flex-wrap items-center gap-1 border-t border-slate-700/60 pt-1.5"
    : "mt-2 flex flex-wrap items-center gap-1 border-t border-[#edf0f3] pt-1.5";
  const statusClass = dark ? "px-1 text-[11px] text-slate-400" : "px-1 text-xs text-[#646a73]";

  return (
    <div className={barClass}>
      <button type="button" onClick={() => void copy()} className={buttonClass} title="复制这一条回复的 Markdown 原文">
        复制
      </button>
      {deepSeekEnabled && (
        <button
          type="button"
          onClick={() => void toggleSpeak()}
          disabled={ttsState === "loading"}
          className={`${buttonClass} ${ttsState === "playing" ? (dark ? "text-sky-300" : "text-sky-600") : ""}`}
          title="使用 DeepSeek 语音合成朗读此回复"
        >
          {ttsState === "loading" ? "合成中…" : ttsState === "playing" ? "停止朗读" : "朗读"}
        </button>
      )}
      <button
        type="button"
        onClick={() => void view()}
        disabled={opening}
        className={buttonClass}
        title="在独立大窗口中阅读这一条回复"
      >
        {opening ? "正在打开…" : "单独查看"}
      </button>
      {deepSeekEnabled && canContinue && onContinue && (
        <button
          type="button"
          onClick={onContinue}
          disabled={disabled}
          className={buttonClass}
          title="使用 DeepSeek 对话前缀续写继续生成"
        >
          继续生成
        </button>
      )}
      {onRegenerate && (
        <button
          type="button"
          onClick={onRegenerate}
          disabled={disabled}
          className={buttonClass}
          title="重新生成此条回复"
        >
          重新生成
        </button>
      )}
      {feedback && (
        <span role="status" className={statusClass}>
          {feedback}
        </span>
      )}
    </div>
  );
}
