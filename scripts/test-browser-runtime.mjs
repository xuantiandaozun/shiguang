// 真实输入引擎单元测试：页面侧（jsdom 模拟布局）+ 后台引擎（假 CDP）。
// 真实浏览器里的端到端验证见 test-browser-runtime-e2e.mjs。
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import { createContext, runInContext } from 'node:vm';

const read = (file) => readFileSync(new URL(`../browser-extension/${file}`, import.meta.url), 'utf8');
const PAGE = ['page-api.js', 'page-operations.js', 'page-runtime.js'].map(read);

function page(html) {
  const dom = new JSDOM(html, { url: 'https://fixture.invalid/', runScripts: 'outside-only', pretendToBeVisual: true });
  const w = dom.window;
  w.HTMLElement.prototype.getClientRects = function () {
    for (let p = this; p; p = p.parentElement) if (p.hidden || w.getComputedStyle(p).display === 'none') return [];
    return [{ x: 0, y: 0, width: 100, height: 30 }];
  };
  w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 10, y: 10, left: 10, top: 10, right: 110, bottom: 40, width: 100, height: 30 });
  w.HTMLElement.prototype.scrollIntoView = function () { w.__hit = this; };
  w.document.elementFromPoint = () => w.__hit;
  // jsdom 没有实现 isContentEditable。
  Object.defineProperty(w.HTMLElement.prototype, 'isContentEditable', {
    get() { return !!this.closest('[contenteditable]:not([contenteditable="false"])'); }, configurable: true,
  });
  const t = w.setTimeout.bind(w);
  w.setTimeout = (fn, ms, ...a) => t(fn, Math.min(ms, 2), ...a);
  for (const src of PAGE) w.eval(src);
  return { w, d: w.document, api: w.__dh, rt: w.__dh.rt, close: () => w.close() };
}

function loadRuntime() {
  const ctx = createContext({ setTimeout, clearTimeout, console, URL });
  runInContext(read('runtime.js'), ctx);
  return ctx.DhRuntime;
}

test('ref 在元素被重渲染替换后按指纹唯一重定位，重复时拒绝猜测', () => {
  const p = page('<div id="app"><button name="save">保存</button><button>取消</button></div>');
  try {
    const found = p.api.find('保存', 5);
    const ref = found.candidates[0].ref;
    const app = p.d.getElementById('app');
    app.innerHTML = '<button name="save">保存</button><button>取消</button>';
    assert.equal(p.api.ref(ref), app.querySelector('button[name="save"]'));
    app.innerHTML = '<button name="save">保存</button><button name="save">保存</button>';
    p.api.find('取消', 5);
    p.api.find('保存', 5);
    app.innerHTML = '<button name="save">保存</button><button name="save">保存</button>';
    assert.equal(p.api.ref(1), null);
  } finally { p.close(); }
});

test('prepare：遮挡不点、禁用不点、容器解析到内部输入框', async () => {
  const p = page('<button id="b">提交</button><button id="off" disabled>禁用</button><div id="wrap" role="group" aria-label="标题"><input id="inner"></div><div id="mask">遮罩</div>');
  try {
    const ok = await p.rt.prepare({ target: { css: '#b' } });
    assert.equal(ok.ok, true);
    assert.ok(Number.isFinite(ok.x) && Number.isFinite(ok.y));
    assert.equal((await p.rt.prepare({ target: { css: '#off' } })).code, 'disabled');
    const typed = await p.rt.prepare({ target: { css: '#wrap' }, intent: 'type' });
    assert.equal(typed.kind, 'input');
    p.d.elementFromPoint = () => p.d.getElementById('mask');
    const covered = await p.rt.prepare({ target: { css: '#b' } });
    assert.equal(covered.code, 'covered');
    assert.match(covered.covered_by, /遮罩/);
  } finally { p.close(); }
});

