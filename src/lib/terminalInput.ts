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
