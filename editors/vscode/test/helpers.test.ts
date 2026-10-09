import test from 'node:test';
import assert from 'node:assert/strict';
import { parseArguments, debugMapPatch } from '../src/helpers';

test('legacy arguments remain available when projects have no jobs', () => {
  assert.deepEqual(parseArguments(['build', '//demo:app']), ['build', '//demo:app']);
});

test('debug source map maps the remote root to the local root', () => {
  assert.deepEqual(debugMapPatch('/srv/build/u/repo/main', '/work/repo'), {
    '/srv/build/u/repo/main': '/work/repo',
  });
});

import { parseProgressLine, estimateBuildPercent, isCargoProgressBar, formatBytes, prefixLine, finishLine, describeTransfer, overallPercent, OutputFormatter, LineSplitter } from '../src/helpers';

const sync = (state: string, df: number, tf: number, db: number, tb: number) =>
  JSON.stringify({ event: 'rbuild.progress', phase: 'sync', state, done_files: df, total_files: tf, done_bytes: db, total_bytes: tb });

test('parseProgressLine reads transfer, phase and job lines and ignores build output', () => {
  assert.deepEqual(parseProgressLine(sync('progress', 3, 10, 300, 1000)),
    { kind: 'transfer', phase: 'sync', state: 'progress', doneFiles: 3, totalFiles: 10, doneBytes: 300, totalBytes: 1000 });
  assert.deepEqual(parseProgressLine('{"event":"rbuild.progress","phase":"build","state":"start"}'), { kind: 'phase', phase: 'build', state: 'start' });
  assert.deepEqual(parseProgressLine('{"event":"rbuild.job","index":1,"name":"uf2","total":3}'), { kind: 'job', name: 'uf2', index: 1, total: 3 });
  assert.equal(parseProgressLine('{"unrelated":true}'), undefined);
  assert.equal(parseProgressLine('{"event":"rbuild.progress", broken'), undefined);
  assert.equal(parseProgressLine('[370 / 409] compiling'), undefined);
});

test('estimateBuildPercent handles Bazel and cargo progress, else undefined', () => {
  assert.equal(estimateBuildPercent('[370 / 409] 3 actions running'), 90);
  assert.equal(estimateBuildPercent('\x1b[32m[1,000 / 4,000]\x1b[0m Compiling x'), 25);
  assert.equal(estimateBuildPercent('    Building [=====>        ] 120/300: foo'), 40);
  assert.equal(estimateBuildPercent('[0 / 0] nothing'), undefined);
  assert.equal(estimateBuildPercent('error[E0001]: nope'), undefined);
  assert.equal(isCargoProgressBar('   Building [==>   ] 1/3'), true);
  assert.equal(isCargoProgressBar('[1 / 3] x'), false);
});

test('prefixes, byte units and the finish line follow the CMake style', () => {
  assert.equal(prefixLine('fetch', 'x'), '[fetch] x');
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(1536), '1.5 KiB');
  assert.equal(finishLine('build', 0, 12345), '[build] Build finished with exit code 0 (12.3s)');
  assert.equal(finishLine('run', 2, 500), '[run] Run finished with exit code 2 (0.5s)');
  assert.match(finishLine('build', null, 1000), /exit code none/);
});

test('transfer text shows files, bytes and percent; unknown totals show no percent', () => {
  const t = parseProgressLine(sync('progress', 3, 10, 512, 2048));
  assert.ok(t && t.kind === 'transfer');
  assert.deepEqual(describeTransfer(t), { percent: 25, text: '3/10 files, 512 B / 2.0 KiB (25%)' });
  const c = parseProgressLine('{"event":"rbuild.progress","phase":"collect","state":"progress","done_files":2,"done_bytes":2048}');
  assert.ok(c && c.kind === 'transfer');
  assert.deepEqual(describeTransfer(c), { percent: undefined, text: '2 files, 2.0 KiB' });
  assert.equal(overallPercent('sync', 100), 15);
  assert.equal(overallPercent('build', 50), 53);
  assert.equal(overallPercent('build', undefined), undefined);
});

test('OutputFormatter labels stages, reports up to date, and multi-stage job headers', () => {
  const f = new OutputFormatter('build', ['uf2', 'run']);
  assert.deepEqual(f.line('{"event":"rbuild.job","index":0,"name":"build","total":3}').output, ['[job:build] Started (1/3)']);
  assert.deepEqual(f.line('rbuild: Synchronizing workspace...').output, ['[sync] rbuild: Synchronizing workspace...']);
  assert.deepEqual(f.line(sync('start', 0, 0, 0, 0)).output, ['[sync] up to date']);
  assert.deepEqual(f.line(sync('done', 0, 0, 0, 0)).output, []);
  f.line('{"event":"rbuild.progress","phase":"build","state":"start"}');
  const bazel = f.line('[370 / 409] compiling');
  assert.deepEqual(bazel.output, ['[build] [370 / 409] compiling']);
  assert.equal(bazel.percent, overallPercent('build', 90));
  assert.deepEqual(f.line('   Building [==>  ] 120/300').output, []);
  const start = f.line(sync('start', 0, 4, 0, 4096));
  assert.deepEqual(start.output, ['[sync] Sending 0/4 files, 0 B / 4.0 KiB (0%)']);
  assert.deepEqual(f.line(sync('done', 4, 4, 4096, 4096)).output, ['[sync] Sent 4/4 files, 4.0 KiB / 4.0 KiB (100%)']);
  const collect = f.line('{"event":"rbuild.progress","phase":"collect","state":"done","done_files":2,"total_files":2,"done_bytes":2048,"total_bytes":2048}');
  assert.deepEqual(collect.output, ['[fetch] Received 2/2 files, 2.0 KiB / 2.0 KiB (100%)']);
  f.line('{"event":"rbuild.job","index":1,"name":"uf2","total":3}');
  assert.deepEqual(f.line('converted').output, ['[run] converted']);
});

test('LineSplitter joins chunks and treats carriage returns as separators', () => {
  const s = new LineSplitter();
  assert.deepEqual(s.push('a\nb'), ['a']);
  assert.deepEqual(s.push('c\rd\r\ne'), ['bc', 'd']);
  assert.deepEqual(s.flush(), ['e']);
  assert.deepEqual(s.flush(), []);
});
