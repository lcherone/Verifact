import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdir, chmod, writeFile, symlink, readdir } from 'node:fs/promises';
import path from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { requireValue, generateSource } from './recorder-ir.mjs';

const require = createRequire(import.meta.url);
const timestamp = () => new Date().toISOString();
const hash = value => createHash('sha256').update(value).digest('hex');
function privateReport(report) {
  const result = { stats: report.stats ?? {}, suites: [] };
  function suite(item) {
    return {
      title: String(item.title ?? '').slice(0, 200),
      suites: (item.suites ?? []).map(suite),
      specs: (item.specs ?? []).map(spec => ({
        title: String(spec.title ?? '').slice(0, 200),
        ok: spec.ok === true,
        tests: (spec.tests ?? []).map(test => ({
          status: test.status,
          expectedStatus: test.expectedStatus,
          results: (test.results ?? []).map(run => ({
            status: run.status,
            duration: run.duration,
            retry: run.retry,
            errors: (run.errors ?? []).map(() => ({ message: 'Recorded workflow failed; raw error content omitted for privacy' })),
          })),
        })),
      })),
    };
  }
  result.suites = (report.suites ?? []).map(suite);
  return result;
}
export async function verifyGenerated(worker, recording, timeoutMs = 60000) {
  requireValue(recording.state === 'completed', 'invalid_state', 'Stop recording before verifying generated source');
  requireValue(!worker.operation, 'busy', 'A browser replay or generated test is already running');
  const source = generateSource(recording);
  const sourceHash = hash(source);
  const assertions = recording.steps.filter(step => step.type === 'assertion').length;
  const id = randomUUID();
  const directory = path.join(worker.dataDir, 'generated-tests', id);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await chmod(path.dirname(directory), 0o700);
  const sourcePath = path.join(directory, 'recording.spec.mjs');
  const reportPath = path.join(directory, 'report.json');
  const packageDirectory = path.dirname(require.resolve('playwright/package.json'));
  await mkdir(path.join(directory, 'node_modules'), { mode: 0o700 });
  await symlink(packageDirectory, path.join(directory, 'node_modules', 'playwright'));
  await writeFile(sourcePath, source, { mode: 0o600 });
  const configPath = path.join(directory, 'playwright.config.mjs');
  await writeFile(configPath, `export default ${JSON.stringify({
    testDir: directory,
    testMatch: 'recording.spec.mjs',
    outputDir: path.join(directory, 'results'),
    reporter: 'json',
    workers: 1,
    retries: 0,
    timeout: timeoutMs,
    expect: { timeout: Math.max(100, Math.min(5000, Math.floor(timeoutMs / 3))) },
    globalTimeout: 90000,
    preserveOutput: 'failures-only',
    use: { browserName: 'chromium', headless: true, trace: 'off', screenshot: 'off', video: 'off' },
  })};\n`, { mode: 0o600 });
  const result = { runtime: { node: process.version, playwright: require('playwright/package.json').version }, id, kind: 'generated_test', recordingId: recording.id, revision: recording.revision, sourceHash, sourcePath, reportPath, assertions, isolated: true, startedAt: timestamp(), status: 'running' };
  const operation = { id, kind: 'generated_test', cancelled: false, child: null };
  worker.operation = operation;
  worker.notify('recording.replay', { id, kind: operation.kind, status: 'running', recordingId: recording.id });
  let output = '';
  let overflow = false;
  let timer;
  try {
    const child = spawn(process.execPath, [path.join(packageDirectory, 'cli.js'), 'test', '--config', configPath], {
      cwd: directory,
      env: { ...process.env, PLAYWRIGHT_JSON_OUTPUT_FILE: '', PLAYWRIGHT_JSON_OUTPUT_NAME: '', PLAYWRIGHT_JSON_OUTPUT_DIR: '', PLAYWRIGHT_NO_COPY_PROMPT: '1', FORCE_COLOR: '0' },
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: process.platform !== 'win32',
    });
    operation.child = child;
    child.stdout.on('data', chunk => {
      if (Buffer.byteLength(output) + chunk.length > 4 * 1024 * 1024) {
        overflow = true;
        child.kill('SIGINT');
      } else output += chunk;
    });
    // Child diagnostics may contain values from application code. Do not retain them.
    child.stderr.on('data', () => {});
    timer = setTimeout(() => { result.timedOut = true; worker.cancelReplay(); }, 95000);
    const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
    result.exitCode = code;
    let report;
    try { report = JSON.parse(output); } catch { report = { stats: {}, suites: [] }; }
    const sanitized = privateReport(report);
    const reportBytes = JSON.stringify(sanitized, null, 2);
    await writeFile(reportPath, reportBytes, { mode: 0o600 });
    result.reportHash = hash(reportBytes);
    result.evidence = [];
    async function collectImages(current, depth = 0) {
      if (depth > 5 || result.evidence.length >= 10) return;
      for (const entry of await readdir(current, { withFileTypes: true })) {
        if (entry.isSymbolicLink()) continue;
        const filename = path.join(current, entry.name);
        if (entry.isDirectory()) await collectImages(filename, depth + 1);
        else if (entry.name === 'failure.png') {
          await chmod(filename, 0o600);
          result.evidence.push({ path: filename, redacted: true, kind: 'screenshot' });
        }
      }
    }
    await collectImages(directory);
    if (!result.evidence.length) result.evidenceUnavailable = true;
    const expected = sanitized.stats.expected ?? 0;
    const unexpected = sanitized.stats.unexpected ?? 0;
    if (operation.cancelled) result.status = result.timedOut ? 'timed_out' : 'cancelled';
    else if (!overflow && code === 0 && expected === 1 && unexpected === 0) result.status = assertions > 0 ? 'passed' : 'unverified';
    else result.status = 'failed';
    if (result.status === 'failed') result.failure = { code: overflow ? 'report_limit' : 'generated_test_failed', message: 'Generated Playwright Test did not pass; inspect the private structured report' };
  } catch {
    result.status = operation.cancelled ? 'cancelled' : 'failed';
    result.failure = { code: 'infrastructure_error', message: 'Generated test runner could not complete' };
  } finally {
    clearTimeout(timer);
    if (operation.killTimer) clearTimeout(operation.killTimer);
    worker.operation = null;
  }
  result.finishedAt = timestamp();
  recording.generatedVerification = { status: result.status === 'passed' ? 'verified' : 'unverified', revision: recording.revision, sourceHash, executionId: id, assertions };
  recording.lastGeneratedRun = result;
  await worker.store.save(recording);
  if (worker.recording?.id === recording.id) worker.notify('recording.steps', { recording });
  worker.notify('recording.replay', { ...result });
  return result;
}
