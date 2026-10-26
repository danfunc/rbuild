import * as vscode from 'vscode';
import { spawn, ChildProcess } from 'node:child_process';
import * as path from 'node:path';
import { debugMapPatch, parseArguments, formatPackagesSummary, OutputFormatter, LineSplitter, finishLine, Stage } from './helpers';

let running: ChildProcess | undefined;
let output: vscode.OutputChannel;
let progressItem: vscode.StatusBarItem;

export function activate(context: vscode.ExtensionContext): void {
  output = vscode.window.createOutputChannel('rbuild');
  context.subscriptions.push(output);
  const buildButton = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 20);
  buildButton.text = '$(tools) rbuild'; buildButton.tooltip = 'Run rbuild build'; buildButton.command = 'rbuild.build'; buildButton.show();
  const debugButton = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 19);
  debugButton.text = '$(debug) rbuild'; debugButton.tooltip = 'Build with rbuild and start debugger'; debugButton.command = 'rbuild.debug'; debugButton.show();
  const runButton = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 18);
  runButton.text = '$(play) rbuild'; runButton.tooltip = 'Run rbuild job run'; runButton.command = 'rbuild.run'; runButton.show();
  const packagesButton = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 17);
  packagesButton.text = '$(package) Packages'; packagesButton.tooltip = 'Inspect Package-conforming manifests with rbuild'; packagesButton.command = 'rbuild.packages'; packagesButton.show();
  progressItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 16);
  progressItem.command = 'rbuild.cancel';
  progressItem.tooltip = 'rbuild is running; click to cancel';
  context.subscriptions.push(progressItem, buildButton, debugButton, runButton, packagesButton,
    vscode.commands.registerCommand('rbuild.build', () => runConfigured('build')),
    vscode.commands.registerCommand('rbuild.run', () => runConfigured('run')),
    vscode.commands.registerCommand('rbuild.debug', () => runConfigured('build', true)),
    vscode.commands.registerCommand('rbuild.packages', () => showPackages()),
    vscode.commands.registerCommand('rbuild.cancel', () => { running?.kill(); }));
}

async function showPackages(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) { vscode.window.showErrorMessage('Open an rbuild workspace first.'); return; }
  const config = vscode.workspace.getConfiguration('rbuild', folder.uri);
  const executable = config.get<string>('executable', 'rbuild');
  const activeFile = vscode.window.activeTextEditor?.document.uri.fsPath;
  const manifestPath = activeFile && activeFile.endsWith('.swift') ? activeFile : 'Package.swift';
  return new Promise<void>(resolve => {
    const child = spawn(executable, ['parse-swift', manifestPath, '--json'], { cwd: folder.uri.fsPath, windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout?.on('data', chunk => stdout += chunk.toString());
    child.stderr?.on('data', chunk => stderr += chunk.toString());
    child.on('error', err => { vscode.window.showErrorMessage(`Could not run rbuild: ${err.message}`); resolve(); });
    child.on('close', code => {
      if (code !== 0) {
        vscode.window.showErrorMessage(stderr.trim() || `rbuild parse-swift exited with ${code}`);
        return resolve();
      }
      try {
        const parsed = JSON.parse(stdout);
        const lines = formatPackagesSummary(parsed);
        output.clear();
        for (const line of lines) output.appendLine(line);
        output.show(true);
      } catch (err) {
        vscode.window.showErrorMessage(`Failed to parse package output: ${String(err)}`);
      }
      resolve();
    });
  });
}

async function runConfigured(job: 'build' | 'run', debug = false): Promise<void> {
  if (running) { vscode.window.showInformationMessage('An rbuild process is already running.'); return; }
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) { vscode.window.showErrorMessage('Open an rbuild workspace first.'); return; }
  const config = vscode.workspace.getConfiguration('rbuild', folder.uri);
  const executable = config.get<string>('executable', 'rbuild');
  const jobs = await projectJobs(executable, folder.uri.fsPath);
  const hasJob = jobs !== undefined && Object.prototype.hasOwnProperty.call(jobs, job);
  // --progress must precede every other argument: rbuild treats everything after the first positional as a build argument.
  const args = ['--progress=json'];
  const backend = config.get<string>('backend', '');
  if (hasJob) args.push('job', job);
  else {
    if (backend) args.push(`--backend=${backend}`);
    args.push(...parseArguments(config.get<string[]>('arguments', ['build'])));
  }
  const finalStage: Stage = job === 'run' ? 'run' : 'build';
  const localJobs = Object.entries(jobs ?? {}).filter(([, j]) => j.worker === 'local').map(([name]) => name);
  const formatter = new OutputFormatter(finalStage, localJobs);
  output.clear();
  output.appendLine(`$ ${executable} ${args.filter(a => a !== '--progress=json').map(a => JSON.stringify(a)).join(' ')}`);
  output.show(true);
  const started = Date.now();
  let reported = 0;
  const code = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `rbuild ${job}`, cancellable: true },
    (progress, token) => new Promise<number | null>((resolve) => {
      const child = spawn(executable, args, { cwd: folder.uri.fsPath, windowsHide: true });
      running = child;
      token.onCancellationRequested(() => child.kill());
      progressItem.text = `$(sync~spin) rbuild: ${finalStage}`;
      progressItem.show();
      const handle = (line: string) => {
        const update = formatter.line(line);
        for (const text of update.output) output.appendLine(text);
        if (update.percent !== undefined && update.percent > reported) {
          progress.report({ increment: update.percent - reported });
          reported = update.percent;
        }
        const label = update.message ?? update.stage;
        const percent = update.percent !== undefined ? ` ${update.percent}%` : reported > 0 ? ` ${reported}%` : '';
        progress.report({ message: `[${update.stage}] ${label}${percent}` });
        progressItem.text = `$(sync~spin) rbuild: [${update.stage}]${percent}`;
      };
      const splitters = [new LineSplitter(), new LineSplitter()];
      [child.stdout, child.stderr].forEach((stream, i) => stream?.on('data', chunk => splitters[i].push(chunk.toString()).forEach(handle)));
      child.on('error', error => { output.appendLine(`\nrbuild launch error: ${error.message}`); resolve(null); });
      child.on('close', code => { splitters.forEach(sp => sp.flush().forEach(handle)); resolve(code); });
    }));
  running = undefined;
  progressItem.hide();
  output.appendLine(finishLine(formatter.stage === 'sync' || formatter.stage === 'fetch' ? finalStage : formatter.stage, code, Date.now() - started));
  if (code !== 0) { vscode.window.showErrorMessage(code === null ? 'Could not start rbuild.' : `rbuild failed (exit ${code ?? 'signal'}).`); return; }
  vscode.window.showInformationMessage(`rbuild ${job} completed successfully.`);
  if (debug) await launchConfiguredDebug(folder, config);
}

