import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { currentMonitor, primaryMonitor, getCurrentWindow, PhysicalPosition, PhysicalSize } from "@tauri-apps/api/window";
import { ipc } from "../lib/ipc";
import appIcon from "../assets/brand-mark.svg";

// Keep the native/WebView surface fixed: Windows can clamp small window widths.
// Only the visible child changes shape. All dimensions below are logical pixels.
const FRAME_W = 160;
const FRAME_H = 76;
const LOGO_SIZE = 64;
const CAPSULE_W = 28;
const DOCK_DIST = 64;
const UNDOCK_DIST = 48;
const SETTLE_MS = 260;
type Mode = "ball" | "bar";
type Side = "left" | "right";

export default function FloatBall() {
  const [mode, setMode] = useState<Mode>("ball");
  const [side, setSide] = useState<Side>("right");
  const modeRef = useRef<Mode>("ball");
  const downPos = useRef<{ x: number; y: number } | null>(null);

  const onDown = (e: ReactMouseEvent) => { downPos.current = { x: e.screenX, y: e.screenY }; };
  const onUp = (e: ReactMouseEvent) => {
    const down = downPos.current;
    downPos.current = null;
    if (down && Math.hypot(e.screenX - down.x, e.screenY - down.y) < 6) void ipc.toggleChat().catch(console.error);
  };

  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    let ready = false;
    let moving = false;
    let pending: boolean | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let unlisten: (() => void) | undefined;
    let polling = false;
    let observed: { x: number; y: number; changedAt: number } | undefined;

    const changeMode = (next: Mode) => { modeRef.current = next; setMode(next); };
    const evaluate = async (settled: boolean) => {
      const [pos, size, scale, monitor] = await Promise.all([
        win.outerPosition(), win.outerSize(), win.scaleFactor(), currentMonitor(),
      ]);
      if (cancelled || !monitor) return;
      const left = monitor.position.x;
      const right = left + monitor.size.width - size.width;
      const dl = pos.x - left;
      const dr = right - pos.x;
      if (modeRef.current === "ball") {
        // The full ball is centered inside a wider transparent native surface.
        // Measure its visible edges, allowing native windows partly off-screen.
        const inset = (size.width - LOGO_SIZE * scale) / 2;
        const visibleLeft = dl + inset;
        const visibleRight = dr + inset;
        if (!settled || Math.min(visibleLeft, visibleRight) > DOCK_DIST * scale) return;
        const dockSide: Side = visibleLeft <= visibleRight ? "left" : "right";
        // Fixed-size native surface and CSS viewport now always agree.
        await win.setPosition(new PhysicalPosition(dockSide === "left" ? left : right, pos.y));
        if (cancelled) return;
        setSide(dockSide);
        changeMode("bar");
      } else if (dl > UNDOCK_DIST * scale && dr > UNDOCK_DIST * scale) {
        changeMode("ball");
      }
      // Do not snap a moving capsule back on every event: that fights native dragging.
      // Vertical-only dragging keeps the side; a settled capsule snaps flush again.
      else if (settled) {
        const dockSide: Side = dl <= dr ? "left" : "right";
        setSide(dockSide);
        const y = Math.round(Math.min(Math.max(pos.y, monitor.position.y), monitor.position.y + monitor.size.height - size.height));
        const x = Math.round(dockSide === "left" ? left : right);
        if (pos.x !== x || pos.y !== y) await win.setPosition(new PhysicalPosition(x, y));
      }
    };
    const schedule = async (settled: boolean) => {
      if (!ready || cancelled) return;
      if (moving) { pending = settled; return; }
      moving = true;
      try {
        let next: boolean | null = settled;
        while (next !== null && !cancelled) {
          pending = null;
          await evaluate(next);
          next = pending;
        }
      } catch (error) { console.error("悬浮球定位失败", error); }
      finally { moving = false; }
    };
    // Some Windows/WebView combinations coalesce native drag move events.
    // Check real coordinates as a fallback, only docking once motion has stopped.
    const poll = setInterval(() => {
      if (!ready || cancelled || polling) return;
      polling = true;
      void win.outerPosition().then((pos) => {
        if (cancelled) return;
        const now = performance.now();
        if (!observed || observed.x !== pos.x || observed.y !== pos.y) {
          observed = { x: pos.x, y: pos.y, changedAt: now };
          if (modeRef.current === "bar") void schedule(false);
        } else if (now - observed.changedAt >= SETTLE_MS) {
          void schedule(true);
        }
      }).catch(console.error).finally(() => { polling = false; });
    }, 150);
    void (async () => {
      const stop = await win.onMoved(() => {
        if (!ready || cancelled) return;
        clearTimeout(timer);
        if (modeRef.current === "bar") void schedule(false);
        timer = setTimeout(() => void schedule(true), SETTLE_MS);
      });
      if (cancelled) { stop(); return; }
      unlisten = stop;
      const scale = await win.scaleFactor();
      await win.setSize(new PhysicalSize(Math.round(FRAME_W * scale), Math.round(FRAME_H * scale)));
      const [actual, monitor] = await Promise.all([win.outerSize(), primaryMonitor()]);
      if (cancelled) return;
      if (monitor) {
        const margin = Math.round(64 * scale);
        const minX = monitor.position.x + margin;
        const minY = monitor.position.y + margin;
        const maxX = Math.max(minX, monitor.position.x + monitor.size.width - actual.width - margin);
        const maxY = Math.max(minY, monitor.position.y + monitor.size.height - actual.height - margin);
        await win.setPosition(new PhysicalPosition(
          Math.round(Math.min(Math.max(monitor.position.x + monitor.size.width * 0.7 - actual.width / 2, minX), maxX)),
          Math.round(Math.min(Math.max(monitor.position.y + monitor.size.height * 0.4, minY), maxY)),
        ));
      }
      if (cancelled) return;
      changeMode("ball");
      await win.unminimize();
      await win.show();
      ready = true;
    })().catch(console.error);
    return () => { cancelled = true; ready = false; clearTimeout(timer); clearInterval(poll); unlisten?.(); };
  }, []);

  return <div className="relative w-full h-full" onMouseDown={onDown} onMouseUp={onUp}>
    {mode === "bar" ? <div data-tauri-drag-region className="absolute overflow-hidden cursor-pointer select-none" style={{
      top: 6, width: CAPSULE_W, height: LOGO_SIZE,
      [side]: 0, borderRadius: side === "left" ? "0 32px 32px 0" : "32px 0 0 32px", background: "#e8a23a",
    }}>
      <img src={appIcon} alt="打开拾光" draggable={false} style={{ position: "absolute", top: 0, width: LOGO_SIZE, height: LOGO_SIZE, maxWidth: "none", pointerEvents: "none", [side]: CAPSULE_W - LOGO_SIZE }} />
    </div> : <div data-tauri-drag-region className="absolute cursor-pointer select-none" style={{ top: 6, left: "50%", transform: "translateX(-50%)", width: LOGO_SIZE, height: LOGO_SIZE }}>
      <img src={appIcon} alt="拾光" draggable={false} className="rounded-full shadow-lg" style={{ width: LOGO_SIZE, height: LOGO_SIZE, pointerEvents: "none" }} />
    </div>}
  </div>;
}
