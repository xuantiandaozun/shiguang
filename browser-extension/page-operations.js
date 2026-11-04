// Semantic operations shared by extension main/isolated worlds and desktop CDP.
// No framework-private state mutation; component support is feature/ARIA based.
(() => {
  const api = window.__dh;
  if (!api || api.operationsVersion === 2) return;
  const norm = value => String(value ?? '').replace(/\s+/g, ' ').trim();
  const text = el => norm(el?.innerText ?? el?.textContent);
  const visible = el => {
    if (!el?.isConnected || el.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
    const style = getComputedStyle(el);
    return style.display !== 'none' && style.visibility !== 'hidden' && el.getClientRects().length > 0;
  };
  const enabled = el => !el.matches(':disabled') && !el.closest('[inert], [aria-disabled="true"]');
  const label = el => norm(el.getAttribute('aria-label') ||
    (el.getAttribute('aria-labelledby') || '').split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ') ||
    Array.from(el.labels || []).map(text).join(' '));
  const role = el => el.getAttribute('role') || ({ BUTTON: 'button', TEXTAREA: 'textbox', SELECT: 'combobox', A: el.hasAttribute('href') ? 'link' : '' }[el.tagName]) ||
    (el.tagName === 'INPUT' ? ({ checkbox: 'checkbox', radio: 'radio', button: 'button', submit: 'button' }[el.type] || 'textbox') : '');
  const summary = el => ({ tag: el.tagName.toLowerCase(), role: role(el), name: (label(el) || text(el)).slice(0, 160), disabled: !enabled(el) });
  const scopeRoot = scope => {
    if (scope == null) return document;
    const roots = typeof scope === 'number' ? [api.ref(scope)].filter(Boolean) : Array.from(document.querySelectorAll(String(scope))).filter(visible);
    if (roots.length !== 1) throw new Error(`scope 必须唯一且有效，当前匹配 ${roots.length} 个`);
    return roots[0];
  };
  const remember = el => {
    const refs = window.__dhRefs || (window.__dhRefs = {});
    for (const key of Object.keys(refs)) if (refs[key] === el) return Number(key);
    const id = Math.max(0, ...Object.keys(refs).map(Number).filter(Number.isFinite)) + 1;
    refs[id] = el;
    (window.__dhRefMeta || (window.__dhRefMeta = {}))[id] = api.fingerprint ? api.fingerprint(el) : null;
    return id;
  };
  function locate(target = {}) {
    if (!target || typeof target !== 'object' || Array.isArray(target)) throw new Error('target 必须是定位对象');
    if (!['ref', 'css', 'role', 'name', 'label', 'placeholder'].some(key => target[key] != null && target[key] !== '')) throw new Error('缺少目标定位条件');
    const root = scopeRoot(target.scope);
    let matches = target.ref != null ? [api.ref(target.ref)].filter(Boolean) : Array.from(root.querySelectorAll(target.css || 'button,a,input,textarea,select,label,[role],[contenteditable="true"],[tabindex]'));
    if (matches.length > 5000) throw new Error('候选范围过大，请提供 scope 或更精确的 css');
    const exact = (a, b) => norm(a).toLocaleLowerCase() === norm(b).toLocaleLowerCase();
    matches = matches.filter(el => root.contains(el) && visible(el))
      .filter(el => target.css == null || el.matches(target.css))
      .filter(el => target.role == null || role(el) === target.role)
      .filter(el => target.name == null || exact(label(el) || text(el) || el.getAttribute('title'), target.name))
      .filter(el => target.label == null || exact(label(el), target.label))
      .filter(el => target.placeholder == null || exact(el.getAttribute('placeholder'), target.placeholder));
    return matches;
  }
  function unique(target) {
    const matches = locate(target);
    if (matches.length !== 1) return { error: matches.length ? '目标不唯一，未执行操作' : '未找到可见目标，未执行操作', code: matches.length ? 'ambiguous' : 'not_found', count: matches.length, candidates: matches.slice(0, 12).map(el => ({ ...summary(el), ref: remember(el) })) };
    return { el: matches[0] };
  }
  const guarded = fn => async args => { try { return await fn(args || {}); } catch (error) { return { error: String(error.message || error), code: 'invalid_request' }; } };
  const hitTest = el => {
    el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' });
    const r = el.getBoundingClientRect();
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return hit && (hit === el || el.contains(hit));
  };
  const checked = el => el.matches('input[type="checkbox"],input[type="radio"]') ? el.checked :
    el.getAttribute('aria-checked') === 'mixed' ? 'mixed' :
    el.hasAttribute('aria-checked') ? el.getAttribute('aria-checked') === 'true' : null;

  api.capabilities = () => ({
    version: 1,
    actions: ['locate', 'click', 'fill', 'select', 'set_checked'],
    conditions: ['visible', 'hidden', 'enabled', 'text', 'value'],
    extract: ['table', 'list', 'fields'],
    controls: {
      native: true, aria_combobox: !!document.querySelector('[role="combobox"]'),
      contenteditable: !!document.querySelector('[contenteditable]:not([contenteditable="false"])'),
      same_origin_frames: Array.from(document.querySelectorAll('iframe')).filter(f => { try { return !!f.contentDocument; } catch { return false; } }).length,
    },
    limits: ['定位默认精确匹配；歧义不操作', '自定义选择仅支持可见 ARIA 选项，不猜私有框架状态', 'iframe 与 Shadow DOM 暂不自动穿透', 'changed 不是业务成功；提交后需独立核验', '只提取当前已渲染内容，不自动滚动或翻页'],
  });

  api.action = guarded(async args => {
    if (!['locate', 'click', 'fill', 'select', 'set_checked'].includes(args.action)) throw new Error('未知 action');
    if (args.action === 'locate') {
      const matches = locate(args.target);
      return { count: matches.length, candidates: matches.slice(0, 12).map(el => ({ ...summary(el), ref: remember(el) })), truncated: matches.length > 12 };
    }
    const found = unique(args.target);
    if (!found.el) return found;
    const el = found.el;
    if (!enabled(el)) return { error: '目标已禁用，未操作', code: 'disabled', target: summary(el) };
    if (!hitTest(el)) return { error: '目标中心被遮挡，未操作', code: 'covered', target: summary(el) };
    const ref = remember(el);
    if (args.action === 'click') return api.click(ref);
    if (args.action === 'fill') {
      if (typeof args.text !== 'string') throw new Error('fill 需要 text');
      if (el.matches('input[type="password"]') || el.querySelector('input[type="password"]')) return { error: '不通过通用库写入密码，请使用专门的授权流程', code: 'sensitive' };
      if (el.readOnly || el.closest('[aria-readonly="true"]')) return { error: '目标只读，未输入', code: 'readonly' };
      return api.type(ref, args.text, args.clear !== false);
    }
    if (args.action === 'set_checked') {
      if (typeof args.checked !== 'boolean') throw new Error('set_checked 需要布尔 checked');
      const before = checked(el);
      if (before === null) return { error: '目标不支持 checked 状态', code: 'unsupported' };
      if (before === true && args.checked === false && el.matches('input[type="radio"], [role="radio"]')) return { error: '不能通过点击取消单选项，请选择其它选项', code: 'unsupported' };
      if (before !== args.checked) await api.click(ref);
      const after = el.isConnected ? checked(el) : null;
      return { ok: after === args.checked, verified: after === args.checked, before, after, target: summary(el), ...(after !== args.checked ? { error: '未能验证勾选状态，请重新观察，不要盲目重试' } : {}) };
    }
    if (typeof args.text !== 'string' || !norm(args.text)) throw new Error('select 需要精确选项 text');
    if (el.tagName === 'SELECT') {
      const options = Array.from(el.options).filter(o => o.value === args.text || norm(o.textContent) === norm(args.text));
      if (options.length !== 1) return { error: '选项不存在或不唯一', code: 'ambiguous', count: options.length };
      if (options[0].disabled || options[0].closest('optgroup[disabled]')) return { error: '选项已禁用', code: 'disabled' };
      const result = await api.type(ref, options[0].value, true);
      const verified = el.isConnected && el.selectedOptions[0] === options[0];
      return { ...result, verified, selected: norm(el.selectedOptions[0]?.textContent), ...(!verified ? { ok: false, error: '未能验证选择结果，请重新观察' } : {}) };
    }
    if (role(el) !== 'combobox' && !el.hasAttribute('aria-controls') && !el.hasAttribute('aria-haspopup')) return { error: '未识别为选择控件，请检查局部快照', code: 'unsupported' };
    if (el.getAttribute('aria-expanded') !== 'true') await api.click(ref);
    const ids = `${el.getAttribute('aria-controls') || ''} ${el.getAttribute('aria-owns') || ''}`.split(/\s+/).filter(Boolean);
    const roots = ids.map(id => document.getElementById(id)).filter(Boolean);
    // 只在唯一可见 listbox 内回退，绝不全页找同名文本后点击。
    if (!roots.length) roots.push(...Array.from(document.querySelectorAll('[role="listbox"]')).filter(visible));
    if (roots.length !== 1) return { error: '无法唯一关联选项浮层，控件可能已展开，请重新观察', code: 'ambiguous', opened_control: true };
    const options = Array.from(roots[0].querySelectorAll('[role="option"]')).filter(o => visible(o) && norm(label(o) || text(o)) === norm(args.text));
    if (options.length !== 1) return { error: '可见选项不存在或不唯一；如需搜索请先 fill，再等待选项出现', code: 'ambiguous', count: options.length, opened_control: true };
    const option = options[0];
    if (!enabled(option) || !hitTest(option)) return { error: '选项被禁用或遮挡，未选择', code: 'unavailable' };
    if (roots[0].getAttribute('aria-multiselectable') === 'true') return { error: '暂不自动操作多选列表，请观察每项状态后逐项操作', code: 'unsupported', opened_control: true };
    if (option.getAttribute('aria-selected') === 'true') return { ok: true, verified: true, selected: args.text, changed: false };
    const result = await api.click(remember(option));
    const selected = option.isConnected && option.getAttribute('aria-selected') === 'true';
    return { ...result, verified: !!selected, selected: args.text, next: selected ? undefined : '已尝试选择，但缺少确定的选中证据；用局部快照核验最终值，不要重复提交。' };
  });

  // Single synchronous observation. Rust owns polling/deadline/cancellation.
  api.probe = guarded(args => {
    if (!['visible', 'hidden', 'enabled', 'text', 'value'].includes(args.condition)) throw new Error('未知等待条件');
    const matches = locate(args.target);
    if (args.condition === 'hidden') return { matched: matches.length === 0, count: matches.length };
    if (matches.length > 1) return { error: '等待目标不唯一，请缩小范围', code: 'ambiguous', count: matches.length };
    if (!matches.length) return { matched: false, count: 0 };
    const el = matches[0];
    let observed;
    let matched;
    if (args.condition === 'visible') matched = true;
    if (args.condition === 'enabled') matched = enabled(el);
    if (args.condition === 'text') {
      if (typeof args.text !== 'string' || !args.text) throw new Error('text 条件需要非空 text');
      observed = text(el).slice(0, 2000); matched = observed.includes(args.text);
    }
    if (args.condition === 'value') {
      if (typeof args.text !== 'string') throw new Error('value 条件需要 text');
      if (el.matches('input[type="password"]')) throw new Error('不读取密码字段');
      observed = String(el.value ?? '').slice(0, 2000); matched = observed === args.text;
    }
    return { matched, count: 1, observed, target: summary(el) };
  });

  api.extract = guarded(args => {
    const root = scopeRoot(args.scope);
    const limit = Math.max(1, Math.min(Number(args.limit) || 20, 100));
    let remaining = 16000;
    let clipped = false;
    const clip = value => {
      const raw = norm(value);
      const cap = Math.min(500, remaining);
      const out = raw.slice(0, cap);
      remaining -= out.length;
      clipped ||= raw.length > cap;
      return out;
    };
    const boundedRows = (rows, render) => {
      const items = [];
      for (const row of rows.slice(0, limit)) {
        if (remaining <= 0) { clipped = true; break; }
        items.push(render(row));
      }
      return items;
    };
    if (args.kind === 'table') {
      const tables = root.matches?.('table,[role="table"],[role="grid"]') ? [root] : Array.from(root.querySelectorAll('table,[role="table"],[role="grid"]')).filter(visible);
      if (tables.length !== 1) return { error: '表格必须唯一，请使用 scope 指定', count: tables.length };
      const rows = Array.from(tables[0].querySelectorAll('tr,[role="row"]')).filter(visible);
      const output = boundedRows(rows, row => {
        const cells = Array.from(row.querySelectorAll('th,td,[role="cell"],[role="gridcell"],[role="columnheader"],[role="rowheader"]'));
        if (cells.length > 20) clipped = true;
        return cells.slice(0, 20).map(cell => clip(text(cell)));
      });
      return { rows: output, truncated: clipped || rows.length > limit, rendered_only: true };
    }
    if (!['list', 'fields'].includes(args.kind)) throw new Error('未知提取 kind');
    const fields = args.fields;
    if (!Array.isArray(fields) || !fields.length || fields.length > 12) throw new Error('fields 需要 1–12 个字段');
    for (const field of fields) {
      if (!field.name || !field.selector || (field.attribute && !['href', 'title', 'aria-label', 'datetime'].includes(field.attribute))) throw new Error('字段需 name/selector，attribute 仅支持 href/title/aria-label/datetime');
    }
    const rows = args.kind === 'fields' ? [root] : Array.from(root.querySelectorAll(args.row_selector || '')).filter(visible);
    const items = boundedRows(rows, row => Object.fromEntries(fields.map(field => {
      const el = field.selector === ':scope' ? row : row.querySelector(field.selector);
      const value = !el || !visible(el) ? null : field.attribute === 'href' ? (el.href || el.getAttribute('href')) : field.attribute ? el.getAttribute(field.attribute) : text(el);
      return [field.name, value == null ? null : clip(value)];
    })));
    return { items, truncated: clipped || rows.length > limit, rendered_only: true, note: '仅返回可见且已渲染数据；不保证完整分页、业务真实性或币种一致。' };
  });
  // page-runtime.js（扩展真实输入引擎的页面侧）复用同一套定位与判定。
  api.ops = { locate, unique, summary, remember, visible, enabled, role, label, checked, text, norm };
  api.operationsVersion = 2;
})();
