import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const executable = join(root, 'target/debug/verifact-cli');
const receiptPath = join(root, 'evidence/core-verification.json');
const sourceFiles = [
  'Cargo.lock',
  'crates/verifact-cli/Cargo.toml',
  'crates/verifact-cli/src/main.rs',
  'crates/verifact-core/Cargo.toml',
  'crates/verifact-core/src/lib.rs',
  'crates/verifact-core/src/model.rs',
  'crates/verifact-core/src/scanner.rs',
  'crates/verifact-core/src/service.rs',
  'crates/verifact-core/src/store.rs',
  'worker/verify-core.mjs',
];
const report = {
  schema_version: 1,
  product: 'Verifact',
  kind: 'core_cli_acceptance',
  started_at: new Date().toISOString(),
  status: 'running',
  environment: { node: process.version, platform: process.platform, architecture: process.arch },
  checks: [],
  commands: [],
  fixtures: [],
};
let temporaryRoot;
let dataDirectory;

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

async function sourceHashes() {
  return Object.fromEntries(await Promise.all(sourceFiles.map(async file => [
    file,
    sha256(await readFile(join(root, file))),
  ])));
}

async function command(args, expectedExit = 0) {
  const argv = ['--data-dir', dataDirectory, ...args];
  const startedAt = Date.now();
  const receipt = { argv, expected_exit: expectedExit, status: 'running' };
  report.commands.push(receipt);
  const result = await new Promise((resolve, reject) => {
    const child = spawn(executable, argv, {
      cwd: root,
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: 10000,
      killSignal: 'SIGKILL',
    });
    let stdout = '';
    let stderr = '';
    let bytes = 0;
    let exceededLimit = false;
    const collect = (target, chunk) => {
      bytes += chunk.length;
      if (bytes > 4 * 1024 * 1024) {
        exceededLimit = true;
        child.kill('SIGKILL');
        return;
      }
      if (target === 'stdout') stdout += chunk;
      else stderr += chunk;
    };
    child.stdout.on('data', chunk => collect('stdout', chunk));
    child.stderr.on('data', chunk => collect('stderr', chunk));
    child.once('error', reject);
    child.once('close', (exitCode, signal) => resolve({ stdout, stderr, exitCode, signal, exceededLimit }));
  }).catch(error => {
    receipt.status = 'failed';
    receipt.error = error.message;
    throw error;
  });
  Object.assign(receipt, {
    exit_code: result.exitCode,
    signal: result.signal,
    duration_ms: Date.now() - startedAt,
    stdout_sha256: sha256(result.stdout),
    stderr: result.stderr,
  });
  try {
    assert.equal(result.exceededLimit, false, `CLI output exceeded the bounded receipt limit: ${args.join(' ')}`);
    assert.equal(result.signal, null, `CLI command exceeded its timeout or was interrupted: ${args.join(' ')}`);
    assert.equal(result.exitCode, expectedExit, `Unexpected CLI result for ${args.join(' ')}\n${result.stderr}`);
  } catch (error) {
    receipt.status = 'failed';
    receipt.error = error.message;
    throw error;
  }
  receipt.status = 'passed';
  return result;
}

async function jsonCommand(args) {
  const result = await command(args);
  return JSON.parse(result.stdout);
}

function passed(name) {
  report.checks.push({ name, status: 'passed' });
  console.log(`Passed: ${name}`);
}

function assertPassiveScan(scan, expectedCount) {
  assert.equal(scan.candidates.length, expectedCount);
  const frameworks = new Map(scan.frameworks.map(framework => [framework.id, framework]));
  assert.equal(frameworks.size, scan.frameworks.length);
  const candidateIds = new Set();
  for (const framework of scan.frameworks) {
    assert.ok(framework.evidence.length > 0, 'Every framework detection must explain its evidence');
  }
  for (const candidate of scan.candidates) {
    assert.equal(candidate.discovery_kind, 'file_candidate');
    assert.equal(isAbsolute(candidate.file), false);
    assert.ok(frameworks.has(candidate.framework_id));
    candidateIds.add(candidate.id);
  }
  assert.equal(candidateIds.size, expectedCount, 'Candidate IDs must be unique within a scan');
}

