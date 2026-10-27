import React from "react";
import ReactDOM from "react-dom/client";
import FloatBall from "./windows/FloatBall";
import ChatPanel from "./windows/ChatPanel";
import MainWindow from "./windows/MainWindow";
import Reminder from "./windows/Reminder";
import DelegationWindow from "./windows/DelegationWindow";
import "./index.css";
import "./theme.css";
import "./buttons.css";

const ServerTerminal = React.lazy(() => import("./windows/ServerTerminal"));

const route = window.location.hash.replace(/^#\/?/, "").split("?")[0] || "floatball";

const views: Record<string, React.ReactNode> = {
  floatball: <FloatBall />,
  chat: <ChatPanel />,
  main: <MainWindow />,
  terminal: <React.Suspense fallback={<div className="h-screen bg-slate-950 p-4 text-slate-300">正在打开终端…</div>}><ServerTerminal /></React.Suspense>,
  reminder: <Reminder />,
  delegation: <DelegationWindow />,
};

document.body.dataset.window = route;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{views[route] ?? <FloatBall />}</React.StrictMode>
);
