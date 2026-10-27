import type { CSSProperties } from "react";

const paths = {
  plus: "M12 5v14M5 12h14",
  history: "M3 11a9 9 0 1 1 2.6 7M3 4v7h7M12 7v5l3 2",
  close: "m6 6 12 12M18 6 6 18",
  maximize: "M8 3H3v5M16 3h5v5M3 16v5h5M21 16v5h-5",
  files: "M3 7V4h6l2 3h10v13H3z",
  ai: "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5z",
  clear: "m4 14 9-10 8 7-9 10H9zM12 21h9M8 10l8 7",
  plug: "M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0zM12 17v4",
  refresh: "M20 7a9 9 0 1 0 1 8M20 3v5h-5",
  upload: "M12 16V3m-5 5 5-5 5 5M4 16v5h16v-5",
  up: "M12 21V3m-7 7 7-7 7 7",
  send: "m3 3 18 9-18 9 3-9zM6 12h15",
  stop: "M5 5h14v14H5z",
  tab: "M4 5h16v14H4zM7 12h9m-3-3 3 3-3 3",
  todo: "M9 5h12M9 12h12M9 19h12M3 5h1M3 12h1M3 19h1",
  server: "M3 3h18v7H3zM3 14h18v7H3zM7 6.5h.01M7 17.5h.01",
  skill: "M12 3 3 8l9 5 9-5zM3 12l9 5 9-5M3 16l9 5 9-5",
  usage: "M4 20V10M12 20V4M20 20v-7",
  settings: "M4 7h16M4 17h16M8 4v6M16 14v6",
} as const;
export type IconName = keyof typeof paths;
export default function Icon({ name, className = "", style }: { name: IconName; className?: string; style?: CSSProperties }) {
  return <svg aria-hidden="true" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" className={`inline-block shrink-0 ${className}`} style={style}><path d={paths[name]} /></svg>;
}
