import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { emitTo, listen } from "@tauri-apps/api/event";

/** Transfer only this reply after the reader has subscribed; no disk/browser storage. */
export async function openReplyReader(content: string) {
  const label = `reply-${crypto.randomUUID()}`;
  const unlisten = await listen<string>("reply-reader-ready", (event) => {
    if (event.payload === label) void emitTo(label, "reply-reader-content", content);
  });
  const timer = window.setTimeout(unlisten, 30000);
  try {
    const reader = new WebviewWindow(label, {
      url: "/index.html#reply-reader", title: "查看回复 · 拾光",
      width: 960, height: 760, minWidth: 480, minHeight: 360,
      center: true, resizable: true,
    });
    await new Promise<void>((resolve, reject) => {
      void reader.once("tauri://created", () => resolve());
      void reader.once("tauri://error", (event) => reject(new Error(String(event.payload))));
    });
  } catch (error) {
    clearTimeout(timer);
    unlisten();
    throw error;
  }
}
