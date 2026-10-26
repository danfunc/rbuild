export function parseArguments(args: readonly string[]): string[] {
  if (!args.length) throw new Error('rbuild.arguments must contain at least one argument (normally "build").');
  return [...args];
}

export function debugMapPatch(remoteRoot: string, localRoot: string): Record<string, string> {
  if (!remoteRoot || !localRoot) throw new Error('Both remote and local roots are required.');
  return { [remoteRoot]: localRoot };
}

export interface ParsedPackageOrigin {
  protocol_conformance?: { type_name: string };
}

export interface ParsedTargetSummary {
  name: string;
  kind: string;
  effective_path: string;
  dependencies?: string[];
  cxx_flags?: string[];
  linker_flags?: string[];
  output_format?: string;
}

export interface ParsedPackageEntry {
  manifest: {
    name: string;
    origin?: string | ParsedPackageOrigin;
    tools_version?: string;
    cxx_language_standard?: string;
  };
  resolved_plan: ParsedTargetSummary[];
}

/** Formats the JSON output of `rbuild parse-swift --json` into Output lines showing Package conformances and targets. */
export function formatPackagesSummary(raw: unknown): string[] {
  if (!raw || typeof raw !== 'object') return [];
  const data = raw as { parse_time_micros?: number; packages?: ParsedPackageEntry[]; manifest?: ParsedPackageEntry['manifest']; resolved_plan?: ParsedTargetSummary[] };
  const entries: ParsedPackageEntry[] = Array.isArray(data.packages) && data.packages.length > 0
    ? data.packages
    : (data.manifest && Array.isArray(data.resolved_plan) ? [{ manifest: data.manifest, resolved_plan: data.resolved_plan }] : []);
  const lines: string[] = [];
  for (const entry of entries) {
    const { manifest, resolved_plan } = entry;
    const originObj = typeof manifest.origin === 'object' && manifest.origin !== null ? manifest.origin : undefined;
    const originLabel = originObj?.protocol_conformance?.type_name
      ? `struct ${originObj.protocol_conformance.type_name}: Package`
      : 'Package(...)';
    const timeSuffix = typeof data.parse_time_micros === 'number' ? ` (${data.parse_time_micros}µs)` : '';
    lines.push(`[package] ${manifest.name} [${originLabel}]${timeSuffix}`);
    for (const target of resolved_plan) {
      const deps = target.dependencies?.length ? ` deps=[${target.dependencies.join(', ')}]` : '';
      const fmt = target.output_format ? ` format=${target.output_format}` : '';
      lines.push(`  - [${target.kind}] ${target.name} (${target.effective_path})${deps}${fmt}`);
    }
  }
  return lines;
}

// ---- Output formatting and progress (pure, unit-tested) ----

export type Stage = 'sync' | 'build' | 'fetch' | 'run';

export type RbuildEvent =
  | { kind: 'phase'; phase: string; state: string }
  | { kind: 'transfer'; phase: string; state: string; doneFiles: number; totalFiles?: number; doneBytes: number; totalBytes?: number }
  | { kind: 'job'; name: string; index: number; total: number };

/** Parses one machine-readable line emitted by `rbuild --progress=json`; undefined for anything else. */
export function parseProgressLine(line: string): RbuildEvent | undefined {
  if (!line.startsWith('{"event":"rbuild.')) return undefined;
  let value: Record<string, unknown>;
  try { value = JSON.parse(line); } catch { return undefined; }
  const num = (v: unknown): number | undefined => typeof v === 'number' ? v : undefined;
  if (value.event === 'rbuild.job' && typeof value.name === 'string') {
    return { kind: 'job', name: value.name, index: num(value.index) ?? 0, total: num(value.total) ?? 1 };
  }
  if (value.event === 'rbuild.progress' && typeof value.phase === 'string' && typeof value.state === 'string') {
    if (num(value.done_files) === undefined) return { kind: 'phase', phase: value.phase, state: value.state };
    return {
      kind: 'transfer', phase: value.phase, state: value.state,
      doneFiles: num(value.done_files) ?? 0, totalFiles: num(value.total_files),
      doneBytes: num(value.done_bytes) ?? 0, totalBytes: num(value.total_bytes),
    };
  }
  return undefined;
}

/** Estimates 0-100 build completion from a Bazel `[370 / 409]` or cargo `Building [==> ] 120/300` line. */
export function estimateBuildPercent(line: string): number | undefined {
  const bazel = /^\s*(?:\x1b\[[0-9;]*[A-Za-z])*\[([\d,]+) \/ ([\d,]+)\]/.exec(line);
  const cargo = /^\s*Building\s*\[[^\]]*\]\s*([\d,]+)\/([\d,]+)/.exec(line);
  const match = bazel ?? cargo;
  if (!match) return undefined;
  const done = Number(match[1].replace(/,/g, ''));
  const total = Number(match[2].replace(/,/g, ''));
  if (!(total > 0)) return undefined;
  return Math.max(0, Math.min(100, Math.floor(done * 100 / total)));
}

/** True for cargo's progress-bar redraws, which feed the estimate but add only noise to Output. */
export function isCargoProgressBar(line: string): boolean {
  return /^\s*Building\s*\[[^\]]*\]\s*[\d,]+\/[\d,]+/.test(line);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

export function prefixLine(stage: Stage, text: string): string {
  return `[${stage}] ${text}`;
}

