export function terminalCompletionStatus(status: string) {
  const labels: Record<string, string> = {
    checking: "正在检测 Tab 增强补全…",
    initial: "Tab 增强已启用（Bash 5+）",
    readline: "Tab 增强已启用（Bash 4 兼容）",
    custom: "使用原生 Tab：保留了已有的自定义补全绑定",
    unsupported: "使用原生 Tab：当前 Shell 不支持自动添加 ./",
    probe_failed: "使用原生 Tab：Shell 检测失败，增强未启用",
    timeout: "使用原生 Tab：增强初始化超时，请重连后检查",
    error: "使用原生 Tab：增强注册失败",
  };
  return { label: labels[status] || "使用原生 Tab：未收到增强状态", enhanced: status === "initial" || status === "readline" };
}

export interface TerminalKeyEvent {
  type: string;
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
}

// Use event.key rather than legacy keyCode. In an embedded WebView a Tab must
// remain terminal input, not move browser focus to the file panel or toolbar.
// Do not interfere with an IME choosing/composing text or OS-level shortcuts.
export function terminalTabSequence(event: TerminalKeyEvent): string | undefined {
  if (event.type !== "keydown" || event.key !== "Tab" || event.isComposing || event.ctrlKey || event.altKey || event.metaKey) return undefined;
  return event.shiftKey ? "\u001b[Z" : "\t";
}

// IPC calls are asynchronous. Preserve keystroke order so the server receives
// the typed prefix before Tab, even when a preceding write takes longer.
export function createTerminalInputQueue(send: (data: string) => Promise<unknown>) {
  let tail: Promise<unknown> = Promise.resolve();
  return (data: string) => {
    const request = tail.catch(() => {}).then(() => send(data));
    tail = request;
    return request;
  };
}
