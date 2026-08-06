import React from "react";
import ReactDOM from "react-dom/client";
import FloatBall from "./windows/FloatBall";
import ChatPanel from "./windows/ChatPanel";
import MainWindow from "./windows/MainWindow";
import Reminder from "./windows/Reminder";
import "./index.css";

const route = window.location.hash.replace(/^#\/?/, "") || "floatball";

const views: Record<string, React.ReactNode> = {
  floatball: <FloatBall />,
  chat: <ChatPanel />,
  main: <MainWindow />,
  reminder: <Reminder />,
};

document.body.dataset.window = route;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{views[route] ?? <FloatBall />}</React.StrictMode>
);