async function projectJobs(executable: string, cwd: string): Promise<Record<string, { worker?: string }> | undefined> {
  return new Promise(resolve => {
    const child = spawn(executable, ['jobs', '--json'], { cwd, windowsHide: true });
    let stdout = '';
    child.stdout?.on('data', chunk => stdout += chunk.toString());
    child.on('error', () => resolve(undefined));
    child.on('close', code => {
      if (code !== 0) return resolve(undefined);
      try { resolve(JSON.parse(stdout)); }
      catch { resolve(undefined); }
    });
  });
}

async function launchConfiguredDebug(folder: vscode.WorkspaceFolder, config: vscode.WorkspaceConfiguration): Promise<void> {
  const name = config.get<string>('debug.launchConfiguration', '').trim();
  if (!name) { vscode.window.showErrorMessage('Set rbuild.debug.launchConfiguration to an existing launch configuration name.'); return; }
  const launch = vscode.workspace.getConfiguration('launch', folder.uri);
  const configurations = launch.get<Array<Record<string, unknown>>>('configurations', []);
  const selected = configurations.find(c => c.name === name);
  if (!selected) { vscode.window.showErrorMessage(`Launch configuration not found: ${name}`); return; }
  const mapPath = path.join(folder.uri.fsPath, '.rbuild-out', 'rbuild-debug-map.json');
  try {
    const bytes = await vscode.workspace.fs.readFile(vscode.Uri.file(mapPath));
    const map = JSON.parse(Buffer.from(bytes).toString('utf8')) as { remote_root?: string; local_root?: string };
    const property = config.get<string>('debug.sourceMapProperty', 'sourceFileMap');
    if (property !== 'none' && map.remote_root && map.local_root) {
      const substitutions = { ...(selected[property] as Record<string, string> | undefined), ...debugMapPatch(map.remote_root, map.local_root) };
      const updated = { ...selected, [property]: substitutions };
      const all = configurations.map(c => c.name === name ? updated : c);
      await launch.update('configurations', all, vscode.ConfigurationTarget.WorkspaceFolder);
    }
  } catch (error) {
    output.appendLine(`Could not apply rbuild debug map: ${String(error)}`);
    vscode.window.showWarningMessage('Could not read .rbuild-out/rbuild-debug-map.json; starting the configured debugger without an injected path map.');
  }
  const started = await vscode.debug.startDebugging(folder, name);
  if (!started) vscode.window.showErrorMessage(`VS Code could not start debug configuration: ${name}`);
}

export function deactivate(): void { running?.kill(); }
