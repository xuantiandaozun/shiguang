// 按 JSON Schema 渲染简单表单：string（含 enum、format=textarea/password）、number/integer、boolean。
import type { JsonSchema } from "../lib/ipc";

const input = "w-full rounded-lg border border-slate-700 bg-slate-900 px-2.5 py-1.5 text-sm";

export function SchemaFields({
  schema,
  value,
  onChange,
}: {
  schema: JsonSchema;
  value: Record<string, unknown>;
  onChange: (next: Record<string, unknown>) => void;
}) {
  const props = schema.properties ?? {};
  const set = (key: string, v: unknown) => onChange({ ...value, [key]: v });
  return (
    <div className="space-y-3">
      {Object.entries(props).map(([key, prop]) => {
        const label = prop.title ?? key;
        const current = value[key] ?? prop.default;
        let field: JSX.Element;
        if (prop.type === "boolean") {
          field = (
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={Boolean(current)} onChange={(e) => set(key, e.target.checked)} />
              {label}
            </label>
          );
          return (
            <div key={key}>
              {field}
              {prop.description && <div className="mt-1 text-xs text-slate-500">{prop.description}</div>}
            </div>
          );
        }
        if (Array.isArray(prop.enum)) {
          field = (
            <select className={input} value={String(current ?? "")} onChange={(e) => set(key, e.target.value)}>
              {prop.enum.map((opt) => (
                <option key={String(opt)} value={String(opt)}>
                  {String(opt)}
                </option>
              ))}
            </select>
          );
        } else if (prop.type === "number" || prop.type === "integer") {
          field = (
            <input
              type="number"
              className={input}
              value={current === undefined || current === null ? "" : String(current)}
              onChange={(e) => set(key, e.target.value === "" ? null : Number(e.target.value))}
            />
          );
        } else if (prop.format === "textarea") {
          field = <textarea rows={4} className={input} value={String(current ?? "")} onChange={(e) => set(key, e.target.value)} />;
        } else {
          field = (
            <input
              type={prop.format === "password" ? "password" : "text"}
              className={input}
              value={String(current ?? "")}
              onChange={(e) => set(key, e.target.value)}
            />
          );
        }
        return (
          <label key={key} className="block">
            <div className="mb-1 text-xs text-slate-400">{label}</div>
            {field}
            {prop.description && <div className="mt-1 text-xs text-slate-500">{prop.description}</div>}
          </label>
        );
      })}
    </div>
  );
}
