import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import { createContext, runInContext } from 'node:vm';

const base = readFileSync(new URL('../browser-extension/page-api.js', import.meta.url), 'utf8');
const sdk = readFileSync(new URL('../browser-extension/page-operations.js', import.meta.url), 'utf8');
const pageRuntime = readFileSync(new URL('../browser-extension/page-runtime.js', import.meta.url), 'utf8');
function fixture(html) {
  const dom = new JSDOM(html, { url: 'https://fixture.invalid/', runScripts: 'outside-only', pretendToBeVisual: true });
  const w = dom.window;
  // jsdom has no layout engine: simulate visibility/geometry, not a real browser.
  w.HTMLElement.prototype.getClientRects = function () {
    for (let p = this; p; p = p.parentElement) {
      if (p.hidden || w.getComputedStyle(p).display === 'none') return [];
    }
    return [{ x: 0, y: 0, width: 100, height: 30 }];
  };
  w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 0, y: 0, left: 0, top: 0, width: 100, height: 30 });
  w.HTMLElement.prototype.scrollIntoView = function () { w.__hit = this; };
  w.document.elementFromPoint = () => w.__hit;
  // Speed up existing primitive settle delays; SDK polling lives in Rust.
  const originalTimeout = w.setTimeout.bind(w);
  w.setTimeout = (fn, ms, ...args) => originalTimeout(fn, Math.min(ms, 2), ...args);
  w.eval(base);
  w.eval(sdk);
  w.eval(pageRuntime);
  return { w, api: w.__dh, close: () => w.close() };
}

