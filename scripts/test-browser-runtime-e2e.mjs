// 真实输入引擎端到端测试：启动独立的无头 Chrome（临时用户目录，不碰日常浏览器），
// 通过 CDP 驱动与扩展相同的 runtime.js + 页面脚本，验证"只认真人操作"的组件。
// 找不到 Chrome/Edge 时跳过。可用 CHROME_PATH 指定浏览器。
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { createContext, runInContext } from 'node:vm';

const read = (file) => readFileSync(new URL(`../browser-extension/${file}`, import.meta.url), 'utf8');
const PAGE_SRC = ['readability.js', 'page-api.js', 'page-operations.js', 'page-runtime.js'].map(read).join('\n');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const FIXTURE = `<!doctype html><meta charset="utf-8"><title>运行时测试</title>
<style>body{font:14px sans-serif;padding:20px} .mask{position:fixed;inset:0;background:rgba(0,0,0,.2)} .row{margin:12px 0}</style>
<div class="row"><input id="controlled" placeholder="受控输入"><span id="controlled-out"></span></div>
<div class="row"><div id="chat" contenteditable="true" data-placeholder="说点什么" style="min-height:40px;border:1px solid #999"></div><div id="sent"></div></div>
<div class="row"><div id="model-editor" contenteditable="true" style="min-height:40px;border:1px solid #999"></div></div>
<div class="row"><button id="trusted-btn">只认真人点击</button><span id="trusted-count">0</span></div>
<div class="row" id="list"><button class="re">重渲染按钮</button></div><span id="re-count">0</span>
<div class="row"><button id="save">保存</button></div>
<div class="row"><input type="file" id="file" hidden multiple><button id="pick">上传图片</button><span id="fname"></span></div>
<div class="row"><div id="city" role="combobox" aria-expanded="false" aria-controls="city-list" tabindex="0" style="border:1px solid #999;width:200px">请选择城市</div>
<ul id="city-list" role="listbox" hidden><li role="option">北京</li><li role="option">大阪</li></ul></div>
<div class="row"><button id="danger">删除</button><span id="danger-out"></span></div>
<div class="row"><button id="covered">被遮挡</button></div>
<div class="row" style="text-align:right"><button id="trash" style="width:44px;height:44px;border:0;background:#fff"><svg width="32" height="32" viewBox="0 0 24 24"><path d="M9 3h6l1 2h4v2H4V5h4l1-2zm-3 6h12l-1 12H7L6 9z" fill="#d32f2f"/></svg></button><span id="trash-count">0</span></div>
<script>
const $ = (id) => document.getElementById(id);
// 受控组件：非真实输入会被状态回滚（React 受控输入的典型表现）。
let state = '';
$('controlled').addEventListener('input', (e) => {
  if (!e.isTrusted) { $('controlled').value = state; return; }
  state = $('controlled').value; $('controlled-out').textContent = state;
});
// 聊天框：回车直接发送，Shift+Enter 换行。
$('chat').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); $('sent').textContent += '[发送]' + $('chat').innerText; $('chat').innerHTML = ''; }
});
// 模型驱动编辑器（Slate/Lexical 类）：自己处理 beforeinput，只认真实输入。
let model = [''];
const render = () => { $('model-editor').innerHTML = model.map((p) => '<p>' + (p || '<br>') + '</p>').join(''); };
$('model-editor').addEventListener('beforeinput', (e) => {
  e.preventDefault();
  if (!e.isTrusted) return;
  if (e.inputType === 'insertText') model[model.length - 1] += e.data;
  if (e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak') model.push('');
  render();
});
$('trusted-btn').addEventListener('click', (e) => { if (e.isTrusted) $('trusted-count').textContent = String(Number($('trusted-count').textContent) + 1); });
$('list').addEventListener('click', (e) => {
  if (!e.target.classList.contains('re') || !e.isTrusted) return;
  $('re-count').textContent = String(Number($('re-count').textContent) + 1);
});
$('save').addEventListener('click', async () => {
  const r = await fetch('/api/save', { method: 'POST', body: '{}' });
  const tip = document.createElement('div'); tip.setAttribute('role', 'alert'); tip.textContent = r.ok ? '保存成功' : '保存失败';
  document.body.appendChild(tip);
});
$('pick').addEventListener('click', () => $('file').click());
$('file').addEventListener('change', () => { $('fname').textContent = Array.from($('file').files).map((f) => f.name).join(','); });
$('city').addEventListener('click', (e) => {
  if (!e.isTrusted) return;
  const open = $('city-list').hidden; $('city-list').hidden = !open; $('city').setAttribute('aria-expanded', String(open));
});
$('city-list').addEventListener('click', (e) => {
  if (!e.isTrusted || e.target.getAttribute('role') !== 'option') return;
  $('city').textContent = e.target.textContent; $('city-list').hidden = true; $('city').setAttribute('aria-expanded', 'false');
});
$('trash').addEventListener('click', (e) => { if (e.isTrusted) $('trash-count').textContent = String(Number($('trash-count').textContent) + 1); });
$('danger').addEventListener('click', () => { if (confirm('确定删除？')) $('danger-out').textContent = '已删除'; });
</script>
<div class="mask" id="mask" style="top:auto;height:60px"></div>`;

