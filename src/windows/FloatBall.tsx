import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import {
  currentMonitor,
  getCurrentWindow,
  PhysicalPosition,
  PhysicalSize,
} from "@tauri-apps/api/window";
import { ipc } from "../lib/ipc";
import appIcon from "../assets/app-icon.png";

const BALL = 76; // 悬浮球窗口边长（逻辑像素）
const BAR_W = 10; // 贴边长条宽度
const BAR_H = 80; // 贴边长条高度
const DOCK_DIST = 40; // 停下时距左右边缘小于该值 → 收成贴边长条
const UNDOCK_DIST = 32; // 长条被拖离边缘超过该值 → 还原悬浮球
const SETTLE_MS = 260; // 拖动停止多久后判定为"停放"

type Mode = "ball" | "bar";

export default function FloatBall() {
  const [mode, setMode] = useState<Mode>("ball");
  const modeRef = useRef<Mode>("ball");
  const downPos = useRef<{ x: number; y: number } | null>(null);
  const busy = useRef(false);
  const queued = useRef<{ pos: PhysicalPosition; settling: boolean } | null>(null);
  const settleTimer = useRef<number | undefined>(undefined);

  const switchMode = (m: Mode) => {
    modeRef.current = m;
    setMode(m);
  };

  const onDown = (e: ReactMouseEvent) => {
    downPos.current = { x: e.screenX, y: e.screenY };
  };
  const onUp = (e: ReactMouseEvent) => {
    const d = downPos.current;
    downPos.current = null;
    if (d && Math.hypot(e.screenX - d.x, e.screenY - d.y) < 6) {
      ipc.toggleChat().catch(() => {});
    }
  };

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    // 重启后窗口尺寸被 window-state 插件还原：宽度明显小于球径说明上次停在长条形态
    Promise.all([win.outerSize(), win.scaleFactor()])
      .then(([s, sc]) => {
        if (!cancelled && s.width < BALL * sc * 0.8) switchMode("bar");
      })
      .catch(() => {});

    const evaluate = async (pos: PhysicalPosition, settling: boolean) => {
      const [mon, scale, size] = await Promise.all([
        currentMonitor(),
        win.scaleFactor(),
        win.outerSize(),
      ]);
      if (!mon) return;
      const mp = mon.position;
      const ms = mon.size;
      const rightEdge = mp.x + ms.width - size.width;
      const dl = pos.x - mp.x;
      const dr = rightEdge - pos.x;

      if (modeRef.current === "ball") {
        // 悬浮球只在停放判定后收编，拖动过程中经过边缘不变形
        if (!settling || Math.min(dl, dr) > DOCK_DIST * scale) return;
        switchMode("bar");
        const barW = Math.round(BAR_W * scale);
        const barH = Math.round(BAR_H * scale);
        const centerY = pos.y + size.height / 2;
        const y = Math.round(Math.min(Math.max(centerY - barH / 2, mp.y), mp.y + ms.height - barH));
        const x = dl <= dr ? mp.x : mp.x + ms.width - barW;
        await win.setSize(new PhysicalSize(barW, barH));
        await win.setPosition(new PhysicalPosition(x, y));
        return;
      }

      // 长条被拖离左右边缘 → 还原成悬浮球，球心对齐长条中心
      if (dl > UNDOCK_DIST * scale && dr > UNDOCK_DIST * scale) {
        switchMode("ball");
        const bw = Math.round(BALL * scale);
        const x = Math.round(pos.x + (size.width - bw) / 2);
        const y = Math.round(pos.y + (size.height - bw) / 2);
        await win.setSize(new PhysicalSize(bw, bw));
        await win.setPosition(new PhysicalPosition(x, y));
        return;
      }

      // 长条保持贴边（可沿边上下拖动），并保证不滑出屏幕上下沿
      const x = Math.round(dl <= dr ? mp.x : rightEdge);
      const y = Math.round(Math.min(Math.max(pos.y, mp.y), mp.y + ms.height - size.height));
      if (x !== pos.x || y !== pos.y) {
        await win.setPosition(new PhysicalPosition(x, y));
      }
    };

    // 串行处理移动事件；处理期间到达的事件只保留最后一个
    const handle = (pos: PhysicalPosition, settling: boolean) => {
      if (busy.current) {
        queued.current = { pos, settling };
        return;
      }
      busy.current = true;
      (async () => {
        try {
          let job: { pos: PhysicalPosition; settling: boolean } | null = { pos, settling };
          while (job) {
            queued.current = null;
            await evaluate(job.pos, job.settling).catch(() => {});
            job = queued.current;
          }
        } finally {
          busy.current = false;
        }
      })();
    };

    win
      .onMoved(({ payload: pos }) => {
        window.clearTimeout(settleTimer.current);
        // 长条形态需要实时响应（拖出还原、贴边跟随）；悬浮球形态拖动中不处理
        if (modeRef.current === "bar") handle(pos, false);
        settleTimer.current = window.setTimeout(() => {
          win
            .outerPosition()
            .then((p) => handle(p, true))
            .catch(() => {});
        }, SETTLE_MS);
      })
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      });

    return () => {
      cancelled = true;
      window.clearTimeout(settleTimer.current);
      unlisten?.();
    };
  }, []);

  if (mode === "bar") {
    return (
      <div className="w-full h-full" data-tauri-drag-region onMouseDown={onDown} onMouseUp={onUp}>
        <div
          data-tauri-drag-region
          className="w-full h-full rounded-full bg-gradient-to-b from-sky-400 to-indigo-600 shadow-lg flex flex-col items-center justify-center gap-1.5 cursor-pointer select-none"
        >
          <span data-tauri-drag-region className="w-1 h-1 rounded-full bg-white/80" />
          <span data-tauri-drag-region className="w-1 h-1 rounded-full bg-white/80" />
          <span data-tauri-drag-region className="w-1 h-1 rounded-full bg-white/80" />
        </div>
      </div>
    );
  }

  return (
    <div
      className="w-[76px] h-[76px] flex items-center justify-center"
      data-tauri-drag-region
      onMouseDown={onDown}
      onMouseUp={onUp}
    >
      <img
        data-tauri-drag-region
        src={appIcon}
        alt="拾光"
        draggable={false}
        className="w-14 h-14 rounded-full animate-float-pulse cursor-pointer select-none pointer-events-none shadow-lg"
      />
    </div>
  );
}
