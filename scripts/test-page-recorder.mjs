// 录屏页面脚本：打码规则与事件轨（jsdom 模拟，无真实布局）。
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';

const src = readFileSync(new URL('../src-tauri/plugins/recorder/assets/page-recorder.js', import.meta.url), 'utf8');

function fixture(html, cfg = {}) {
  const dom = new JSDOM(html, { url: 'https://fixture.invalid/', runScripts: 'outside-only', pretendToBeVisual: true });
  const w = dom.window;
  w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 10, y: 20, left: 10, top: 20, width: 100, height: 30 });
  const events = [];
  w.__sgRec = (json) => events.push(JSON.parse(json));
  w.eval(src);
  w.__sgRecInstall(cfg);
  const frame = () => new Promise((r) => w.requestAnimationFrame(() => r()));
  return { w, doc: w.document, events, frame, close: () => w.close() };
}

const masked = (el) => el.hasAttribute('data-sg-mask') || !!el.closest('sg-mask');

test('密码框、手机号、邮箱在安装时就打码，普通文字不动', () => {
  const { doc, close } = fixture(`
    <input id="pw" type="password" value="secret">
    <p id="phone">联系电话 13812345678</p>
    <span id="mail">a.b@example.com</span>
    <p id="plain">今天天气不错</p>`);
  assert.ok(masked(doc.getElementById('pw')));
  assert.ok(masked(doc.getElementById('phone')));
  assert.ok(masked(doc.getElementById('mail')));
  assert.ok(!masked(doc.getElementById('plain')));
  close();
});

test('关闭的预设不打码，自定义选择器和正则生效', () => {
  const { doc, close } = fixture(`
    <p id="phone">13812345678</p>
    <div class="customer">张三</div>
    <p id="order">订单 SO-2026-0001</p>`, { presets: { phone: false }, selectors: ['.customer'], patterns: ['SO-\\d{4}-\\d{4}'] });
  assert.ok(!masked(doc.getElementById('phone')));
  assert.ok(masked(doc.querySelector('.customer')));
  assert.ok(masked(doc.getElementById('order')));
  close();
});

test('后来插入的敏感内容在下一帧前打码', async () => {
  const { doc, frame, close } = fixture('<div id="root"></div>');
  const p = doc.createElement('p');
  p.textContent = '身份证 110101199003077777';
  doc.getElementById('root').appendChild(p);
  await new Promise((r) => setTimeout(r, 0)); // MutationObserver 回调
  await frame();
  assert.ok(masked(p));
  close();
});

test('输入框输入敏感内容后打码', async () => {
  const { w, doc, frame, close } = fixture('<input id="q">');
  const input = doc.getElementById('q');
  input.value = 'token sk-abcdefghijklmnopqrstuvwxyz';
  input.dispatchEvent(new w.Event('input', { bubbles: true }));
  await frame();
  assert.ok(masked(input));
  close();
});

test('点击上报元素文字和位置；密码框按键不记录，普通输入不记内容', () => {
  const { w, doc, events, close } = fixture('<button id="b">提交订单</button><input id="pw" type="password"><input id="t">');
  doc.getElementById('b').dispatchEvent(new w.MouseEvent('click', { bubbles: true, clientX: 50, clientY: 30 }));
  const click = events.find((e) => e.type === 'click');
  assert.equal(click.target.label, '提交订单');
  assert.deepEqual(click.target.bbox, [10, 20, 100, 30]);
  assert.equal(click.x, 50);
  doc.getElementById('pw').dispatchEvent(new w.KeyboardEvent('keydown', { key: 'a', bubbles: true }));
  assert.equal(events.filter((e) => e.type === 'key').length, 0);
  doc.getElementById('t').dispatchEvent(new w.KeyboardEvent('keydown', { key: 'a', bubbles: true }));
  doc.getElementById('t').dispatchEvent(new w.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  const keys = events.filter((e) => e.type === 'key').map((e) => e.key);
  assert.deepEqual(keys, ['char', 'Enter']);
  close();
});

test('打码元素上的点击不泄露文字', () => {
  const { w, doc, events, close } = fixture('<button id="b">13812345678</button>');
  doc.getElementById('b').dispatchEvent(new w.MouseEvent('click', { bubbles: true }));
  assert.equal(events.find((e) => e.type === 'click').target.label, '（已打码）');
  close();
});

test('卸载后去掉所有打码标记', () => {
  const { w, doc, close } = fixture('<p id="p">13812345678</p>');
  assert.ok(masked(doc.getElementById('p')));
  w.__sgRecUninstall();
  assert.ok(!masked(doc.getElementById('p')));
  close();
});
