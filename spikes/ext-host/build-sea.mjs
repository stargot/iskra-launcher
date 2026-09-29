// build-sea.mjs — Node 24 SEA spike: bundle host runtime into a single .exe, measure size.
// Fallback: if postject/signtool unavailable or injection fails, report node.exe size as upper bound.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.join(HERE, 'sea-out');
fs.mkdirSync(OUT, { recursive: true });

const report = { steps: [], ok: false, exeSizeBytes: null, nodeExeSizeBytes: fs.statSync(process.execPath).size };
const mb = (b) => (b / 1024 / 1024).toFixed(2) + ' MB';

function run(name, cmd, args, { useShell = false } = {}) {
  const r = useShell
    ? spawnSync(cmd + ' ' + args.join(' '), { cwd: HERE, shell: true, encoding: 'utf8', timeout: 120_000 })
    : spawnSync(cmd, args, { cwd: HERE, encoding: 'utf8', timeout: 120_000 });
  const out = ((r.stdout ?? '') + (r.stderr ?? '')).trim();
  report.steps.push({ name, status: r.status, error: r.error?.code ?? null, output: out.slice(-800) });
  return { status: r.status, out, enoent: r.error?.code === 'ENOENT' };
}

// 1) blob: node --experimental-sea-config sea-config.json (fallback: without flag, for future Node)
let s1 = run('sea-config (flagged)', process.execPath, ['--experimental-sea-config', 'sea-config.json']);
if (s1.status !== 0) s1 = run('sea-config (unflagged)', process.execPath, ['sea-config.json', '--experimental-sea-config']);
const blobOk = s1.status === 0 || fs.existsSync(path.join(OUT, 'ext-host.blob'));

if (blobOk) {
  // 2) copy node.exe -> ext-host.exe
  const exe = path.join(OUT, 'ext-host.exe');
  fs.copyFileSync(process.execPath, exe);
  report.steps.push({ name: 'copy node.exe', status: 0, sizeBefore: fs.statSync(exe).size });

  // 3) optional: strip signature (signtool). May be absent — that's fine, note it.
  const sig = run('signtool check', 'signtool', ['remove', '/s', exe]);
  const signtoolAvailable = !sig.enoent;
  report.signtoolAvailable = signtoolAvailable;
  if (!signtoolAvailable) report.steps.push({ name: 'signtool', status: 'absent', note: 'signtool не найден в PATH — пропускаем снятие подписи, пробуем postject напрямую' });

  // 4) inject blob
  const inj = run(
    'postject inject',
    'npx', ['--yes', 'postject', exe, 'NODE_SEA_BLOB', path.join(OUT, 'ext-host.blob'),
            '--sentinel-fuse', 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2'],
    { useShell: true },
  );

  if (inj.status === 0) {
    report.exeSizeBytes = fs.statSync(exe).size;
    // 5) verify the fused binary actually runs
    const ver = spawnSync(exe, { encoding: 'utf8', timeout: 15_000 });
    report.runsOk = ver.status === 0 && (ver.stdout ?? '').includes('SEA OK');
    report.runOutput = ((ver.stdout ?? '') + (ver.stderr ?? '')).trim().slice(0, 200);
  } else {
    report.injectFailed = true;
  }
}

// ---- summary ----
console.log('===== SEA BUILD REPORT =====');
for (const st of report.steps) console.log(`- ${st.name}: status=${st.status}${st.note ? ' — ' + st.note : ''}`);
if (report.runOutput) console.log(`- run check: ${JSON.stringify(report.runOutput)}`);
if (report.exeSizeBytes != null) {
  console.log(`ext-host.exe (SEA): ${mb(report.exeSizeBytes)} (${report.exeSizeBytes} bytes), runs=${report.runsOk}`);
} else {
  console.log(`SEA build FAILED (см. шаги выше). Верхняя граница — размер node.exe: ${mb(report.nodeExeSizeBytes)} (${report.nodeExeSizeBytes} bytes)`);
}
console.log('JSON ' + JSON.stringify(report));