test('extension transports inject the SDK on a cold page and preserve structured failures without eval in scripting', async () => {
  const background = readFileSync(new URL('../browser-extension/background.js', import.meta.url), 'utf8');
  const source = file => readFileSync(new URL(`../browser-extension/${file}`, import.meta.url), 'utf8');
  for (const channel of ['debugger', 'extension']) {
    const f = fixture('<button>提交</button><button>提交</button>');
    delete f.w.__dh; // No snapshot or prior capability call.
    const noEvent = { addListener() {} };
    const injected = [];
    let scriptedCalls = 0;
    const chrome = {
      alarms: { create() {}, onAlarm: noEvent },
      runtime: { onStartup: noEvent, onInstalled: noEvent, getURL: file => file },
      tabs: { query: async () => [{ id: 1 }], onCreated: noEvent },
      debugger: {
        onDetach: noEvent, onEvent: noEvent, attach: async () => {},
        sendCommand: async (_, method, args) => method === 'Runtime.evaluate' ? { result: { value: await f.w.eval(args.expression) } } : {},
      },
      scripting: { executeScript: async options => {
        if (options.files) { for (const file of options.files) { injected.push(file); f.w.eval(source(file)); } return []; }
        scriptedCalls++;
        // Production uses static function invocation; this evaluation only simulates Chrome serialization.
        const fn = options.func.toString();
        assert.ok(!/\beval\(/.test(fn));
        return [{ result: await f.w.eval(`(${fn})(...${JSON.stringify(options.args || [])})`) }];
      } },
    };
    const context = createContext({ chrome, console, setInterval() {}, setTimeout() {}, clearTimeout() {},
      WebSocket: class { static CONNECTING = 0; static OPEN = 1; readyState = 0; },
      fetch: async file => ({ text: async () => source(file) }),
    });
    context.self = context;
    context.importScripts = file => runInContext(source(file), context);
    try {
      runInContext(background, context);
      context.params = { method: 'capabilities', args: {}, channel };
      const caps = await runInContext('handle("page_operation", params)', context);
      assert.equal(caps.result.version, 1);
      context.params = { method: 'action', args: { action: 'click', target: { role: 'button', name: '提交' } }, channel };
      const failure = await runInContext('handle("page_operation", params)', context);
      assert.equal(failure.result.code, 'ambiguous');
      assert.equal(failure.result.count, 2);
      if (channel === 'extension') { assert.ok(injected.includes('page-operations.js')); assert.ok(scriptedCalls > 0); }
      context.params = { method: 'evil()', args: {}, channel };
      await assert.rejects(runInContext('handle("page_operation", params)', context), /未知/);
    } finally { f.close(); }
  }
});

test('capabilities, idempotent injection, exact scoped lookup and retained refs', async () => {
  const f = fixture('<section id="one"><button>提交</button></section><section id="two"><button>提交</button></section>');
  try {
    assert.equal(f.api.capabilities().version, 1);
    const old = f.api.action;
    f.w.eval(sdk);
    assert.equal(f.api.action, old);
    let clicks = 0;
    f.w.document.querySelectorAll('button').forEach(el => el.onclick = () => clicks++);
    const ambiguous = await f.api.action({ action: 'click', target: { role: 'button', name: '提交' } });
    assert.equal(ambiguous.code, 'ambiguous');
    assert.equal(clicks, 0);
    const ref = ambiguous.candidates[0].ref;
    const located = await f.api.action({ action: 'locate', target: { name: '提交', scope: '#two' } });
    assert.equal(located.count, 1);
    assert.equal(f.api.ref(ref), f.w.document.querySelector('#one button'));
    const result = await f.api.action({ action: 'click', target: { role: 'button', name: '提交', scope: '#two' } });
    assert.equal(result.ok, true);
    assert.equal(clicks, 1);
    assert.equal((await f.api.action({ action: 'click', target: { css: 'button', scope: '.missing' } })).code, 'invalid_request');
  } finally { f.close(); }
});

test('disabled, hidden, covered and stale targets are not clicked', async () => {
  const f = fixture('<button disabled id="disabled">禁用</button><button hidden id="hidden">隐藏</button><button id="covered">遮挡</button>');
  try {
    assert.equal((await f.api.action({ action: 'click', target: { css: '#disabled' } })).code, 'disabled');
    assert.equal((await f.api.action({ action: 'click', target: { css: '#hidden' } })).code, 'not_found');
    f.w.document.elementFromPoint = () => f.w.document.body;
    assert.equal((await f.api.action({ action: 'click', target: { css: '#covered' } })).code, 'covered');
    const located = await f.api.action({ action: 'locate', target: { css: '#covered' } });
    f.w.document.querySelector('#covered').remove();
    assert.equal((await f.api.action({ action: 'click', target: { ref: located.candidates[0].ref } })).code, 'not_found');
  } finally { f.close(); }
});

test('fill reuses native setter and input events; checkbox operation is idempotent', async () => {
  const f = fixture('<label for="title">标题</label><input id="title"><label><input type="checkbox" id="agree">同意</label>');
  try {
    let inputs = 0;
    f.w.document.querySelector('#title').addEventListener('input', () => inputs++);
    const result = await f.api.action({ action: 'fill', target: { label: '标题' }, text: '测试结论' });
    assert.equal(result.ok, true);
    assert.equal(f.w.document.querySelector('#title').value, '测试结论');
    assert.ok(inputs > 0);
    let clicks = 0;
    f.w.document.querySelector('#agree').addEventListener('click', () => clicks++);
    assert.equal((await f.api.action({ action: 'set_checked', target: { css: '#agree' }, checked: true })).verified, true);
    assert.equal((await f.api.action({ action: 'set_checked', target: { css: '#agree' }, checked: true })).verified, true);
    assert.equal(clicks, 1);
  } finally { f.close(); }
});

test('native select rejects ambiguous/disabled options and verifies the selected value', async () => {
  const f = fixture('<select aria-label="地区"><option value="a">大阪</option><option value="b">东京</option><option disabled value="c">禁用</option></select>');
  try {
    const result = await f.api.action({ action: 'select', target: { label: '地区' }, text: '东京' });
    assert.equal(result.verified, true);
    assert.equal(f.w.document.querySelector('select').value, 'b');
    assert.equal((await f.api.action({ action: 'select', target: { label: '地区' }, text: '禁用' })).code, 'disabled');
    assert.equal((await f.api.action({ action: 'select', target: { label: '地区' }, text: '京' })).code, 'ambiguous');
    f.w.document.querySelector('select').insertAdjacentHTML('beforeend', '<option value="d">东京</option>');
    assert.equal((await f.api.action({ action: 'select', target: { label: '地区' }, text: '东京' })).code, 'ambiguous');
  } finally { f.close(); }
});

test('ARIA portal selector uses related listbox and verifies aria-selected', async () => {
  const f = fixture('<button role="combobox" aria-label="地区" aria-controls="options" aria-expanded="false">选择</button><div role="listbox" id="options" hidden><div role="option" aria-selected="false">大阪</div></div><div role="listbox"><div role="option">大阪</div></div>');
  try {
    const button = f.w.document.querySelector('button');
    button.onclick = () => { button.setAttribute('aria-expanded', 'true'); f.w.document.querySelector('#options').hidden = false; };
    const option = f.w.document.querySelector('#options [role="option"]');
    option.onclick = () => option.setAttribute('aria-selected', 'true');
    const result = await f.api.action({ action: 'select', target: { role: 'combobox', label: '地区' }, text: '大阪' });
    assert.equal(result.verified, true);
    assert.equal(option.getAttribute('aria-selected'), 'true');
    assert.equal(f.w.document.querySelectorAll('[role="option"]')[1].getAttribute('aria-selected'), null);
  } finally { f.close(); }
});

test('single-observation wait conditions do not click and reject ambiguous/invalid requests', async () => {
  const f = fixture('<div id="result">正在加载</div><input value="done"><button disabled>查询</button><i hidden id="spinner"></i>');
  try {
    assert.equal((await f.api.probe({ target: { css: '#result' }, condition: 'text', text: '完成' })).matched, false);
    f.w.document.querySelector('#result').textContent = '查询完成';
    assert.equal((await f.api.probe({ target: { css: '#result' }, condition: 'text', text: '完成' })).matched, true);
    assert.equal((await f.api.probe({ target: { css: 'input' }, condition: 'value', text: 'done' })).matched, true);
    assert.equal((await f.api.probe({ target: { css: 'button' }, condition: 'enabled' })).matched, false);
    assert.equal((await f.api.probe({ target: { css: '#spinner' }, condition: 'hidden' })).matched, true);
    assert.equal((await f.api.probe({ target: { css: '[' }, condition: 'hidden' })).code, 'invalid_request');
    f.w.document.body.insertAdjacentHTML('beforeend', '<button>查询</button>');
    assert.equal((await f.api.probe({ target: { css: 'button' }, condition: 'visible' })).code, 'ambiguous');
  } finally { f.close(); }
});

test('extraction enforces total character budget and rejects sensitive attributes', async () => {
  const cards = Array.from({ length: 100 }, () => `<section class="card"><b>${'x'.repeat(1000)}</b></section>`).join('');
  const f = fixture(cards);
  try {
    const result = await f.api.extract({ kind: 'list', row_selector: '.card', fields: [{ name: 'text', selector: 'b' }], limit: 100 });
    assert.equal(result.truncated, true);
    assert.ok(result.items.length < 100);
    assert.ok(result.items.reduce((sum, item) => sum + item.text.length, 0) <= 16000);
    assert.equal((await f.api.extract({ kind: 'fields', scope: '.card:nth-child(1)', fields: [{ name: 'value', selector: 'b', attribute: 'value' }] })).code, 'invalid_request');
  } finally { f.close(); }
});

test('bounded tables/cards/fields extraction preserves missing data without guessing', async () => {
  const f = fixture('<table><tr><th>名称</th><th>价格</th></tr><tr><td>板 A</td><td>¥ 12</td></tr><tr><td>板 B</td><td>$ 3</td></tr></table><section class="card"><a href="/a">板 A</a><b>¥ 12</b></section><section class="card"><a href="/b">板 B</a></section>');
  try {
    const table = await f.api.extract({ kind: 'table', limit: 2 });
    assert.equal(table.rows.length, 2);
    assert.equal(table.truncated, true);
    const fields = [{ name: 'title', selector: 'a' }, { name: 'url', selector: 'a', attribute: 'href' }, { name: 'price', selector: 'b' }];
    const list = await f.api.extract({ kind: 'list', row_selector: '.card', fields });
    assert.equal(list.items[0].url, 'https://fixture.invalid/a');
    assert.equal(list.items[1].price, null);
    assert.equal((await f.api.extract({ kind: 'fields', scope: '.card', fields })).code, 'invalid_request');
    assert.equal((await f.api.extract({ kind: 'fields', scope: '.card:first-of-type', fields: [{ name: 'secret', selector: 'a', attribute: 'value' }] })).code, 'invalid_request');
  } finally { f.close(); }
});
