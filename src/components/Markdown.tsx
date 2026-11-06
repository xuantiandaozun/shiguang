import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc } from "../lib/ipc";
import "./Markdown.css";

/** 本地绝对路径：C:\… / C:/… / \\共享 / file:///… */
const isLocalPath = (src: string) => /^([a-zA-Z]:[\\/]|\\\\|file:\/\/\/)/.test(src);

/** file:/// 前缀去掉并还原 %XX，给 convertFileSrc / openExternal 用 */
const toFsPath = (src: string) =>
  src.startsWith("file:///") ? decodeURIComponent(src.slice(8)) : src;

const openOutside = (target: string) => {
  ipc.openExternal(target).catch(() => {});
};

/** 行内代码是否像文件路径：绝对路径、带目录和扩展名的相对路径、常见交付文件名 */
const looksLikePath = (text: string) => {
  const t = text.trim();
  if (!t || t.length > 400 || t.includes("\n")) return false;
  if (/^([a-zA-Z]:[\\/]|\\\\|file:\/\/\/|~[\\/]|%[A-Za-z_]+%[\\/])/.test(t)) return true;
  if (/^[^\s<>|*?"]+[\\/][^\s<>|*?"]*\.[A-Za-z0-9]{1,8}$/.test(t)) return !/^[a-z]+:\/\//i.test(t);
  return /^[^\s\\/<>|*?"]+\.(png|jpe?g|gif|webp|bmp|svg|mp4|webm|mov|mkv|mp3|wav|m4a|pdf|docx?|xlsx?|pptx?|csv|zip|7z|rar|srt|html?|md|txt|json)$/i.test(t);
};

/** 点击文件名：在资源管理器中打开所在目录并选中它（不直接打开文件） */
function PathChip({ path, children, code }: { path: string; children?: React.ReactNode; code?: boolean }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <span
      role="button"
      tabIndex={0}
      title={error ?? `${path}\n点击在资源管理器中显示`}
      className={`chat-path ${code ? "chat-path-code" : ""} ${error ? "chat-path-missing" : ""}`}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        ipc.revealInFolder(path).then(() => setError(null)).catch((err) => setError(String(err)));
      }}
    >
      {children}
    </span>
  );
}

/** 正文里的 Windows 绝对路径（如 C:\Users\me\a.png）转成可点击节点。中文标点、空白视为结束。 */
const ABS_PATH_RE = /(?:[a-zA-Z]:[\\/]|\\\\)[^\s<>|*?"'`，。；：！？、（）【】「」《》]+/g;

type MdNode = { type: string; value?: string; url?: string; children?: MdNode[]; data?: Record<string, unknown> };

function remarkLocalPaths() {
  const walk = (node: MdNode) => {
    if (!node.children || node.type === "link" || node.type === "linkReference") return;
    const next: MdNode[] = [];
    for (const child of node.children) {
      if (child.type !== "text" || !child.value) {
        walk(child);
        next.push(child);
        continue;
      }
      const text = child.value;
      let last = 0;
      for (const m of text.matchAll(ABS_PATH_RE)) {
        const raw = m[0].replace(/[.,;:)\]]+$/, "");
        const start = m.index ?? 0;
        if (raw.length < 4) continue;
        if (start > last) next.push({ type: "text", value: text.slice(last, start) });
        next.push({
          type: "link",
          url: raw,
          data: { hProperties: { "data-local-path": "1" } },
          children: [{ type: "text", value: raw }],
        });
        last = start + raw.length;
      }
      if (last === 0) next.push(child);
      else if (last < text.length) next.push({ type: "text", value: text.slice(last) });
    }
    node.children = next;
  };
  return (tree: MdNode) => walk(tree);
}

function ExternalLink({ href, children }: { href?: string; children?: React.ReactNode }) {
  if (!href) return <span>{children}</span>;
  // 本地文件链接：在资源管理器中定位，而不是直接打开
  if (isLocalPath(href)) return <PathChip path={toFsPath(href)}>{children}</PathChip>;
  const target = href;
  return (
    <a
      href={href}
      title={`${target}（点击在浏览器中打开）`}
      className="chat-markdown-link"
      onClick={(e) => {
        // 阻止 WebView 内部跳转，一律交给系统默认浏览器
        e.preventDefault();
        e.stopPropagation();
        openOutside(target);
      }}
    >
      {children}
    </a>
  );
}

function ChatImage({ src, alt }: { src?: string; alt?: string }) {
  const [failed, setFailed] = useState(false);
  if (!src || failed) {
    return <span className="text-slate-500 text-xs break-all">[图片无法显示：{alt || src || "未知"}]</span>;
  }
  // 网络图片/data 直接渲染；本地路径转成 asset 协议 URL 才能在 WebView 里加载
  const local = isLocalPath(src);
  const fsPath = local ? toFsPath(src) : src;
  const url = local ? convertFileSrc(fsPath) : src;
  return (
    <img
      src={url}
      alt={alt ?? ""}
      loading="lazy"
      title={`${fsPath}（点击查看原图）`}
      onError={() => setFailed(true)}
      onClick={() => openOutside(fsPath)}
      className="my-1.5 max-w-full max-h-64 rounded-lg border border-slate-700/70 object-contain cursor-zoom-in hover:border-sky-500/50 transition"
    />
  );
}

export default function Markdown({ content }: { content: string }) {
  return (
    <div className="chat-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkLocalPaths]}
        // 默认消毒会清掉 file:/// 与 Windows 路径；点击行为已被拦截并走后端白名单，安全可控
        urlTransform={(url) => url}
        components={{
          a: ({ href, children }) => <ExternalLink href={href}>{children}</ExternalLink>,
          img: ({ src, alt }) => <ChatImage src={typeof src === "string" ? src : undefined} alt={alt} />,
          code: ({ className, children }) => {
            const text = typeof children === "string" ? children : Array.isArray(children) && children.every((c) => typeof c === "string") ? children.join("") : null;
            if (!className && text && looksLikePath(text)) {
              return <PathChip path={text.trim()} code><code>{children}</code></PathChip>;
            }
            return <code className={className}>{children}</code>;
          },
          table: ({ children }) => <div className="chat-markdown-table"><table>{children}</table></div>,
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