await mkdir(dirname(receiptPath), { recursive: true });
try {
  temporaryRoot = await mkdtemp(join(tmpdir(), 'verifact-core-acceptance-'));
  dataDirectory = join(temporaryRoot, 'data');
  const initialSources = await sourceHashes();
  const initialBinaryHash = sha256(await readFile(executable));
  const manifest = await readFile(join(root, 'crates/verifact-cli/Cargo.toml'), 'utf8');
  report.cli = {
    executable,
    declared_package_version: manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? null,
    executable_sha256: initialBinaryHash,
  };
  report.source_sha256 = initialSources;
  report.isolated_data_directory = dataDirectory;

  const help = await command(['--help']);
  assert.match(help.stdout, /Verifact/);
  assert.match(help.stdout, /candidates are not confirmed individual test cases/);
  passed('CLI describes passive candidate discovery honestly');

  const fixtures = [
    ['phpunit-basic', ['PHPUnit'], 1],
    ['phpunit-legacy', ['PHPUnit'], 1],
    ['pest', ['Pest'], 1],
    ['vitest', ['Vitest'], 1],
    ['jest', ['Jest'], 1],
    ['playwright', ['Playwright'], 1],
    ['mixed', ['PHPUnit', 'Vitest'], 2],
    ['monorepo', ['Jest', 'Vitest'], 2],
    ['no-tests', [], 0],
    ['malformed', [], 0],
    ['codeception', ['Codeception'], 1],
    ['cypress', ['Cypress'], 1],
    ['mocha', ['Mocha'], 1],
    ['node-test', ['Node test'], 1],
  ];
  const registered = new Map();
  for (const [fixture, expectedFrameworks, expectedCount] of fixtures) {
    const result = await jsonCommand(['add', join(root, 'fixtures/discovery', fixture)]);
    assert.equal(result.project.path, await realpath(join(root, 'fixtures/discovery', fixture)));
    assert.equal(result.scan.project_id, result.project.id);
    assertPassiveScan(result.scan, expectedCount);
    assert.deepEqual(result.scan.frameworks.map(framework => framework.name).sort(), expectedFrameworks.toSorted());
    if (fixture === 'malformed') {
      assert.equal(result.scan.warnings.length, 2);
      assert.deepEqual(result.scan.warnings.map(warning => warning.path).sort(), ['composer.json', 'package.json']);
    } else {
      assert.equal(result.scan.warnings.length, 0, `Unexpected warning in ${fixture}`);
    }
    registered.set(fixture, result);
    report.fixtures.push({ name: fixture, ...result });
  }
  passed('Fourteen named PHP/Node fixtures produce expected frameworks and candidate-file counts');

  const projects = await jsonCommand(['projects']);
  assert.equal(projects.length, fixtures.length);
  assert.deepEqual(new Set(projects.map(project => project.id)), new Set([...registered.values()].map(result => result.project.id)));
  const mixed = registered.get('mixed');
  assert.deepEqual(await jsonCommand(['project', mixed.project.id]), mixed.project);
  assert.deepEqual(await jsonCommand(['tests', mixed.project.id]), mixed.scan);
  passed('Separate CLI processes reopen the same persisted project registry and catalogue');

  const rescanned = await jsonCommand(['scan', mixed.project.id]);
  assertPassiveScan(rescanned, 2);
  assert.deepEqual(rescanned.candidates.map(candidate => candidate.id), mixed.scan.candidates.map(candidate => candidate.id));
  const history = await jsonCommand(['history', mixed.project.id]);
  assert.equal(history.length, 2);
  assert.deepEqual(history[0], rescanned);
  assert.deepEqual(history[1], mixed.scan);
  const jobs = await jsonCommand(['jobs', mixed.project.id]);
  assert.equal(jobs.length, 2);
  assert.ok(jobs.every(job => job.status === 'completed' && job.finished_at));
  report.rescan = { project_id: mixed.project.id, history, jobs };
  passed('Rescanning preserves candidate identities, appends scan history and finalizes jobs');

  const baitProject = join(temporaryRoot, 'passive-project');
  await mkdir(join(baitProject, 'tests'), { recursive: true });
  await writeFile(join(baitProject, 'package.json'), JSON.stringify({
    name: 'verifact-passive-acceptance',
    scripts: { test: 'node bait.cjs' },
    devDependencies: { vitest: '^3.0.0' },
  }));
  await writeFile(join(baitProject, 'bait.cjs'), "require('node:fs').writeFileSync('script-executed', 'unexpected');\n");
  await writeFile(join(baitProject, 'vitest.config.cjs'), "require('node:fs').writeFileSync('config-executed', 'unexpected'); module.exports = {};\n");
  await writeFile(join(baitProject, 'tests/passive.test.js'), "import { test } from 'vitest'; test('passive', () => {});\n");
  const bait = await jsonCommand(['add', baitProject]);
  assertPassiveScan(bait.scan, 1);
  for (const marker of ['script-executed', 'config-executed']) {
    await assert.rejects(readFile(join(baitProject, marker)), { code: 'ENOENT' });
  }
  report.passive_safety = { scan: bait.scan, script_marker_absent: true, configuration_marker_absent: true };
  passed('Importing a project does not execute its scripts or executable runner configuration');

  const beforeRejectedImports = await jsonCommand(['projects']);
  const file = join(temporaryRoot, 'not-a-directory');
  await writeFile(file, 'A file cannot be imported as a project.\n');
  for (const args of [
    ['add', join(temporaryRoot, 'missing-project')],
    ['add', file],
    ['project', 'unknown-project-id'],
    ['tests', 'unknown-project-id'],
    ['scan', 'unknown-project-id'],
  ]) {
    const rejected = await command(args, 1);
    assert.match(rejected.stderr, /Verifact:/);
    assert.equal(rejected.stdout, '');
  }
  assert.deepEqual(await jsonCommand(['projects']), beforeRejectedImports);
  passed('Missing directories, files and unknown identities fail with exit 1 without corrupting the registry');

  assert.equal(sha256(await readFile(executable)), initialBinaryHash, 'CLI binary changed during acceptance; rebuild and rerun');
  assert.deepEqual(await sourceHashes(), initialSources, 'Core/CLI source changed during acceptance; rebuild and rerun');
  passed('CLI binary and relevant source stayed unchanged throughout the acceptance run');
  report.status = 'passed';
} catch (error) {
  report.status = 'failed';
  report.error = error.stack ?? String(error);
  console.error(report.error);
  process.exitCode = 1;
} finally {
  if (temporaryRoot) {
    try {
      await rm(temporaryRoot, { recursive: true, force: true });
      report.cleanup = { status: 'passed', isolated_temporary_data_removed: true };
    } catch (error) {
      report.cleanup = { status: 'failed', error: error.message };
      report.status = 'failed';
      process.exitCode = 1;
    }
  }
  report.finished_at = new Date().toISOString();
  await writeFile(receiptPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`Core acceptance ${report.status}: ${receiptPath}`);
}