function findChrome() {
  const candidates = [
    process.env.CHROME_PATH,
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    '/usr/bin/google-chrome', '/usr/bin/chromium',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  ];
  return candidates.find((p) => p && existsSync(p));
}

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.seq = 0;
    this.pending = new Map();
    this.listeners = new Set();
    this.ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) reject(new Error(m.error.message)); else resolve(m.result);
      } else if (m.method) {
        for (const l of this.listeners) l(m);
      }
    };
  }
  open() { return new Promise((resolve, reject) => { this.ws.onopen = resolve; this.ws.onerror = reject; }); }
  send(method, params = {}, sessionId) {
    const id = ++this.seq;
    this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

const chromePath = findChrome();
let chrome, server, cdp, sessionId, rt, profile, uploadDir, origin;

async function evaluate(expression) {
  const r = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId);
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result.value;
}
// 与扩展 background.js 的 callPageApi 相同：缺失才注入。
async function callPage(expr) {
  const guarded = `(async()=>{if(!(window.__dh&&window.__dh.rt))return{__dhMissing:true};return{v:await (${expr})};})()`;
  let res = await evaluate(guarded);
  if (res && res.__dhMissing) { await evaluate(`${PAGE_SRC}\n;true`); res = await evaluate(guarded); }
  return res.v;
}
async function navigate() {
  const loaded = new Promise((resolve) => {
    const l = (m) => { if (m.method === 'Page.loadEventFired' && m.sessionId === sessionId) { cdp.listeners.delete(l); resolve(); } };
    cdp.listeners.add(l);
  });
  await cdp.send('Page.navigate', { url: `${origin}/` }, sessionId);
  await loaded;
}

