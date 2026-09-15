// Run with node tools/test-chart.cjs. Exercise the chart's actual scale logic.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync(require('node:path').join(__dirname, '../ui/chart.html'), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
new vm.Script(script); // Check the entire page script, including event handlers.
const extract = (name) => script.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n\\}`))[0];
const context = vm.createContext({});
vm.runInContext(extract('niceCeil') + '\n' + extract('networkChartDomain'), context);
const domain = context.networkChartDomain;
const aggregate = [{ t: 10000, up: 1000, down: 500 }];
const nodes = [[{ t: 10000, up: 200, down: 100 }]];
const a = domain(10500, 5, aggregate, nodes.values());
assert.equal(a.start, 5000);
assert.equal(a.end, 10000);
assert.equal(a.max, 2000);
assert.equal(domain(10999, 5, aggregate, nodes.values()).max, a.max);
// Out-of-range samples must not distort any chart's shared vertical scale.
assert.equal(domain(10500, 5, aggregate, [[
  { t: 4999, up: 1e9, down: 0 }, { t: 10001, up: 1e9, down: 0 },
]]).max, a.max);
// A node peak above the aggregate still fits the common scale.
assert.equal(domain(10500, 5, aggregate, [[{ t: 10000, up: 3000, down: 0 }]]).max, 5000);
assert.equal(domain(10500, 5, [], []).max, 1024);
console.log('Chart script syntax and shared axis regression checks passed.');
// Verify redraw uses the current CSS dimensions, including high-DPI scaling.
Object.assign(context, {
  window: { devicePixelRatio: 2 }, bindChartHover() {}, isDarkTheme: () => false,
  points: [], nodeSeries: new Map(), rangeSecs: 300,
  AXIS_TIME_FORMAT: { format: () => '12:00' }, fmtAxis: String,
});
vm.runInContext(extract('drawNetworkChart'), context);
const ctx = new Proxy({}, { get: (obj, key) => obj[key] || (() => {}) });
const canvas = { clientWidth: 292, clientHeight: 124, setAttribute() {}, getContext: () => ctx };
context.drawNetworkChart(canvas, [], false);
assert.equal(canvas.width, 584);
assert.equal(canvas.height, 248);
canvas.clientWidth = 612;
canvas.clientHeight = 240;
context.drawNetworkChart(canvas, [], false);
assert.equal(canvas.width, 1224);
assert.equal(canvas.height, 480);
context.drawNetworkChart(canvas, [], true);
assert.equal(canvas.width, 1224);
console.log('Aggregate and node canvas resize regression checks passed.');
// Expanding content grows the window until screen space is exhausted; shrinking
// content releases that space again. Saved width must not bypass this request.
const panel = { style: {}, offsetHeight: 540 };
const requests = [];
Object.assign(context, {
  document: { getElementById: () => panel, body: { style: {} } },
  invoke: async (command, { height }) => {
    assert.equal(command, 'resize_chart');
    requests.push(height);
    context.window.innerHeight = Math.min(height, 988);
    return context.window.innerHeight;
  },
  setTimeout: (callback) => callback(), fitAgain: false,
});
vm.runInContext(extract('fitHeightOnce'), context);
(async () => {
  await context.fitHeightOnce();
  assert.equal(context.window.innerHeight, 540);
  panel.offsetHeight = 1400;
  await context.fitHeightOnce();
  assert.equal(context.window.innerHeight, 988);
  panel.offsetHeight = 400;
  await context.fitHeightOnce();
  assert.equal(context.window.innerHeight, 400);
  assert.deepEqual(requests, [540, 1400, 400]);
  assert.equal(panel.style.height, '100%');
  console.log('Content expansion, screen limit, and collapse regression checks passed.');
})().catch((error) => { console.error(error); process.exitCode = 1; });