test('读回校验：忽略占位文字和空白归一化，发现缺行和旧内容残留', () => {
  const p = page('<div id="ed" contenteditable="true" data-placeholder="请输入正文"></div><textarea id="ta"></textarea>');
  try {
    const ed = p.d.getElementById('ed');
    ed.focus();
    p.rt.focusState();
    ed.innerText = '请输入正文 第一段\n\n第二段';
    // jsdom 没有 innerText 布局，用 textContent 模拟读回。
    Object.defineProperty(ed, 'innerText', { get: () => ed.textContent, set: (v) => { ed.textContent = v; }, configurable: true });
    ed.textContent = '请输入正文 第一段  第二段';
    assert.equal(p.rt.verifyText({ text: '第一段\n第二段' }).verified, true);
    const missing = p.rt.verifyText({ text: '第一段\n第三段' });
    assert.equal(missing.verified, false);
    assert.equal(missing.missing, '第三段');
    const ta = p.d.getElementById('ta');
    ta.focus();
    p.rt.focusState();
    ta.value = '很长很长的旧内容还没有被清除掉，后面才是新内容 新内容';
    assert.equal(p.rt.verifyText({ text: '新内容', clear: true }).verified, false);
    assert.equal(p.rt.verifyText({ text: '新内容', clear: false }).verified, true);
  } finally { p.close(); }
});

test('observe 采集弹窗、提示与错误，expect 判断文字和 URL', () => {
  const p = page('<div role="dialog" aria-label="发布确认">确定发布？</div><div role="alert">保存成功</div><p class="form-error">标题不能为空</p>');
  try {
    const o = p.rt.observe();
    assert.deepEqual(plain(o.dialogs), ['发布确认']);
    assert.ok(o.messages.includes('保存成功'));
    assert.ok(o.errors.includes('标题不能为空'));
    assert.equal(p.rt.expect({ url_contains: 'fixture' }).met, true);
    assert.equal(p.rt.expect({ text: '不存在的文字', timeout_ms: 0 }).met, false);
  } finally { p.close(); }
});

// ---- 后台引擎（假 CDP） ----
function fakeIo(overrides = {}) {
  const sent = [];
  let mut = 0;
  const pageCalls = [];
  const handlers = {
    observe: () => ({ url: 'https://a.test/', title: 't', dialogs: [], messages: [], errors: [], focus: 'body', mut }),
    quiet: () => ({ age: 1000, ready: 'complete' }),
    prepare: () => ({ ok: true, x: 10, y: 20, target: { tag: 'button', name: '提交' }, ref: 1, kind: 'other', focused: false }),
    focusState: () => ({ kind: 'contenteditable', editable: true, focus: '<div>', readonly: false }),
    selectAll: () => ({ ok: true }),
    verifyText: ({ text }) => ({ verified: true, preview: text }),
    targetState: () => ({ checked: false }),
    expect: () => ({ met: true }),
    ...overrides,
  };
  const io = {
    sent, pageCalls,
    bump: () => { mut += 1; },
    send: async (method, params) => { sent.push([method, params]); if (io.onSend) io.onSend(method, params); return {}; },
    page: async (method, arg) => { pageCalls.push(method); return handlers[method](arg); },
  };
  return io;
}
const inputs = (io) => io.sent.filter(([m]) => m.startsWith('Input.'));
// 引擎在独立 vm 上下文里运行，比较前先序列化，避免跨上下文对象的原型不同。
const plain = (v) => JSON.parse(JSON.stringify(v));

test('click 发出真实鼠标序列，并汇报页面变化', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo();
  io.onSend = (m, p) => { if (m === 'Input.dispatchMouseEvent' && p.type === 'mouseReleased') io.bump(); };
  const rt = new TabRuntime(io);
  const r = await rt.act({ action: 'click', target: { ref: 1 } });
  assert.deepEqual(inputs(io).map(([, p]) => p.type), ['mouseMoved', 'mousePressed', 'mouseReleased']);
  assert.equal(inputs(io)[1][1].x, 10);
  assert.equal(r.ok, true);
  assert.equal(r.changed, true);
  assert.equal(r.effect.settled, true);
});

test('定位失败时不发任何输入、不等待', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo({ prepare: () => ({ error: '目标不唯一', code: 'ambiguous', count: 2 }) });
  const r = await new TabRuntime(io).act({ action: 'click', target: { name: '提交' } });
  assert.equal(r.code, 'ambiguous');
  assert.equal(inputs(io).length, 0);
  assert.ok(!io.pageCalls.includes('quiet'));
});