before(async () => {
  if (!chromePath) return;
  server = createServer(async (req, res) => {
    if (req.url === '/api/save') { await sleep(400); res.writeHead(200, { 'content-type': 'application/json' }); res.end('{"ok":true}'); return; }
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' }); res.end(FIXTURE);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  origin = `http://127.0.0.1:${server.address().port}`;
  profile = mkdtempSync(join(tmpdir(), 'shiguang-e2e-'));
  uploadDir = mkdtempSync(join(tmpdir(), 'shiguang-upload-'));
  writeFileSync(join(uploadDir, '截图.png'), 'png');
  chrome = spawn(chromePath, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check', 'about:blank'], { stdio: 'ignore' });
  const portFile = join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 100 && !existsSync(portFile); i++) await sleep(100);
  const [port, path] = readFileSync(portFile, 'utf8').trim().split('\n');
  cdp = new Cdp(`ws://127.0.0.1:${port}${path}`);
  await cdp.open();
  const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
  ({ sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true }));
  await cdp.send('Page.enable', {}, sessionId);
  const ctx = createContext({ setTimeout, clearTimeout, console, URL });
  runInContext(read('runtime.js'), ctx);
  rt = new ctx.DhRuntime.TabRuntime({
    send: (method, params) => cdp.send(method, params, sessionId),
    page: (method, arg) => callPage(`window.__dh.rt[${JSON.stringify(method)}](${JSON.stringify(arg ?? null)})`),
  });
  cdp.listeners.add((m) => { if (m.sessionId === sessionId) rt.onEvent(m.method, m.params); });
  await navigate();
});

after(async () => {
  try { await cdp?.send('Browser.close'); } catch { /* 已关闭 */ }
  chrome?.kill();
  server?.close();
  await sleep(300);
  for (const dir of [profile, uploadDir]) if (dir) rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
});

const skip = !chromePath && '未找到 Chrome/Edge';

test('受控输入：旧的页面内输入被回滚，真实输入生效', { skip }, async () => {
  const legacy = await callPage(`(async()=>{const r=window.__dh.find('受控输入',3);return window.__dh.type(r.candidates[0].ref,'旧方法',true);})()`);
  assert.ok(legacy.error, '旧实现应被受控组件拒绝');
  const r = await rt.act({ action: 'fill', target: { placeholder: '受控输入' }, text: '真实输入' });
  assert.equal(r.verified, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.getElementById('controlled-out').textContent`), '真实输入');
});

test('聊天框：默认 Shift+Enter 换行不会误发送；按回车才发送', { skip }, async () => {
  const r = await rt.act({ action: 'fill', target: { css: '#chat' }, text: '第一行\n第二行' });
  assert.equal(r.verified, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.getElementById('sent').textContent`), '');
  await rt.act({ action: 'press', key: 'Enter', target: { css: '#chat' } });
  assert.match(await evaluate(`document.getElementById('sent').textContent`), /\[发送\]第一行\s*第二行/);
});

test('模型驱动编辑器（只认真实 beforeinput）：分段输入并读回', { skip }, async () => {
  const r = await rt.act({ action: 'fill', target: { css: '#model-editor' }, text: '标题段\n正文段', newline: 'enter' });
  assert.equal(r.verified, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.querySelectorAll('#model-editor p').length`), 2);
});

test('只认真人点击的按钮被点中；被遮挡的按钮不点并报告遮挡者', { skip }, async () => {
  const r = await rt.act({ action: 'click', target: { css: '#trusted-btn' } });
  assert.equal(r.ok, true);
  assert.equal(await evaluate(`document.getElementById('trusted-count').textContent`), '1');
  await evaluate(`(()=>{const b=document.getElementById('covered');const m=document.getElementById('mask');const r=b.getBoundingClientRect();m.style.top=(r.top-10)+'px';m.style.height=(r.height+20)+'px';})()`);
  await evaluate(`window.scrollTo(0,0)`);
  const covered = await rt.page('prepare', { target: { css: '#covered' } });
  // scrollIntoView 后遮罩仍盖住按钮：遮罩是 fixed，随按钮当前视口位置重新摆放。
  if (covered.ok) {
    await evaluate(`(()=>{const b=document.getElementById('covered');const m=document.getElementById('mask');const r=b.getBoundingClientRect();m.style.top=(r.top-10)+'px';})()`);
  }
  const blocked = await rt.act({ action: 'click', target: { css: '#covered' }, settle_ms: 0 });
  assert.equal(blocked.code, 'covered', JSON.stringify(blocked));
  await evaluate(`document.getElementById('mask').remove()`);
});

test('滚动后仍不在可视区域的目标报 offscreen，不说成被遮挡', { skip }, async () => {
  await evaluate(`(()=>{const b=document.createElement('button');b.id='offscreen';b.textContent='视口外按钮';b.style.cssText='position:fixed;top:-200px;left:10px';document.body.appendChild(b);})()`);
  const r = await rt.act({ action: 'click', target: { css: '#offscreen' }, settle_ms: 0 });
  assert.equal(r.code, 'offscreen', JSON.stringify(r));
  assert.match(r.error, /不在可视区域/);
  await evaluate(`document.getElementById('offscreen').remove()`);
});

test('ref 指向的元素被重渲染替换后仍能点中', { skip }, async () => {
  const found = await callPage(`window.__dh.find('重渲染按钮', 3)`);
  const ref = found.candidates[0].ref;
  await evaluate(`document.getElementById('list').innerHTML = '<button class="re">重渲染按钮</button>'`);
  const r = await rt.act({ action: 'click', target: { ref } });
  assert.equal(r.relocated, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.getElementById('re-count').textContent`), '1');
});

