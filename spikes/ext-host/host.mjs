// host.mjs — spike: out-of-process plugin host, JSON-RPC 2.0 over stdio (NDJSON).
// Runs 4 tests: (a) cold start, (b) warm onQuery, (c) timeout+kill, (d) crash isolation.
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { EventEmitter } from 'node:events';
import { performance } from 'node:perf_hooks';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const NODE = process.execPath;
const PLUGIN = path.join(HERE, 'plugin.mjs');
const CRAZY = path.join(HERE, 'plugin-crazy.mjs');
const CRASH = path.join(HERE, 'plugin-crash.mjs');

process.on('unhandledRejection', (e) => {
  console.error('[host] unhandledRejection:', e?.message ?? e);
});

/** Manages one plugin child process + JSON-RPC 2.0 client with per-call timeouts. */
class PluginClient extends EventEmitter {
  constructor(script, { callTimeoutMs = 3000 } = {}) {
    super();
    this.script = script;
    this.callTimeoutMs = callTimeoutMs;
    this.proc = null;
    this.pending = new Map(); // id -> {resolve, reject}
    this.nextId = 1;
    this.#killing = false;
    this.exitInfo = null;
    this.exitPromise = null;
  }

  #killing;

  start() {
    const t0 = performance.now();
    this.proc = spawn(NODE, [this.script], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.#spawnMs = () => performance.now() - t0;
    this.exitPromise = new Promise((resolve) => {
      this.proc.once('close', (code, signal) => {
        this.exitInfo = { code, signal };
        resolve(this.exitInfo);
      });
    });
    const rl = readline.createInterface({ input: this.proc.stdout, terminal: false });
    rl.on('line', (line) => this.#onLine(line));
    this.proc.stderr.on('data', (d) => this.emit('stderr', String(d).trim()));
    // Unexpected exit -> reject in-flight calls + emit "toast" event (UI would show it).
    this.proc.once('exit', (code, signal) => {
      for (const p of this.pending.values()) {
        p.reject(new Error(`plugin exited before responding (code=${code}, signal=${signal})`));
      }
      this.pending.clear();
      if (!this.#killing) {
        this.emit('toast', {
          kind: 'crash',
          message: `Плагин аварийно завершился (code=${code}, signal=${signal ?? '-'})`,
        });
      }
    });
    return this;
  }

  #spawnMs = () => 0;
  spawnMs() { return this.#spawnMs(); }

  call(method, params, { timeoutMs = this.callTimeoutMs } = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(Object.assign(new Error(`timeout ${timeoutMs}ms waiting for ${method} (id=${id})`), { code: 'ETIMEDOUT' }));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }

  #onLine(line) {
    if (!line.trim()) return;
    let msg;
    try { msg = JSON.parse(line); } catch {
      this.emit('toast', { kind: 'protocol', message: 'plugin sent malformed JSON' });
      return;
    }
    const p = this.pending.get(msg.id);
    if (!p) return; // late/duplicate response — ignore
    this.pending.delete(msg.id);
    if (msg.error) p.reject(new Error(`plugin error ${msg.error.code}: ${msg.error.message}`));
    else p.resolve(msg.result);
  }

  async kill(sig = 'SIGKILL') {
    if (!this.proc || this.proc.exitCode !== null) return this.exitInfo ?? null;
    this.#killing = true;
    this.proc.kill(sig); // SIGKILL -> TerminateProcess on Windows
    return this.exitPromise;
  }
}

function stats(msArr) {
  const s = [...msArr].sort((a, b) => a - b);
  const avg = s.reduce((a, b) => a + b, 0) / s.length;
  const p95 = s[Math.min(s.length - 1, Math.ceil(0.95 * s.length) - 1)];
  return { n: s.length, avg: round(avg), p95: round(p95), min: round(s[0]), max: round(s[s.length - 1]) };
}
const round = (x) => Math.round(x * 100) / 100;

// (a) Cold start: spawn -> initialize round-trip, 10 times.
async function testColdStart() {
  const N = 10;
  const times = [];
  for (let i = 0; i < N; i++) {
    const t0 = performance.now();
    const c = new PluginClient(PLUGIN).start();
    const res = await c.call('initialize', {}, { timeoutMs: 10_000 });
    times.push(performance.now() - t0);
    if (res?.ok !== true || !res.commands?.includes('demo')) throw new Error('bad initialize response');
    await c.kill();
    await c.exitPromise;
  }
  return stats(times);
}

// (b) Warm onQuery: 100 sequential requests into one live process.
async function testWarmQuery() {
  const N = 100;
  const c = new PluginClient(PLUGIN).start();
  await c.call('initialize', {}, { timeoutMs: 10_000 });
  const times = [];
  for (let i = 0; i < N; i++) {
    const t0 = performance.now();
    const res = await c.call('onQuery', { q: `test-${i}` });
    times.push(performance.now() - t0);
    if (!Array.isArray(res?.items) || res.items.length !== 3) throw new Error('bad onQuery response');
  }
  await c.kill();
  return stats(times);
}

// (c) Timeout + kill: crazy plugin ignores stdin; host waits 500ms, SIGKILLs, respawns fine.
async function testTimeoutKill() {
  let timedOut = false;
  const c = new PluginClient(CRAZY).start();
  try {
    await c.call('onQuery', { q: 'hang' }, { timeoutMs: 500 });
  } catch (e) {
    if (e.code === 'ETIMEDOUT') timedOut = true;
    else throw e;
  }
  if (!timedOut) throw new Error('crazy plugin unexpectedly answered');
  const t0 = performance.now();
  const exit = await c.kill('SIGKILL');
  const killMs = round(performance.now() - t0);
  // Host must still be alive and able to spawn a fresh plugin.
  const c2 = new PluginClient(PLUGIN).start();
  const res = await c2.call('initialize', {}, { timeoutMs: 10_000 });
  await c2.kill();
  return { timedOut, exit, killMs, hostAlive: true, respawnOk: res?.ok === true };
}

// (d) Crash: plugin exits(1) on initialize; host records crash, emits toast event, keeps working.
async function testCrash() {
  const toasts = [];
  const c = new PluginClient(CRASH).start();
  c.on('toast', (t) => toasts.push(t));
  let callFailed = false;
  try { await c.call('initialize', {}); } catch { callFailed = true; }
  await c.exitPromise; // wait for full close
  // Host continues: spawn a healthy plugin next.
  const c2 = new PluginClient(PLUGIN).start();
  const res = await c2.call('initialize', {}, { timeoutMs: 10_000 });
  await c2.kill();
  return {
    callFailed,
    toastEmitted: toasts.length === 1,
    toast: toasts[0]?.message ?? null,
    respawnOk: res?.ok === true,
  };
}

// ---- main ----
const results = {};
const t0 = performance.now();
results.coldStart = await testColdStart();
results.warmOnQuery = await testWarmQuery();
results.timeoutKill = await testTimeoutKill();
results.crash = await testCrash();
results.totalMs = round(performance.now() - t0);

const okTimeout = results.timeoutKill.timedOut && results.timeoutKill.respawnOk;
const okCrash = results.crash.callFailed && results.crash.toastEmitted && results.crash.respawnOk;
results.allPassed = okTimeout && okCrash;

console.log('\n===== SPIKE RESULTS: ext-host (JSON-RPC 2.0 over NDJSON stdio) =====');
console.log(`(a) cold start (spawn->initialize), n=${results.coldStart.n}:`);
console.log(`    avg=${results.coldStart.avg} ms  p95=${results.coldStart.p95} ms  min=${results.coldStart.min} ms  max=${results.coldStart.max} ms`);
console.log(`(b) warm onQuery (live process), n=${results.warmOnQuery.n}:`);
console.log(`    avg=${results.warmOnQuery.avg} ms  p95=${results.warmOnQuery.p95} ms  min=${results.warmOnQuery.min} ms  max=${results.warmOnQuery.max} ms`);
console.log(`(c) timeout+kill: timedOut=${results.timeoutKill.timedOut}, kill(SIGNKILL) took ${results.timeoutKill.killMs} ms, exit=${JSON.stringify(results.timeoutKill.exit)}, hostAlive=${results.timeoutKill.hostAlive}, respawnOk=${results.timeoutKill.respawnOk} -> ${okTimeout ? 'PASS' : 'FAIL'}`);
console.log(`(d) crash isolation: callFailed=${results.crash.callFailed}, toastEmitted=${results.crash.toastEmitted} ("${results.crash.toast}"), respawnOk=${results.crash.respawnOk} -> ${okCrash ? 'PASS' : 'FAIL'}`);
console.log(`total wall time: ${results.totalMs} ms`);
console.log(`ALL TESTS: ${results.allPassed ? 'PASSED' : 'FAILED'}`);
console.log('JSON ' + JSON.stringify(results));
process.exit(results.allPassed ? 0 : 1);