test('输入：富文本默认 Shift+Enter 换行，enter 模式按回车，textarea 插入换行，单行框换成空格', async () => {
  const { TabRuntime } = loadRuntime();
  const keys = (io) => inputs(io).filter(([m, p]) => m === 'Input.dispatchKeyEvent' && p.type !== 'keyUp').map(([, p]) => `${p.modifiers}:${p.key}`);
  const texts = (io) => inputs(io).filter(([m]) => m === 'Input.insertText').map(([, p]) => p.text);

  let io = fakeIo();
  await new TabRuntime(io).act({ action: 'fill', target: { ref: 1 }, text: '第一行\n第二行' });
  assert.deepEqual(texts(io), ['第一行', '第二行']);
  assert.deepEqual(keys(io), ['8:Enter']);

  io = fakeIo();
  await new TabRuntime(io).act({ action: 'type', target: { ref: 1 }, text: 'a\nb', newline: 'enter' });
  assert.deepEqual(keys(io), ['0:Enter']);

  io = fakeIo({ focusState: () => ({ kind: 'textarea', editable: true, focus: '<textarea>' }) });
  await new TabRuntime(io).act({ action: 'type', target: { ref: 1 }, text: 'a\nb' });
  assert.deepEqual(texts(io), ['a', '\n', 'b']);

  io = fakeIo({ focusState: () => ({ kind: 'input', editable: true, focus: '<input>' }) });
  const r = await new TabRuntime(io).act({ action: 'type', target: { ref: 1 }, text: 'a\nb' });
  assert.deepEqual(texts(io), ['a b']);
  assert.match(r.note, /单行/);
});

test('输入后没有获得可编辑焦点时报错，不盲目敲键盘', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo({ focusState: () => ({ kind: 'other', editable: false, focus: '<div> 卡片' }) });
  const r = await new TabRuntime(io).act({ action: 'fill', target: { ref: 3 }, text: 'x' });
  assert.equal(r.code, 'not_editable');
  assert.equal(inputs(io).filter(([m]) => m === 'Input.insertText').length, 0);
});

test('上传：点击触发文件选择框后设置文件，并关闭拦截', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo();
  const rt = new TabRuntime(io);
  io.onSend = (m, p) => { if (m === 'Input.dispatchMouseEvent' && p.type === 'mouseReleased') setTimeout(() => rt.onEvent('Page.fileChooserOpened', { backendNodeId: 7, mode: 'selectSingle' }), 5); };
  const r = await rt.act({ action: 'upload', target: { name: '上传图片' }, files: ['C:/a.png'] });
  assert.equal(r.ok, true);
  const set = io.sent.find(([m]) => m === 'DOM.setFileInputFiles');
  assert.deepEqual(plain(set[1]), { files: ['C:/a.png'], backendNodeId: 7 });
  const intercepts = io.sent.filter(([m]) => m === 'Page.setInterceptFileChooserDialog').map(([, p]) => p.enabled);
  assert.deepEqual(intercepts, [true, false]);
});

test('多步：停在第一个失败的步骤并说明位置；expect 未满足算失败', async () => {
  const { TabRuntime } = loadRuntime();
  let n = 0;
  const io = fakeIo({ prepare: () => (++n === 2 ? { error: '未找到可见目标', code: 'not_found' } : { ok: true, x: 1, y: 1, target: {}, ref: 1 }) });
  const r = await new TabRuntime(io).run([{ action: 'click', target: { ref: 1 } }, { action: 'click', target: { ref: 2 } }, { action: 'click', target: { ref: 3 } }]);
  assert.equal(r.ok, false);
  assert.equal(r.stopped_at, 1);
  assert.equal(r.steps.length, 2);

  const io2 = fakeIo({ expect: () => ({ met: false, text_found: false }) });
  const r2 = await new TabRuntime(io2).act({ action: 'click', target: { ref: 1 }, expect: { text: '发布成功', timeout_ms: 0 } });
  assert.equal(r2.code, 'expect_unmet');
});