test('点击后等待请求完成，汇报请求和提示文字', { skip }, async () => {
  const r = await rt.act({ action: 'click', target: { css: '#save' } });
  assert.equal(r.effect.settled, true, JSON.stringify(r.effect));
  assert.ok(r.effect.requests.some((q) => q.url.endsWith('/api/save') && q.status === 200), JSON.stringify(r.effect));
  assert.ok((r.effect.messages || []).includes('保存成功'), JSON.stringify(r.effect));
});

test('上传：点按钮弹出文件选择框后设置文件；也可直接指定隐藏的文件框', { skip }, async () => {
  const file = join(uploadDir, '截图.png');
  const r = await rt.act({ action: 'upload', target: { css: '#pick' }, files: [file] });
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.getElementById('fname').textContent`), '截图.png');
  const direct = await rt.act({ action: 'upload', target: { css: '#file' }, files: [file, file] });
  assert.equal(direct.ok, true, JSON.stringify(direct));
  assert.equal(await evaluate(`document.getElementById('file').files.length`), 2);
});

test('自定义下拉：真实点击展开并选中，读回控件文字', { skip }, async () => {
  const r = await rt.act({ action: 'select', target: { css: '#city' }, text: '大阪' });
  assert.equal(r.verified, true, JSON.stringify(r));
  assert.equal(await evaluate(`document.getElementById('city').textContent`), '大阪');
});

test('原生 confirm：动作后汇报，dialog 确认后页面继续', { skip }, async () => {
  const r = await rt.act({ action: 'click', target: { css: '#danger' } });
  assert.equal(r.effect.native_dialog?.message, '确定删除？', JSON.stringify(r));
  const handled = await rt.act({ action: 'dialog', accept: true });
  assert.equal(handled.ok, true, JSON.stringify(handled));
  assert.equal(await evaluate(`document.getElementById('danger-out').textContent`), '已删除');
});

test('多步一次提交：填写后保存，expect 校验结果', { skip }, async () => {
  await navigate();
  const r = await rt.run([
    { action: 'fill', target: { placeholder: '受控输入' }, text: '批量' },
    { action: 'click', target: { css: '#save' }, expect: { text: '保存成功' } },
  ]);
  assert.equal(r.ok, true, JSON.stringify(r));
  const failed = await rt.run([
    { action: 'click', target: { css: '#trusted-btn' } },
    { action: 'click', target: { name: '不存在的按钮' } },
    { action: 'click', target: { css: '#trusted-btn' } },
  ]);
  assert.equal(failed.stopped_at, 1);
  assert.equal(await evaluate(`document.getElementById('trusted-count').textContent`), '1');
});

const pngSize = (b64) => { const b = Buffer.from(b64, 'base64'); return [b.readUInt32BE(16), b.readUInt32BE(20)]; };
const rectOf = (css) => evaluate(`(()=>{const r=document.querySelector(${JSON.stringify(css)}).getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom};})()`);

test('截图尺寸符合 Jev 要求；按坐标点击能点中只认真人点击的按钮', { skip }, async () => {
  await navigate();
  const shot = await rt.capture({ max_side: 1500 });
  const [w, h] = pngSize(shot.data);
  assert.ok(w >= 280 && w <= 1568 && h >= 280 && h <= 1568, `${w}x${h}`);
  const r = await rectOf('#trusted-btn');
  const res = await rt.act({ action: 'click', point: { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 } });
  assert.equal(res.ok, true, JSON.stringify(res));
  assert.equal(res.target.name, '只认真人点击');
  assert.equal(await evaluate(`document.getElementById('trusted-count').textContent`), '1');
});

// 真实调用 TypeSafe：需要设置 TYPESAFE_API_KEY，会产生少量费用。
const liveKey = process.env.TYPESAFE_API_KEY;
async function jevLocate(b64, description) {
  const key = `img-${Date.now()}`;
  const res = await fetch(`${process.env.TYPESAFE_BASE_URL || 'https://api.typesafe.ai/v1'}/systemone`, {
    method: 'POST',
    headers: { authorization: `Bearer ${liveKey}`, 'content-type': 'application/json', 'TypeSafe-Danger-Zone-Images': 'true' },
    body: JSON.stringify({
      model: process.env.TYPESAFE_MODEL || 'jev-latest',
      state: { screenshot: key },
      assets: { [key]: { type: 'image', format: 'png', data_base64: b64 } },
      questions: {
        target: { type: 'box', instructions: `${description} in \`screenshot\`` },
        present: { type: 'noul', instructions: `Is ${description} visible in \`screenshot\`?` },
      },
    }),
  });
  const body = await res.json();
  assert.equal(res.status, 200, JSON.stringify(body));
  return body;
}

