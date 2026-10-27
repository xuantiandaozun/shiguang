export function fileKind(name: string, directory = false, regular = true) {
  if (directory) return { label: "文件夹", color: "#e8a23a", mark: "" };
  if (!regular) return { label: "链接", color: "#a78bfa", mark: "↗" };
  const ext = name.split(".").pop()?.toLowerCase() || "";
  if (/^(png|jpg|jpeg|gif|svg|webp|bmp|ico|avif)$/.test(ext)) return { label: "图片", color: "#5fbf9b", mark: "▧" };
  if (/^(zip|gz|bz2|xz|tar|7z|rar|tgz|zst)$/.test(ext)) return { label: "压缩包", color: "#d5aa62", mark: "Z" };
  if (/^(mp4|webm|mov|avi|mkv|mp3|wav|ogg|flac)$/.test(ext)) return { label: "音视频", color: "#bd8cdd", mark: "▷" };
  if (/^(js|ts|tsx|jsx|py|rs|go|java|c|h|cpp|sh|bash|sql|html|css)$/.test(ext)) return { label: "代码或脚本", color: "#73a9d7", mark: "‹›" };
  if (name.startsWith(".env") || /^(json|yaml|yml|toml|ini|conf|config|xml|env)$/.test(ext)) return { label: "配置文件", color: "#b8a3d9", mark: "=" };
  if (ext === "log") return { label: "日志", color: "#94a3b8", mark: "≡" };
  if (/^(pdf|doc|docx|txt|md|csv|xls|xlsx)$/.test(ext)) return { label: "文档", color: "#8ab4cf", mark: "≡" };
  return { label: "文件", color: "#94a3b8", mark: "" };
}
export default function FileTypeIcon({ name, directory = false, regular = true }: { name: string; directory?: boolean; regular?: boolean }) {
  const kind = fileKind(name, directory, regular);
  return <svg role="img" aria-label={kind.label} viewBox="0 0 24 24" width="18" height="18" className="inline-block shrink-0 align-middle" style={{ color: kind.color }}>
    <title>{kind.label}</title>
    {directory ? <path d="M3 6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v10H3z" fill="currentColor" fillOpacity=".18" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" /> : <>
      <path d="M5 3h9l5 5v13H5zM14 3v6h5" fill="currentColor" fillOpacity=".1" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
      <text x="12" y="17" textAnchor="middle" fill="currentColor" fontSize="10" fontFamily="sans-serif" fontWeight="600">{kind.mark}</text>
    </>}
  </svg>;
}