test('原生对话框：动作后汇报，未处理前拒绝其它动作，dialog 动作处理掉', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo();
  const rt = new TabRuntime(io);
  io.onSend = (m, p) => { if (m === 'Input.dispatchMouseEvent' && p.type === 'mouseReleased') rt.onEvent('Page.javascriptDialogOpening', { type: 'confirm', message: '确定删除？' }); };
  const r = await rt.act({ action: 'click', target: { ref: 1 } });
  assert.equal(r.effect.native_dialog.message, '确定删除？');
  io.onSend = null;
  assert.equal((await rt.act({ action: 'click', target: { ref: 1 } })).code, 'native_dialog');
  const handled = await rt.act({ action: 'dialog', accept: false });
  assert.equal(handled.ok, true);
  assert.deepEqual(plain(io.sent.find(([m]) => m === 'Page.handleJavaScriptDialog')[1]), { accept: false });
});

test('等待页面安静：进行中的请求结束前不返回，并列出请求', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo();
  const rt = new TabRuntime(io);
  io.onSend = (m, p) => {
    if (m === 'Input.dispatchMouseEvent' && p.type === 'mouseReleased') {
      rt.onEvent('Network.requestWillBeSent', { requestId: 'r1', type: 'Fetch', request: { method: 'POST', url: 'https://a.test/api/save?token=secret' } });
      setTimeout(() => { rt.onEvent('Network.responseReceived', { requestId: 'r1', response: { status: 200 } }); rt.onEvent('Network.loadingFinished', { requestId: 'r1' }); }, 400);
    }
  };
  const started = Date.now();
  const r = await rt.act({ action: 'click', target: { ref: 1 } });
  assert.ok(Date.now() - started >= 380);
  assert.deepEqual(plain(r.effect.requests), [{ method: 'POST', url: 'https://a.test/api/save', type: 'Fetch', status: 200 }]);
});

test('按键解析：组合键不产生文字，Shift+Enter 保留回车文字', () => {
  const { parseKey } = loadRuntime();
  assert.deepEqual(plain(parseKey('Control+A')), { key: 'a', code: 'KeyA', keyCode: 65, text: '', modifiers: 2 });
  assert.equal(parseKey('Shift+Enter').modifiers, 8);
  assert.equal(parseKey('Shift+Enter').text, '\r');
  assert.throws(() => parseKey('Hyper+X'));
});

test('按坐标点击：不走 DOM 定位，鼠标落在给定点并报告该点元素', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo({
    describePoint: ({ x, y }) => ({ ok: true, target: { tag: 'button', name: '相机' }, ref: 9, kind: 'other', focused: false, x, y }),
    prepare: () => { throw new Error('按坐标操作不应调用 prepare'); },
  });
  const r = await new TabRuntime(io).act({ action: 'click', point: { x: 321.5, y: 88 } });
  assert.equal(r.ok, true, JSON.stringify(r));
  const pressed = inputs(io).find(([, p]) => p.type === 'mousePressed')[1];
  assert.deepEqual([pressed.x, pressed.y], [321.5, 88]);
  assert.ok(io.pageCalls.includes('describePoint'));
});

test('截图：按像素比缩放，使最长边不超过上限', async () => {
  const { TabRuntime } = loadRuntime();
  const io = fakeIo({ viewport: () => ({ w: 1920, h: 1080, dpr: 1.5, scrollX: 0, scrollY: 400 }) });
  io.onSend = () => {};
  const send = io.send;
  io.send = async (method, params) => (method === 'Page.captureScreenshot' ? (io.sent.push([method, params]), { data: 'iVBOR' }) : send(method, params));
  const shot = await new TabRuntime(io).capture({ max_side: 1500 });
  const clip = io.sent.find(([m]) => m === 'Page.captureScreenshot')[1].clip;
  assert.equal(clip.y, 400);
  assert.ok(Math.abs(clip.scale * 1920 * 1.5 - 1500) < 0.01);
  assert.deepEqual(plain({ w: shot.width, h: shot.height }), { w: 1920, h: 1080 });
});