test('真实 Jev：在截图里定位按钮，框中心落在按钮上；不存在的目标判为不存在', { skip: skip || (!liveKey && '未设置 TYPESAFE_API_KEY') }, async () => {
  await navigate();
  const shot = await rt.capture({ max_side: 1500 });
  if (process.env.SHOT_OUT) writeFileSync(process.env.SHOT_OUT, Buffer.from(shot.data, 'base64'));
  const found = await jevLocate(shot.data, 'the button labeled "保存"');
  const box = found.answers.target.box;
  const x = (box.left + box.right) / 2 * shot.width;
  const y = (box.top + box.bottom) / 2 * shot.height;
  const r = await rectOf('#save');
  console.log('jev', found.model, 'present', found.answers.present.noul, 'box', JSON.stringify(box), 'conf', found.answers.target.confidence, 'point', x.toFixed(1), y.toFixed(1), 'rect', JSON.stringify(r));
  assert.ok(found.answers.present.noul >= 0.6, `present=${found.answers.present.noul}`);
  assert.ok(x >= r.left - 4 && x <= r.right + 4 && y >= r.top - 4 && y <= r.bottom + 4, `point (${x},${y}) outside ${JSON.stringify(r)}`);
  const clicked = await rt.act({ action: 'click', point: { x, y } });
  assert.ok((clicked.effect.messages || []).includes('保存成功'), JSON.stringify(clicked.effect));
  const zh = await jevLocate(shot.data, '写着“保存”的按钮');
  const zb = zh.answers.target.box;
  const zx = (zb.left + zb.right) / 2 * shot.width, zy = (zb.top + zb.bottom) / 2 * shot.height;
  console.log('jev zh present', zh.answers.present.noul, 'conf', zh.answers.target.confidence, 'point', zx.toFixed(1), zy.toFixed(1));
  assert.ok(zh.answers.present.noul >= 0.6 && zx >= r.left - 4 && zx <= r.right + 4 && zy >= r.top - 4 && zy <= r.bottom + 4, '中文描述定位失败');
  // 按画面定位只看当前视口：先把图标滚动到可见区域、去掉底部遮罩再截图。
  await evaluate(`document.getElementById('mask')?.remove(); document.getElementById('trash').scrollIntoView({block:'center'})`);
  const iconShot = await rt.capture({ max_side: 1500 });
  if (process.env.SHOT_OUT) writeFileSync(process.env.SHOT_OUT, Buffer.from(iconShot.data, 'base64'));
  const icon = await jevLocate(iconShot.data, '页面右侧红色的垃圾桶图标');
  const ib = icon.answers.target.box;
  const ix = (ib.left + ib.right) / 2 * iconShot.width, iy = (ib.top + ib.bottom) / 2 * iconShot.height;
  const ir = await rectOf('#trash');
  console.log('jev icon present', icon.answers.present.noul, 'conf', icon.answers.target.confidence, 'point', ix.toFixed(1), iy.toFixed(1), 'rect', JSON.stringify(ir));
  assert.ok(icon.answers.present.noul >= 0.6, `icon present=${icon.answers.present.noul}`);
  const iconClick = await rt.act({ action: 'click', point: { x: ix, y: iy } });
  assert.equal(await evaluate(`document.getElementById('trash-count').textContent`), '1', JSON.stringify(iconClick));
  const missing = await jevLocate(shot.data, 'a red airplane icon');
  console.log('jev missing present', missing.answers.present.noul);
  assert.ok(missing.answers.present.noul < 0.6, `missing present=${missing.answers.present.noul}`);
});
