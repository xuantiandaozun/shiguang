import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc } from "../lib/ipc";

/** 本地绝对路径：C:\… / C:/… / \\共享 / file:///… */
const isLocalPath = (src: string) => /^([a-zA-Z]:[\\/]|\\\\|file:\/\/\/)/.test(src);

/** file:/// 前缀去掉并还原 %XX，给 convertFileSrc / openExternal 用 */
const toFsPath = (src: string) =>
  src.startsWith("file:///") ? decodeURIComponent(src.slice(8)) : src;

const openOutside = (target: string) => {
  ipc.openExternal(target).catch(() => {});
};

function ExternalLink({ href, children }: { href?: string; children?: React.ReactNode }) {
  if (!href) return <span>{children}</span>;
  const target = isLocalPath(href) ? toFsPath(href) : href;
  return (
    <a
      href={href}
      title={`${target}（点击在浏览器中打开）`}
      className="text-sky-400 hover:text-sky-300 hover:underline cursor-pointer break-all"
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
    <div
      className="prose prose-invert prose-sm max-w-none break-words
        prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ol:my-1.5 prose-li:my-0.5
        prose-pre:bg-slate-950 prose-pre:border prose-pre:border-slate-700 prose-pre:my-2
        prose-code:text-sky-300 prose-code:before:content-none prose-code:after:content-none
        prose-a:text-sky-400 prose-table:text-xs prose-th:px-2 prose-td:px-2"
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        // 默认消毒会清掉 file:/// 与 Windows 路径；点击行为已被拦截并走后端白名单，安全可控
        urlTransform={(url) => url}
        components={{
          a: ({ href, children }) => <ExternalLink href={href}>{children}</ExternalLink>,
          img: ({ src, alt }) => <ChatImage src={typeof src === "string" ? src : undefined} alt={alt} />,
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