export function jobHeader(name: string, index: number, total: number): string {
  return `[job:${name}] Started (${index + 1}/${total})`;
}

/** The closing line, e.g. `[build] Build finished with exit code 0 (12.3s)`. */
export function finishLine(stage: Stage, exitCode: number | null, elapsedMs: number): string {
  const label = stage === 'run' ? 'Run' : stage === 'sync' ? 'Sync' : 'Build';
  const code = exitCode === null ? 'none (could not start or was cancelled)' : String(exitCode);
  return prefixLine(stage, `${label} finished with exit code ${code} (${(elapsedMs / 1000).toFixed(1)}s)`);
}

/** Percent plus human text for a transfer event; percent is undefined while the total is unknown. */
export function describeTransfer(event: Extract<RbuildEvent, { kind: 'transfer' }>): { percent?: number; text: string } {
  const { doneFiles, totalFiles, doneBytes, totalBytes } = event;
  const files = totalFiles !== undefined ? `${doneFiles}/${totalFiles} files` : `${doneFiles} files`;
  const bytes = totalBytes !== undefined ? `${formatBytes(doneBytes)} / ${formatBytes(totalBytes)}` : formatBytes(doneBytes);
  let percent: number | undefined;
  if (totalBytes !== undefined && totalBytes > 0) percent = Math.floor(doneBytes * 100 / totalBytes);
  else if (totalFiles !== undefined && totalFiles > 0) percent = Math.floor(doneFiles * 100 / totalFiles);
  else if (event.state === 'done') percent = 100;
  return { percent, text: percent !== undefined ? `${files}, ${bytes} (${percent}%)` : `${files}, ${bytes}` };
}

/** Maps a stage-local percent onto a rough whole-run percent (sync 0-15, build 15-90, fetch 90-100). */
export function overallPercent(stage: Stage, percent: number | undefined, remote = true): number | undefined {
  if (percent === undefined) return undefined;
  if (!remote) return percent;
  const bands: Record<Stage, [number, number]> = { sync: [0, 15], build: [15, 90], fetch: [90, 100], run: [0, 100] };
  const [lo, hi] = bands[stage];
  return Math.round(lo + (hi - lo) * percent / 100);
}

export interface UiUpdate { output: string[]; stage: Stage; percent?: number; message?: string }

/**
 * Turns the child's stdout/stderr lines into CMake-style Output lines plus progress state.
 * Holds only the current stage, so it can be driven one line at a time.
 */
export class OutputFormatter {
  stage: Stage;
  private localJobs: Set<string>;
  constructor(initial: Stage, localJobs: Iterable<string> = []) { this.stage = initial; this.localJobs = new Set(localJobs); }

  line(raw: string): UiUpdate {
    const text = raw.replace(/\r$/, '');
    const event = parseProgressLine(text);
    if (event) return this.event(event);
    const percent = estimateBuildPercent(text);
    if (percent !== undefined) {
      return { output: isCargoProgressBar(text) ? [] : [prefixLine(this.stage, text)], stage: this.stage, percent: overallPercent(this.stage, percent), message: text.trim() };
    }
    if (text.trim() === '') return { output: [], stage: this.stage };
    if (/^rbuild: (Synchronizing|Sync complete)/.test(text)) this.stage = 'sync';
    else if (/^rbuild: (Collecting|Successfully collected|Cargo reported)/.test(text)) this.stage = 'fetch';
    else if (/^rbuild: (Executing remote|Handshake|Worktree locked|Connecting)/.test(text) && this.stage === 'sync') this.stage = 'build';
    return { output: [prefixLine(this.stage, text)], stage: this.stage };
  }

  private event(event: RbuildEvent): UiUpdate {
    if (event.kind === 'job') {
      this.stage = this.localJobs.has(event.name) ? 'run' : 'build';
      return { output: event.total > 1 ? [jobHeader(event.name, event.index, event.total)] : [], stage: this.stage };
    }
    if (event.kind === 'phase') {
      if (event.phase === 'build') this.stage = 'build';
      return { output: [], stage: this.stage, message: `${event.phase}` };
    }
    const stage: Stage = event.phase === 'collect' ? 'fetch' : 'sync';
    this.stage = stage;
    const { percent, text } = describeTransfer(event);
    const output: string[] = [];
    if (event.state === 'start' && stage === 'sync' && event.totalFiles === 0) output.push(prefixLine('sync', 'up to date'));
    else if (event.state === 'start' && stage === 'sync') output.push(prefixLine('sync', `Sending ${text}`));
    else if (event.state === 'done') {
      if (stage === 'sync') { if (event.totalFiles !== 0) output.push(prefixLine('sync', `Sent ${text}`)); }
      else if (event.doneFiles > 0) output.push(prefixLine('fetch', `Received ${text}`));
      this.stage = 'build';
    }
    return { output, stage, percent: overallPercent(stage, percent), message: `${stage} ${text}` };
  }
}

/** Splits streamed chunks into complete lines, treating `\n` and `\r` as separators. */
export class LineSplitter {
  private rest = '';
  push(chunk: string): string[] {
    const parts = (this.rest + chunk).split(/\r\n|\n|\r/);
    this.rest = parts.pop() ?? '';
    return parts;
  }
  flush(): string[] { const r = this.rest; this.rest = ''; return r ? [r] : []; }
}
