// 便签存在插件私有存储里（sg.store），上限由设置 max_items 控制。
function load(sg) {
  return sg.store.get("notes") || [];
}

module.exports.add = (args, sg) => {
  const text = String(args.text || "").trim();
  if (!text) throw new Error("便签内容不能为空");
  const max = Number(sg.settings().max_items) || 100;
  const notes = load(sg);
  const note = { id: String(Date.now()), text, at: new Date().toISOString().slice(0, 19).replace("T", " ") };
  notes.unshift(note);
  sg.store.set("notes", notes.slice(0, max));
  return { saved: true, note, count: Math.min(notes.length, max) };
};

module.exports.list = (_args, sg) => {
  const notes = load(sg);
  return { count: notes.length, items: notes };
};

module.exports.remove = (args, sg) => {
  const notes = load(sg);
  const rest = notes.filter((n) => n.id !== String(args.id));
  if (rest.length === notes.length) throw new Error("没有这条便签：" + args.id);
  sg.store.set("notes", rest);
  return { deleted: true, count: rest.length };
};
