// Backend giả để xem giao diện trong trình duyệt: mở http://localhost:1420/?mock
// Chỉ được nạp ở bản dev (xem main.ts); bản phát hành không chứa tệp này.

import { emit } from '@tauri-apps/api/event';
import { mockIPC } from '@tauri-apps/api/mocks';
import { MODULES, type Finding, type FixReport, type ModuleId, type RescueRecord, type ScanReport } from '../lib/scan';
import type { AppInfo, BatchUninstallReport, Leftover, RemoveReport, UninstallMode, UninstallPreparation, UninstallResult,
  DuplicateGroup, DuplicateScan, DuplicateRemoval, ComponentAnalysis, ComponentStatus, Tweak, TweakReport,
  BrowserCache, BrowserCacheScan, BrowserCacheReport, MonitorSummary, MonitorReport, MonitorChange,
  BrowserExtension, ExtensionScan, ExtensionRemoval, WindowsUpdate, UpdateResult } from '../lib/tools';

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function finding(module: ModuleId, index: number, kind: string, extra: Partial<Finding> = {}): Finding {
  return {
    id: `${module}:${index}`,
    module,
    kind: `${module}.${kind}`,
    name: null,
    detail: null,
    count: 1,
    bytes: 0,
    recommended: true,
    undoable: false,
    needsAdmin: false,
    ...extra,
  };
}

const sample: Record<ModuleId, Finding[]> = {
  junk: [
    finding('junk', 0, 'userTemp', { count: 10866, bytes: 6_529_000_000, detail: 'C:\\Users\\demo\\AppData\\Local\\Temp' }),
    finding('junk', 1, 'windowsTemp', { count: 39, bytes: 8_500_000, detail: 'C:\\Windows\\Temp', needsAdmin: true }),
    finding('junk', 2, 'browserCache', { count: 1264, bytes: 620_000_000, detail: 'C:\\Users\\demo\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache' }),
    finding('junk', 3, 'recycleBin', { count: 12, bytes: 150_000_000, recommended: false }),
  ],
  privacy: [
    finding('privacy', 0, 'recentDocs', { count: 8, bytes: 9000, detail: 'C:\\Users\\demo\\AppData\\Roaming\\Microsoft\\Windows\\Recent' }),
    finding('privacy', 1, 'typedPaths', { count: 3, detail: 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\TypedPaths' }),
    finding('privacy', 2, 'browserCookies', { count: 16, bytes: 2_900_000, recommended: false }),
  ],
  shortcut: [
    finding('shortcut', 0, 'broken', { name: 'Old Game', detail: 'D:\\Games\\Old Game\\game.exe', undoable: true }),
  ],
  startup: [
    finding('startup', 0, 'broken', { name: 'RemovedTool', detail: '"C:\\Program Files\\RemovedTool\\tray.exe" /auto', undoable: true }),
    finding('startup', 1, 'entry', { name: 'Chat App', detail: '"C:\\Users\\demo\\AppData\\Local\\Programs\\Chat\\Chat.exe"', undoable: true, recommended: false }),
    finding('startup', 2, 'entry', { name: 'Tablet Driver', detail: 'C:\\Program Files\\Tablet\\Tablet.exe', undoable: true, recommended: false, needsAdmin: true }),
  ],
  registry: [
    finding('registry', 0, 'muiCache', { count: 11, undoable: true, detail: 'HKCU\\Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\Shell\\MuiCache' }),
    finding('registry', 1, 'sharedDlls', { count: 4, undoable: true, needsAdmin: true, detail: 'HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\SharedDLLs' }),
  ],
  system: [
    finding('system', 0, 'startupDelay', { undoable: true, detail: 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Serialize\\StartupDelayInMSec' }),
  ],
  internet: [
    finding('internet', 0, 'deadProxy', { undoable: true, detail: '127.0.0.1:8888' }),
    finding('internet', 1, 'throttling', { undoable: true, recommended: false, needsAdmin: true }),
  ],
  disk: [],
  vuln: [
    finding('vuln', 0, 'security', { name: '2026-11 Cumulative Update for Windows 10 (KB5000001)', detail: 'Critical' }),
    finding('vuln', 1, 'update', { name: 'Microsoft .NET Framework 4.8.1 Update', recommended: false }),
  ],
  security: [
    finding('security', 0, 'firewallPublic', { undoable: true, needsAdmin: true }),
    finding('security', 1, 'hiddenExtensions', { undoable: true, recommended: false }),
  ],
};

let memory = { total: 34.3e9, available: 12.1e9, load: 65 };
const turbo = { on: false, available: true };
const startupItems = [
  { id: 'hkcuRun|Chat', name: 'Chat App', command: '"C:\\Users\\demo\\AppData\\Local\\Programs\\Chat\\Chat.exe" --minimized', location: 'hkcuRun', enabled: true, broken: false, needsAdmin: false },
  { id: 'hkcuRun|RemovedTool', name: 'RemovedTool', command: '"C:\\Program Files\\RemovedTool\\tray.exe"', location: 'hkcuRun', enabled: true, broken: true, needsAdmin: false },
  { id: 'hklmRun|Tablet', name: 'Tablet Driver', command: 'C:\\Program Files\\Tablet\\Tablet.exe', location: 'hklmRun', enabled: true, broken: false, needsAdmin: true },
  { id: 'userFolder|Capture.lnk', name: 'Capture.lnk', command: 'C:\\Users\\demo\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\Capture.lnk', location: 'userFolder', enabled: false, broken: false, needsAdmin: false },
];
// Lần mở giả theo UserAssist: 7-Zip vừa dùng, Old Game lâu không mở, Chat App không có ghi nhận.
const MOCK_NOW = Math.floor(Date.now() / 1000);
const MOCK_LAST_USED: Record<string, number> = {
  'HKLM\\a': MOCK_NOW - 3600,
  'HKLM\\b': MOCK_NOW - 400 * 86_400,
  'HKLM\\d': MOCK_NOW - 2 * 86_400,
  'store|SpotifyAB.SpotifyMusic_zpdnekdrzrea0': MOCK_NOW - 5 * 86_400,
};
const apps: AppInfo[] = [
  { id: 'HKLM\\a', kind: 'desktop', builtIn: false, removable: true, name: '7-Zip 23.01 (x64)', publisher: 'Igor Pavlov', version: '23.01', installDate: '2026-03-02', size: 5.6e6, installLocation: 'C:\\Program Files\\7-Zip', needsAdmin: true },
  // Deterministic failure cases exercise queue stopping and no forced fallback.
  { id: 'HKLM\\b', kind: 'desktop', builtIn: false, removable: true, name: 'Old Game', publisher: 'Game Studio', version: '1.2', installDate: '2024-08-19', size: 42e9, installLocation: 'D:\\Games\\Old Game', needsAdmin: true },
  { id: 'HKCU\\c', kind: 'desktop', builtIn: false, removable: true, name: 'Chat App', publisher: 'Chat Inc.', version: '5.0.1', installDate: '2026-11-10', size: 320e6, installLocation: null, needsAdmin: false },
  { id: 'store|Microsoft.WindowsCalculator_8wekyb3d8bbwe', kind: 'store', builtIn: true, removable: true, name: 'Windows Calculator', publisher: 'Microsoft Corporation', version: '11.2409.1.0', installDate: '2026-05-04', size: 32e6, installLocation: 'C:\\Program Files\\WindowsApps\\Microsoft.WindowsCalculator', needsAdmin: false },
  { id: 'store|SpotifyAB.SpotifyMusic_zpdnekdrzrea0', kind: 'store', builtIn: false, removable: true, name: 'Spotify', publisher: 'Spotify AB', version: '1.260.0.0', installDate: '2026-10-12', size: 290e6, installLocation: 'C:\\Program Files\\WindowsApps\\SpotifyAB.SpotifyMusic', needsAdmin: false },
  { id: 'store|Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy', kind: 'store', builtIn: true, removable: false, name: 'Windows Shell Experience Host', publisher: 'Microsoft Corporation', version: '10.0.19041.1', installDate: null, size: null, installLocation: 'C:\\Windows\\SystemApps\\ShellExperienceHost', needsAdmin: false },
  { id: 'HKLM\\d', kind: 'desktop', builtIn: false, removable: true, name: 'Free Video Player', publisher: 'Player Labs', version: '3.4', installDate: '2026-11-15', size: 88e6, installLocation: 'C:\\Program Files\\Free Video Player', needsAdmin: true, installedAt: 1_794_738_000, companions: ['Search Toolbar Helper'] },
  { id: 'HKLM\\e', kind: 'desktop', builtIn: false, removable: true, name: 'Search Toolbar Helper', publisher: 'Deal Search Ltd.', version: '1.0.7', installDate: '2026-11-15', size: 4.1e6, installLocation: 'C:\\Program Files (x86)\\Search Toolbar Helper', needsAdmin: true, installedAt: 1_794_738_090, companions: ['Free Video Player'] },
].map((app) => ({ installedAt: null, companions: [], lastUsed: MOCK_LAST_USED[app.id] ?? null, ...app }) as AppInfo);
// This set models only the current user's installed apps; other users/provisioning are untouched.
const installedApps = new Set(apps.map((app) => app.id));
const forceTargets = new Map<string, AppInfo>();
type PreparationSession = { id: number; ids: string[]; mode: UninstallMode; ready: boolean; started: boolean; restoreError: string | null };
type NativeWait = { sessionId: number; cancelled: boolean };
type LeftoverCache = { mode: UninstallMode; sessionId: number | null; items: Leftover[] };
type ToolUndo = { tool: 'uninstall'; appId: string; leftover: Leftover };
let sessionNumber = 0;
let preparation: PreparationSession | null = null;
let nativeWait: NativeWait | null = null;
let toolBusy = false;
const leftoverCache = new Map<string, LeftoverCache>();
const removedLeftovers = new Map<string, Set<string>>();
const largeFiles = [
  { path: 'C:\\Users\\demo\\Documents\\Windows.iso', size: 4.9e9, modified: 1760000000, category: 'image', appData: false },
  { path: 'C:\\Users\\demo\\AppData\\Local\\SomeApp\\cache\\blob.bin', size: 2.6e9, modified: 1762000000, category: 'other', appData: true },
  { path: 'C:\\Users\\demo\\Videos\\holiday.mp4', size: 1.8e9, modified: 1750000000, category: 'video', appData: false },
];

/** Số mục khuyến nghị còn lại của từng hạng mục đã quét, để giả lập điểm sức khỏe. */
const scanned = new Map<ModuleId, number>();

let cancelled = false;
let lastScan: Finding[] = [];
const history: RescueRecord[] = [];
let recordNumber = Date.now();

function getApp(id: string): AppInfo {
  const app = apps.find((a) => a.id === id) ?? forceTargets.get(id);
  if (!app) throw 'not_found';
  if (!app.removable) throw 'protected_app';
  return app;
}

function getMode(value: unknown): UninstallMode {
  const mode = value ?? 'standard';
  if (mode !== 'standard' && mode !== 'forced') throw 'invalid_mode';
  return mode;
}

function sameTargets(session: PreparationSession, ids: string[], mode: UninstallMode) {
  return session.mode === mode && session.ids.length === ids.length && session.ids.every((id, index) => id === ids[index]);
}

function restoreFailure(): string | null {
  const option = new URLSearchParams(window.location.search).get('restore');
  if (option === 'ok') return null;
  return option && ['needs_admin', 'disabled', 'too_soon', 'restore_failed'].includes(option) ? option : 'needs_admin';
}

async function prepareUninstall(ids: string[], mode: UninstallMode, continueWithoutRestore: boolean): Promise<UninstallPreparation> {
  if (toolBusy || nativeWait) throw 'busy';
  if (!Array.isArray(ids) || !ids.length || new Set(ids).size !== ids.length) throw 'no_selection';
  if (mode === 'forced' && ids.length !== 1) throw 'invalid_target';
  ids.forEach((id) => {
    const app = getApp(id);
    if (mode === 'forced' && app.kind !== 'desktop') throw 'protected_app';
    if (mode === 'standard' && !installedApps.has(id)) throw 'not_found';
  });
  if (continueWithoutRestore) {
    if (!preparation || !sameTargets(preparation, ids, mode) || preparation.ready || !preparation.restoreError) throw 'not_prepared';
    preparation.ready = true;
    return { ready: true, restoreError: null };
  }
  toolBusy = true;
  const session: PreparationSession = { id: ++sessionNumber, ids: [...ids], mode, ready: false, started: false, restoreError: null };
  ids.forEach((id) => leftoverCache.delete(id));
  preparation = session;
  try {
    await sleep(650);
    session.restoreError = restoreFailure();
    session.ready = session.restoreError === null;
    return { ready: session.ready, restoreError: session.restoreError };
  } finally {
    toolBusy = false;
  }
}

function makeForceTarget(name: string, inputPath: string): AppInfo {
  if (toolBusy || nativeWait) throw 'busy';
  if (typeof name !== 'string' || typeof inputPath !== 'string') throw 'invalid_target';
  name = name.trim();
  const path = inputPath.trim().replaceAll('/', '\\').replace(/\\+$/, '');
  const lower = path.toLowerCase();
  if (apps.some((a) => !a.removable && (a.name.toLowerCase() === name.toLowerCase() || a.installLocation?.toLowerCase() === lower))) throw 'protected_app';
  // Browser-only structural validation: no filesystem access or real system changes.
  if (name.length < 3 || name.length > 160 || /[\\/<>:"|?*\x00-\x1f]/.test(name) ||
      !/^[a-z]:\\[^\\]+/i.test(path) || /[<>"|?*\x00-\x1f]/.test(path) || path.slice(2).includes(':') ||
      path.slice(3).split('\\').some((part) => !part || part === '.' || part === '..' || /[. ]$/.test(part))) throw 'invalid_target';
  const parts = lower.slice(3).split('\\');
  if (parts[0] === 'windows' || parts[0] === '$recycle.bin' || parts[0] === 'system volume information' ||
      (['program files', 'program files (x86)', 'programdata', 'users'].includes(parts[0]) && parts.length === 1) ||
      (parts[0] === 'program files' && ['windowsapps', 'common files', 'windows defender', 'windows nt'].includes(parts[1])) ||
      (parts[0] === 'program files (x86)' && parts[1] === 'common files') ||
      (parts[0] === 'users' && (parts.length <= 2 || ['public', 'default', 'all users'].includes(parts[1]) ||
        (parts[2] === 'appdata' && (parts.length <= 3 || (['local', 'locallow', 'roaming'].includes(parts[3]) && parts.length <= 4)))))) throw 'invalid_target';
  if (apps.some((app) => {
    if (!installedApps.has(app.id) || !app.installLocation) return false;
    const other = app.installLocation.replaceAll('/', '\\').replace(/\\+$/, '').toLowerCase();
    return lower === other || lower.startsWith(`${other}\\`) || other.startsWith(`${lower}\\`);
  })) throw 'shared_force_target';
  const id = `force|${lower}`;
  const target: AppInfo = { id, kind: 'desktop', builtIn: false, removable: true, name, publisher: null, version: null, installDate: null, size: null, installLocation: path, needsAdmin: false, installedAt: null, companions: [], lastUsed: null };
  forceTargets.set(id, target);
  return { ...target };
}

async function simulateNativeUninstall(app: AppInfo, wait: NativeWait): Promise<UninstallResult> {
  if (app.id === 'HKCU\\c') throw 'no_uninstaller';
  for (let n = 0; n < 15; n++) {
    await sleep(100);
    if (wait.cancelled) return { exitCode: null, stillInstalled: installedApps.has(app.id) };
  }
  if (app.id === 'HKLM\\b') return { exitCode: 1603, stillInstalled: true };
  installedApps.delete(app.id);
  return { exitCode: 0, stillInstalled: false };
}

async function uninstallBatch(ids: string[]): Promise<BatchUninstallReport> {
  if (toolBusy || nativeWait) throw 'busy';
  if (!preparation?.ready || !sameTargets(preparation, ids, 'standard')) throw 'not_prepared';
  const targets = ids.map(getApp);
  const wait: NativeWait = { sessionId: preparation.id, cancelled: false };
  if (preparation.started) throw 'restore_required';
  preparation.started = true;
  nativeWait = wait;
  const report: BatchUninstallReport = { items: [], pending: [...ids], cancelled: false };
  try {
    for (const [index, app] of targets.entries()) {
      if (wait.cancelled) break;
      await emit('uninstall-progress', { current: index + 1, total: targets.length, name: app.name });
      if (wait.cancelled) break;
      report.pending.shift();
      try {
        if (!installedApps.has(app.id)) throw 'not_found';
        const result = await simulateNativeUninstall(app, wait);
        report.items.push({ id: app.id, name: app.name, result, error: null });
        if (wait.cancelled || result.stillInstalled) break;
      } catch (e) {
        report.items.push({ id: app.id, name: app.name, result: null, error: String(e) });
        // The installed app or its external uninstaller may still be present.
        break;
      }
    }
    report.cancelled = wait.cancelled;
    const completed = report.items.filter((item) => item.result && !item.result.stillInstalled);
    if (completed.length) history.unshift({ id: ++recordNumber, time: Math.floor(Date.now() / 1000), modules: [], title: `uninstall:${completed.map((item) => item.name).join(', ')}`, done: completed.length, failed: report.items.length - completed.length, bytes: 0, undo: [], undoable: 0, undone: false });
    return report;
  } finally {
    if (nativeWait === wait) nativeWait = null;
  }
}

function authorizeLeftovers(id: string, mode: UninstallMode): AppInfo {
  const app = getApp(id);
  if (!preparation?.ready || preparation.mode !== mode || !preparation.ids.includes(id)) throw 'restore_required';
  if (mode === 'forced' && app.kind !== 'desktop') throw 'protected_app';
  if (mode === 'standard' && installedApps.has(id)) throw 'still_installed';
  if (mode === 'standard' && forceTargets.has(id)) throw 'restore_required';
  return app;
}

function makeLeftovers(app: AppInfo): Leftover[] {
  const name = app.id === 'HKLM\\a' ? '7-Zip' : app.name;
  const item = (suffix: string, kind: Leftover['kind'], path: string, bytes = 0, recommended = true): Leftover => ({ id: `${app.id}|${suffix}`, kind, path, bytes, recommended });
  if (app.kind === 'store') {
    const family = app.id.slice('store|'.length);
    return [item('data', 'folder', `C:\\Users\\demo\\AppData\\Local\\Packages\\${family}`, 2.4e6, false)];
  }
  return [
    item('install', 'folder', app.installLocation ?? `C:\\Users\\demo\\AppData\\Local\\Programs\\${name}`, 1.2e6),
    item('data', 'folder', `C:\\Users\\demo\\AppData\\Roaming\\${name}`, 24e3, false),
    item('registry', 'registry', `HKCU\\Software\\${name}`),
    item('uninstall', 'uninstallEntry', `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${name}`),
    item('shortcut', 'shortcut', `C:\\Users\\demo\\Desktop\\${name}.lnk`, 1200, false),
    item('startup', 'startup', `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\${name}Tray`, 0, false),
    item('service', 'service', `${name}Updater → ${app.installLocation ?? name}\\updater.exe`, 0, false),
    item('task', 'task', `\\${name}\\Update → ${app.installLocation ?? name}\\updater.exe`, 0, false),
  ];
}

async function scanAppLeftovers(id: string, mode: UninstallMode): Promise<Leftover[]> {
  if (toolBusy || nativeWait) throw 'busy';
  const app = authorizeLeftovers(id, mode);
  toolBusy = true;
  try {
    await sleep(600);
    const items = makeLeftovers(app)
      .filter((l) => !removedLeftovers.get(id)?.has(l.id) && (l.kind !== 'uninstallEntry' || (mode === 'forced' && installedApps.has(id))))
      .map((l) => ({ ...l, recommended: mode === 'standard' && l.recommended }));
    leftoverCache.set(id, { mode, sessionId: mode === 'forced' ? preparation!.id : null, items });
    return structuredClone(items);
  } finally {
    toolBusy = false;
  }
}

async function removeAppLeftovers(id: string, ids: string[], mode: UninstallMode): Promise<RemoveReport> {
  if (toolBusy || nativeWait) throw 'busy';
  const app = authorizeLeftovers(id, mode);
  const cache = leftoverCache.get(id);
  if (!cache || cache.mode !== mode || (mode === 'forced' && cache.sessionId !== preparation!.id)) throw 'not_prepared';
  if (!Array.isArray(ids) || !ids.length || new Set(ids).size !== ids.length) throw 'no_selection';
  const picked = ids.map((key) => {
    const item = cache.items.find((l) => l.id === key);
    if (!item || removedLeftovers.get(id)?.has(key)) throw 'invalid_leftover';
    return item;
  });
  toolBusy = true;
  try {
    await sleep(500);
    const removed = removedLeftovers.get(id) ?? new Set<string>();
    picked.forEach((l) => removed.add(l.id));
    removedLeftovers.set(id, removed);
    if (picked.some((l) => l.kind === 'uninstallEntry')) installedApps.delete(id);
    cache.items = cache.items.filter((l) => !removed.has(l.id));
    const bytes = picked.reduce((sum, l) => sum + l.bytes, 0);
    const undo: ToolUndo[] = picked.filter((l) => ['registry', 'uninstallEntry', 'startup', 'shortcut', 'service', 'task'].includes(l.kind))
      .map((leftover) => ({ tool: 'uninstall', appId: id, leftover: { ...leftover } }));
    const recordId = ++recordNumber;
    history.unshift({ id: recordId, time: Math.floor(Date.now() / 1000), modules: [], title: `uninstall:${app.name}`, done: picked.length, failed: 0, bytes, undo, undoable: undo.length, undone: false });
    // Forced trace removal does not pretend to have run an app/package uninstaller.
    return { done: picked.length, failed: 0, bytes, recordId };
  } finally {
    toolBusy = false;
  }
}

async function scan(modules: ModuleId[]): Promise<ScanReport> {
  cancelled = false;
  const report: ScanReport = { modules: [], cancelled: false };
  for (const module of modules) {
    if (cancelled) break;
    await emit('scan-progress', { type: 'moduleStarted', module });
    for (const f of sample[module]) {
      await sleep(350);
      await emit('scan-progress', { type: 'path', path: f.detail ?? f.kind });
    }
    const findings = sample[module];
    await emit('scan-progress', {
      type: 'moduleFinished',
      module,
      count: findings.length,
      bytes: findings.reduce((sum, f) => sum + f.bytes, 0),
    });
    report.modules.push({ module, findings });
    scanned.set(module, findings.filter((f) => f.recommended).length);
  }
  report.cancelled = cancelled;
  lastScan = report.modules.flatMap((m) => m.findings);
  return report;
}

async function fix(ids: string[]): Promise<FixReport> {
  const picked = lastScan.filter((f) => ids.includes(f.id));
  const report: FixReport = { items: [], done: 0, failed: 0, bytes: 0, manual: 0, undoable: 0, recordId: null };
  for (const [index, f] of picked.entries()) {
    await emit('fix-progress', { current: index, total: picked.length });
    await sleep(300);
    if (f.module === 'vuln') {
      report.items.push({ id: f.id, module: f.module, done: 0, failed: 0, bytes: 0, manual: 1, error: null });
      report.manual += 1;
      continue;
    }
    // Mục cần quyền quản trị thất bại để xem được trạng thái lỗi.
    const failed = f.needsAdmin ? f.count : 0;
    const done = f.count - failed;
    report.items.push({ id: f.id, module: f.module, done, failed, bytes: failed ? 0 : f.bytes, manual: 0, error: failed ? 'needs_admin' : null });
    if (!failed && f.recommended) scanned.set(f.module, Math.max(0, (scanned.get(f.module) ?? 0) - 1));
    report.done += done;
    report.failed += failed;
    report.bytes += failed ? 0 : f.bytes;
    if (f.undoable && !failed) report.undoable += 1;
  }
  if (report.done > 0) {
    report.recordId = ++recordNumber;
    history.unshift({
      id: report.recordId,
      time: Math.floor(Date.now() / 1000),
      modules: [...new Set(picked.map((f) => f.module))],
      done: report.done,
      failed: report.failed,
      bytes: report.bytes,
      undo: Array(report.undoable).fill({}),
      undoable: report.undoable,
      undone: false,
    });
  }
  return report;
}

type MaintenanceJob = { requestId: string; cancelled: boolean };
type TweakUndo = { tool: 'tuning'; id: string; active: boolean };
let maintenanceJob: MaintenanceJob | null = null;
const usedRequests = new Set<string>();
const option = (key: string) => new URLSearchParams(window.location.search).get(key);
const locations = [
  { path: 'C:\\Users\\demo', kind: 'profile', free: 120e9, total: 500e9 },
  { path: 'C:\\', kind: 'drive', free: 120e9, total: 500e9 },
  { path: 'D:\\', kind: 'drive', free: 800e9, total: 1000e9 },
];
const now = () => Math.floor(Date.now() / 1000);

function validRequest(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) throw 'invalid_request';
  return value;
}
function selectedIds(value: unknown): string[] {
  if (!Array.isArray(value) || !value.length) throw 'no_selection';
  if (value.some((id) => typeof id !== 'string') || new Set(value).size !== value.length) throw 'invalid_request';
  return value as string[];
}
const extensionItems: BrowserExtension[] = [
  { id: 'chrome|Default|aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', browser: 'chrome', profile: 'Default', extensionId: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', name: 'Deal Search Toolbar', version: '1.0.7', source: 'external', enabled: true, removal: 'registry', registry: ['HKLM\\SOFTWARE\\WOW6432Node\\Google\\Chrome\\Extensions\\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'], needsAdmin: true },
  { id: 'chrome|Profile 1|aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', browser: 'chrome', profile: 'Profile 1', extensionId: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', name: 'Deal Search Toolbar', version: '1.0.7', source: 'external', enabled: false, removal: 'registry', registry: ['HKLM\\SOFTWARE\\WOW6432Node\\Google\\Chrome\\Extensions\\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'], needsAdmin: true },
  { id: 'chrome|Default|bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', browser: 'chrome', profile: 'Default', extensionId: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', name: 'Ad Blocker', version: '4.2.0', source: 'store', enabled: true, removal: 'browser', registry: [], needsAdmin: false },
  { id: 'edge|Default|cccccccccccccccccccccccccccccccc', browser: 'edge', profile: 'Default', extensionId: 'cccccccccccccccccccccccccccccccc', name: 'Company Password Manager', version: '9.1', source: 'policy', enabled: true, removal: 'none', registry: [], needsAdmin: false },
  { id: 'edge|-|dddddddddddddddddddddddddddddddd', browser: 'edge', profile: null, extensionId: 'dddddddddddddddddddddddddddddddd', name: 'dddddddddddddddddddddddddddddddd', version: null, source: 'external', enabled: null, removal: 'registry', registry: ['HKCU\\SOFTWARE\\Microsoft\\Edge\\Extensions\\dddddddddddddddddddddddddddddddd'], needsAdmin: false },
  { id: 'firefox|ab12.default-release|blocker@example', browser: 'firefox', profile: 'ab12.default-release', extensionId: 'blocker@example', name: 'Tracker Blocker', version: '2.3', source: 'store', enabled: true, removal: 'browser', registry: [], needsAdmin: false },
];
let extensionCache: BrowserExtension[] | null = null;
const windowsUpdates: WindowsUpdate[] = [
  { id: 'KB5126256', description: 'Security Update', installedOn: '2026-11-12' },
  { id: 'KB5075039', description: 'Update', installedOn: '2026-10-08' },
  { id: 'KB5072653', description: 'Security Update', installedOn: '2026-09-10' },
  { id: 'KB5000099', description: 'Hotfix', installedOn: null },
];
let updateCache: WindowsUpdate[] = [];
let updatePreparation: { id: string; ready: boolean; restoreError: string | null } | null = null;
let updateWait: { cancelled: boolean } | null = null;

async function listExtensions(): Promise<ExtensionScan> {
  return toolWork(async () => {
    await sleep(450);
    extensionCache = structuredClone(extensionItems);
    return { extensions: structuredClone(extensionItems), skipped: 1 };
  });
}

async function removeExtensions(ids: unknown): Promise<ExtensionRemoval> {
  return toolWork(async () => {
    const cache = extensionCache;
    if (!cache || !Array.isArray(ids) || !ids.length) throw 'not_found';
    const picked = ids.map((id) => cache.find((e) => e.id === id));
    if (picked.some((e) => !e)) throw 'not_found';
    if (picked.some((e) => e!.removal !== 'registry')) throw 'protected_app';
    await sleep(500);
    const paths = [...new Set(picked.flatMap((e) => e!.registry))];
    const failedPaths = paths.filter((path) => path.startsWith('HKLM') && new URLSearchParams(window.location.search).get('admin') !== '1');
    const donePaths = paths.filter((path) => !failedPaths.includes(path));
    for (let i = extensionItems.length - 1; i >= 0; i--) {
      if (extensionItems[i].registry.some((path) => donePaths.includes(path))) extensionItems.splice(i, 1);
    }
    extensionCache = null;
    const recordId = donePaths.length ? ++recordNumber : null;
    if (recordId !== null) history.unshift({ id: recordId, time: Math.floor(Date.now() / 1000), modules: [], title: 'browserExtensions', done: donePaths.length, failed: failedPaths.length, bytes: 0, undo: [], undoable: 0, undone: false });
    return { done: donePaths.length, failed: failedPaths.length, recordId, errors: failedPaths.map((path) => `needs_admin: ${path}`) };
  });
}

async function listUpdates(): Promise<WindowsUpdate[]> {
  return toolWork(async () => {
    await sleep(600);
    updatePreparation = null;
    updateCache = structuredClone(windowsUpdates);
    return structuredClone(windowsUpdates);
  });
}

async function prepareUpdate(id: unknown, continueWithoutRestore: boolean) {
  if (toolBusy || nativeWait || updateWait) throw 'busy';
  if (!updateCache.some((u) => u.id === id)) throw 'not_found';
  if (continueWithoutRestore) {
    if (!updatePreparation || updatePreparation.id !== id || updatePreparation.ready || !updatePreparation.restoreError) throw 'restore_required';
    updatePreparation.ready = true;
    return { ready: true, restoreError: updatePreparation.restoreError };
  }
  return toolWork(async () => {
    await sleep(600);
    const restoreError = restoreFailure();
    updatePreparation = { id: id as string, ready: !restoreError, restoreError };
    return { ready: !restoreError, restoreError };
  });
}

async function uninstallUpdate(id: unknown): Promise<UpdateResult> {
  if (toolBusy || nativeWait || updateWait) throw 'busy';
  if (!updateCache.some((u) => u.id === id)) throw 'not_found';
  if (!updatePreparation || !updatePreparation.ready || updatePreparation.id !== id) throw 'restore_required';
  updatePreparation = null;
  const wait = { cancelled: false };
  updateWait = wait;
  try {
    for (let i = 0; i < 12 && !wait.cancelled; i++) await sleep(150);
    if (wait.cancelled) return { exitCode: null, restartRequired: false, stillInstalled: true };
    // ?wu=fail mô phỏng bản tích lũy không gỡ được; mặc định cần khởi động lại.
    const option = new URLSearchParams(window.location.search).get('wu');
    const exitCode = option === 'fail' ? 0x80070057 : option === 'cancel' ? 1223 : 3010;
    const result = { exitCode, restartRequired: exitCode === 3010, stillInstalled: true };
    history.unshift({ id: ++recordNumber, time: Math.floor(Date.now() / 1000), modules: [], title: `windowsUpdate:${id}`, done: result.restartRequired ? 1 : 0, failed: result.restartRequired ? 0 : 1, bytes: 0, undo: [], undoable: 0, undone: false });
    return result;
  } finally {
    updateWait = null;
  }
}

async function toolWork<T>(work: () => Promise<T>): Promise<T> {
  if (toolBusy || nativeWait) throw 'busy';
  toolBusy = true;
  try { return await work(); }
  finally { toolBusy = false; }
}
async function maintenanceWork<T>(value: unknown, work: (job: MaintenanceJob) => Promise<T>): Promise<T> {
  const requestId = validRequest(value);
  if (usedRequests.has(requestId)) throw 'invalid_request';
  if (toolBusy || nativeWait) throw 'busy';
  usedRequests.add(requestId);
  const job: MaintenanceJob = { requestId, cancelled: false };
  maintenanceJob = job; toolBusy = true;
  try { return await work(job); }
  finally { if (maintenanceJob === job) maintenanceJob = null; toolBusy = false; }
}
async function maintenanceProgress(job: MaintenanceJob, path: string, count: number) {
  if (!job.cancelled) await emit('maintenance-progress', { requestId: job.requestId, path, count });
}
function permanentRecord(title: string, done: number, failed: number, bytes: number): number | null {
  if (!done && !failed) return null;
  const id = ++recordNumber;
  history.unshift({ id, title, time: now(), modules: [], done, failed, bytes, undo: [], undoable: 0, undone: false });
  return id;
}

let duplicateCache: DuplicateGroup[] = [];
const recycledDuplicates = new Set<string>();
function duplicateFixtures(root: string): DuplicateGroup[] {
  const base = root.replace(/\\+$/, '');
  const group = (size: number, paths: string[]): DuplicateGroup => ({
    id: crypto.randomUUID(), size,
    files: paths.map((path) => ({ id: crypto.randomUUID(), path: `${base}\\${path}`, modified: 1762000000 })),
  });
  return [
    group(12 * 1024 * 1024, ['Pictures\\portrait.tif', 'Documents\\portrait.tif', 'Copies\\portrait-copy.tif']),
    group(64 * 1024 * 1024, ['Documents\\project.zip', 'Downloads\\project-copy.zip']),
  ].map((g) => ({ ...g, files: g.files.filter((f) => !recycledDuplicates.has(f.path)) })).filter((g) => g.files.length > 1);
}
function scanDuplicates(args: Record<string, unknown>): Promise<DuplicateScan> {
  return maintenanceWork(args.requestId, async (job) => {
    duplicateCache = [];
    if (!locations.some((l) => l.path === args.root) || ![1, 10, 50].includes(args.minMb as number)) throw 'invalid_target';
    const candidates = duplicateFixtures(args.root as string).filter((g) => g.size >= Number(args.minMb) * 1024 * 1024);
    const groups: DuplicateGroup[] = [];
    let filesChecked = 0;
    for (const group of candidates) {
      for (const file of group.files) {
        await sleep(220);
        if (job.cancelled) break;
        filesChecked++;
        await maintenanceProgress(job, file.path, filesChecked);
      }
      if (job.cancelled) break;
      groups.push(group);
    }
    const truncated = option('duplicate') === 'truncated';
    duplicateCache = truncated ? groups.slice(0, 1) : groups;
    return { groups: structuredClone(duplicateCache), filesChecked, skipped: 4, cancelled: job.cancelled, truncated };
  });
}
function recycleDuplicates(args: Record<string, unknown>): Promise<DuplicateRemoval> {
  return maintenanceWork(args.requestId, async (job) => {
    const ids = selectedIds(args.ids);
    const files = duplicateCache.flatMap((g) => g.files.map((f) => ({ ...f, size: g.size })));
    const targets = ids.map((id) => {
      const file = files.find((f) => f.id === id);
      if (!file || recycledDuplicates.has(file.path)) throw 'changed_target';
      return file;
    });
    if (duplicateCache.some((g) => g.files.every((f) => ids.includes(f.id)))) throw 'keep_one';
    const removedIds: string[] = [];
    const recovery: string[] = [];
    let failed = 0; let bytes = 0;
    for (const file of targets) {
      await sleep(350);
      if (job.cancelled) break;
      if (option('duplicate') === 'partial' && file.path.endsWith('portrait-copy.tif')) { failed++; continue; }
      if (option('duplicate') === 'recovery') {
        failed++;
        recovery.push(`recovery_pending: original=${file.path}; staging=C:\\TPRecycleFixture\\payload-${file.id}; recycle_error=fixture failure`);
        continue;
      }
      recycledDuplicates.add(file.path); removedIds.push(file.id); bytes += file.size;
      recovery.push(`recycled: original=${file.path}; recycle_location=C:\\TPRecycleFixture\\bin-${file.id}; volume=fixture; file_id=${file.id}`);
    }
    duplicateCache = duplicateCache.map((g) => ({ ...g, files: g.files.filter((f) => !removedIds.includes(f.id)) }));
    return { done: removedIds.length, failed, bytes, removedIds, recovery, cancelled: job.cancelled,
      recordId: permanentRecord(recovery.length ? `duplicateFiles; ${JSON.stringify(recovery)}` : 'duplicateFiles', removedIds.length, failed, bytes) };
  });
}

let componentAnalysis: ComponentAnalysis | null = null;
let componentPreparation: { ready: boolean; restoreError: string | null } | null = null;
let componentState: ComponentStatus = { state: 'idle', report: null, error: null };
let componentCleaned = false;
function analyzeComponent(args: Record<string, unknown>): Promise<ComponentAnalysis> {
  return maintenanceWork(args.requestId, async (job) => {
    componentAnalysis = null; componentPreparation = null;
    for (let n = 0; n < 8; n++) { await sleep(200); if (job.cancelled) throw 'cancelled'; }
    componentAnalysis = {
      actualSize: 9.8e9, sharedSize: 6.1e9, backupsSize: 2.9e9, cacheSize: .8e9,
      reclaimablePackages: componentCleaned || option('winsxs') === 'clean' ? 0 : 3,
      cleanupRecommended: !componentCleaned && option('winsxs') !== 'clean',
      lastCleanup: '2026-10-12 10:30:00', output: 'Deployment Image Servicing and Management\nActual Size of Component Store: 9.13 GB\nShared with Windows: 5.68 GB\nThe operation completed successfully.',
    };
    return structuredClone(componentAnalysis);
  });
}
function prepareComponent(bypass: boolean): Promise<UninstallPreparation> {
  return toolWork(async () => {
    if (!componentAnalysis?.cleanupRecommended) throw 'cleanup_not_recommended';
    if (bypass) {
      if (!componentPreparation?.restoreError || componentPreparation.ready) throw 'restore_required';
      componentPreparation.ready = true;
    } else {
      componentPreparation = null;
      await sleep(650);
      const restoreError = restoreFailure();
      componentPreparation = { ready: restoreError === null, restoreError };
    }
    return { ...componentPreparation };
  });
}
function startComponent(confirmed: unknown): ComponentStatus {
  if (toolBusy || nativeWait) throw 'busy';
  if (confirmed !== true) throw 'invalid_request';
  if (!componentAnalysis?.cleanupRecommended) throw 'cleanup_not_recommended';
  if (!componentPreparation?.ready) throw 'restore_required';
  componentPreparation = null; componentAnalysis = null;
  toolBusy = true; componentState = { state: 'running', report: null, error: null };
  const recordId = ++recordNumber;
  history.unshift({ id: recordId, time: now(), modules: [], title: 'componentStore', done: 0, failed: 0, bytes: 0, undo: [], undoable: 0, undone: false });
  void (async () => {
    try {
      await sleep(4500);
      const failed = option('winsxs') === 'failed';
      const report = { exitCode: failed ? 87 : option('winsxs') === 'reboot' ? 3010 : 0,
        rebootRequired: option('winsxs') === 'reboot',
        output: failed ? 'DISM Error: 87. The operation failed.' : 'DISM StartComponentCleanup [100.0%]\nThe operation completed successfully.' };
      componentState = { state: failed ? 'failed' : 'completed', report,
        error: failed ? 'DISM Error 87' : option('winsxs') === 'warning' ? 'Mock: cleanup completed but history backup step reported a warning.' : null };
      if (!failed) componentCleaned = true;
      const record = history.find((item) => item.id === recordId);
      if (record && option('winsxs') !== 'warning') { record.done = failed ? 0 : 1; record.failed = failed ? 1 : 0; }
    } finally { toolBusy = false; }
  })();
  return structuredClone(componentState);
}

const tweaks: Tweak[] = [
  { id: 'minimizeAnimations', area: 'deep', active: false, available: true, reason: null, needsRestart: true },
  { id: 'taskbarAnimations', area: 'deep', active: true, available: true, reason: null, needsRestart: true },
  { id: 'backgroundCapture', area: 'deep', active: false, available: true, reason: null, needsRestart: false },
  { id: 'chromeBackground', area: 'browser', active: false, available: true, reason: null, needsRestart: true },
  { id: 'edgeBackground', area: 'browser', active: false, available: false, reason: 'managed_policy', needsRestart: true },
  { id: 'edgeStartupBoost', area: 'browser', active: false, available: false, reason: 'managed_policy', needsRestart: true },
  { id: 'braveBackground', area: 'browser', active: false, available: true, reason: null, needsRestart: true },
];
function applyTweaks(value: unknown): Promise<TweakReport> {
  return toolWork(async () => {
    const ids = selectedIds(value);
    const picked = ids.map((id) => { const item = tweaks.find((t) => t.id === id); if (!item) throw 'invalid_target'; return item; });
    const available = picked.filter((t) => t.available && !t.active);
    const undo: TweakUndo[] = available.map((t) => ({ tool: 'tuning', id: t.id, active: t.active }));
    const recordId = available.length ? ++recordNumber : null;
    if (recordId !== null) history.unshift({ id: recordId, time: now(), modules: [],
      title: available.every((t) => t.area === 'browser') ? 'browserTweaks' : 'deepOptimization', done: 0, failed: 0, bytes: 0, undo, undoable: undo.length, undone: false });
    await sleep(550);
    available.forEach((t) => { t.active = true; });
    const failed = picked.length - available.length;
    const record = history.find((r) => r.id === recordId);
    if (record) { record.done = available.length; record.failed = failed; }
    return { done: available.length, failed, recordId, needsRestart: available.some((t) => t.needsRestart) };
  });
}

let cacheInventory: BrowserCache[] | null = null;
let cacheSelection: BrowserCache[] = [];
function cacheFixtures(): BrowserCache[] {
  const paths: [BrowserCache['browser'], string][] = [
    ['chrome', 'Google\\Chrome\\User Data\\Default'], ['edge', 'Microsoft\\Edge\\User Data\\Default'], ['brave', 'BraveSoftware\\Brave-Browser\\User Data\\Default'],
  ];
  return [...paths.flatMap(([browser, path]) => ['Cache', 'Code Cache', 'GPUCache'].map((folder, index) => ({
    id: crypto.randomUUID(), browser, path: `C:\\Users\\demo\\AppData\\Local\\${path}\\${folder}`, bytes: (3 - index) * 40e6, files: (3 - index) * 120,
  }))), { id: crypto.randomUUID(), browser: 'firefox', path: 'C:\\Users\\demo\\AppData\\Local\\Mozilla\\Firefox\\Profiles\\demo.default\\cache2', bytes: 80e6, files: 220 }];
}
function scanBrowserCaches(args: Record<string, unknown>): Promise<BrowserCacheScan> {
  return maintenanceWork(args.requestId, async (job) => {
    cacheSelection = [];
    cacheInventory ??= cacheFixtures();
    const caches: BrowserCache[] = [];
    for (const item of cacheInventory) {
      await sleep(120); if (job.cancelled) break;
      caches.push({ ...item }); await maintenanceProgress(job, item.path, caches.reduce((sum, c) => sum + c.files, 0));
    }
    cacheSelection = caches;
    return { caches: structuredClone(caches), skipped: 2, cancelled: job.cancelled };
  });
}
function clearBrowserCaches(args: Record<string, unknown>): Promise<BrowserCacheReport> {
  return maintenanceWork(args.requestId, async (job) => {
    const ids = selectedIds(args.ids);
    const picked = ids.map((id) => { const cache = cacheSelection.find((c) => c.id === id); if (!cache) throw 'changed_target'; return cache; });
    if (option('browser') === 'running' && picked.some((c) => c.browser === 'chrome')) throw 'browser_running';
    let done = 0; let failed = 0; let bytes = 0;
    const recovery: string[] = [];
    for (const cache of picked) {
      await sleep(300); if (job.cancelled) break;
      if (option('browser') === 'recovery') {
        failed++;
        recovery.push(`recovery_pending: original=${cache.path}; staging=C:\\TPRecycleFixture\\payload-${cache.id}; recycle_error=fixture failure`);
        continue;
      }
      done += cache.files; bytes += cache.bytes;
      recovery.push(`recycled: original=${cache.path}; recycle_location=C:\\TPRecycleFixture\\bin-${cache.id}; volume=fixture; file_id=${cache.id}`);
      cacheInventory = cacheInventory!.filter((c) => c.id !== cache.id);
      cacheSelection = cacheSelection.filter((c) => c.id !== cache.id);
    }
    return { done, failed, bytes, recovery, cancelled: job.cancelled, recordId: permanentRecord(recovery.length ? `browserCache; ${JSON.stringify(recovery)}` : 'browserCache', done, failed, bytes) };
  });
}

let monitorNumber = Date.now();
const monitorSessions = new Map<number, MonitorReport>();
function monitorTarget(value: unknown): string {
  if (typeof value !== 'string') throw 'invalid_monitor_target';
  const root = value.trim().replaceAll('/', '\\').replace(/\\+$/, '');
  if (!/^[a-z]:\\[^\\]+/i.test(root) || /[<>"|?*\x00-\x1f]/.test(root) || root.slice(2).includes(':')) throw 'invalid_monitor_target';
  const parts = root.slice(3).toLowerCase().split('\\');
  if (parts.some((p) => !p || p === '.' || p === '..' || /[. ]$/.test(p)) ||
      ['windows', '$recycle.bin', 'system volume information'].includes(parts[0]) ||
      (['program files', 'program files (x86)', 'programdata', 'users', 'tools'].includes(parts[0]) && parts.length === 1) ||
      (['program files', 'program files (x86)'].includes(parts[0]) && ['common files', 'windowsapps', 'windows defender', 'windows nt'].includes(parts[1])) ||
      (parts[0] === 'users' && (parts.length < 3 || ['public', 'default', 'all users'].includes(parts[1]) ||
        (parts[2] === 'appdata' && (parts.length <= 3 || (['local', 'locallow', 'roaming'].includes(parts[3]) && parts.length <= 4)))))) throw 'invalid_monitor_target';
  const lower = root.toLowerCase();
  if (apps.some((app) => {
    const path = app.installLocation?.toLowerCase();
    return path && installedApps.has(app.id) && (lower === path || lower.startsWith(`${path}\\`) || path.startsWith(`${lower}\\`));
  })) throw 'invalid_monitor_target';
  return root;
}
function beginMonitor(args: Record<string, unknown>): Promise<MonitorSummary> {
  return maintenanceWork(args.requestId, async (job) => {
    if ([...monitorSessions.values()].some((r) => r.session.finishedAt === null)) throw 'monitor_active';
    if (typeof args.name !== 'string' || !args.name || args.name !== args.name.trim() || new TextEncoder().encode(args.name).length > 128 || /[\u0000-\u001f\u007f-\u009f]/.test(args.name)) throw 'monitor_invalid_name';
    const root = monitorTarget(args.root);
    if (typeof args.vendor !== 'string' || (args.vendor !== '' && (args.vendor !== args.vendor.trim() || args.vendor.length > 64 || !/^[a-z0-9 _-]+$/i.test(args.vendor) || !/[a-z]/i.test(args.vendor) || /^(tp|microsoft|classes|policies|windows)/i.test(args.vendor) || ['software', 'vendor', 'vendors', 'app', 'apps', 'application', 'applications', 'company', 'companies', 'common', 'common files', 'shared', 'system', 'wow6432node', 'clients', 'registeredapplications', 'install', 'installer', 'uninstall', 'app paths', 'run', 'runonce', 'users', 'default', 'public', 'currentversion', 'services', 'security', 'sam', 'components', 'network'].includes(args.vendor.toLowerCase())))) throw 'invalid_vendor';
    for (let n = 1; n <= 6; n++) {
      await sleep(180); if (job.cancelled) throw 'cancelled';
      await maintenanceProgress(job, n < 3 ? root : 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall', n * 12);
    }
    if (job.cancelled) throw 'cancelled';
    const existing = option('monitor') === 'existing';
    const session: MonitorSummary = { id: ++monitorNumber, name: args.name.trim(), root, vendor: args.vendor.trim(), startedAt: Date.now(), finishedAt: null, filesBefore: existing ? 3 : 0, changeCount: 0, complete: true };
    monitorSessions.set(session.id, { session, changes: [], warnings: [...(existing ? [] : ['target_missing']), 'simultaneous_changes'], truncated: false });
    return { ...session };
  });
}
function finishMonitor(args: Record<string, unknown>): Promise<MonitorReport> {
  return maintenanceWork(args.requestId, async (job) => {
    if (typeof args.id !== 'number' || !Number.isSafeInteger(args.id) || args.id <= 0) throw 'invalid_request';
    const before = monitorSessions.get(args.id);
    if (!before) throw 'not_found';
    if (before.session.finishedAt !== null) throw 'invalid_request';
    for (let n = 1; n <= 8; n++) {
      await sleep(180); if (job.cancelled) throw 'cancelled';
      await maintenanceProgress(job, before.session.root, n * 20);
    }
    if (job.cancelled) throw 'cancelled';
    const root = before.session.root;
    const changes: MonitorChange[] = [
      { kind: 'file', change: 'added', path: `${root}\\app.exe`, valueName: null },
      { kind: 'file', change: 'added', path: `${root}\\resources\\app.dat`, valueName: null },
      { kind: 'registry', change: 'added', path: `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${before.session.name}`, valueName: 'DisplayName' },
      { kind: 'registry', change: 'modified', path: 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run', valueName: 'ExampleTray' },
      { kind: 'registry', change: 'removed', path: 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\ExampleOld', valueName: null },
    ];
    if (before.session.filesBefore > 0) changes.push(
      { kind: 'file', change: 'modified', path: `${root}\\settings.ini`, valueName: null },
      { kind: 'file', change: 'removed', path: `${root}\\old-version.dll`, valueName: null },
    );
    if (before.session.vendor) changes.push({ kind: 'registry', change: 'added', path: `HKCU\\Software\\${before.session.vendor}`, valueName: null });
    const incomplete = option('monitor') === 'incomplete';
    const truncated = option('monitor') === 'truncated';
    const result: MonitorReport = { session: { ...before.session, finishedAt: Date.now(), changeCount: changes.length, complete: !incomplete && !truncated },
      changes, warnings: [...before.warnings, ...(incomplete ? ['monitor_incomplete'] : [])], truncated };
    monitorSessions.set(result.session.id, result);
    return structuredClone(result);
  });
}

export function install(): void {
  mockIPC(
    (cmd, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case 'maintenance_cancel': {
          const id = validRequest(args.requestId);
          if (maintenanceJob?.requestId === id) maintenanceJob.cancelled = true;
          return;
        }
        case 'duplicate_scan': return scanDuplicates(args);
        case 'duplicate_recycle': return recycleDuplicates(args);
        case 'duplicate_reveal':
          if (!duplicateCache.some((g) => g.files.some((f) => f.id === args.id))) throw 'changed_target';
          return;
        case 'component_analyze': return analyzeComponent(args);
        case 'component_prepare': return prepareComponent(args.continueWithoutRestore === true);
        case 'component_start': return startComponent(args.confirmed);
        case 'component_status': return structuredClone(componentState);
        case 'tuning_list':
          if (toolBusy || nativeWait) throw 'busy';
          return structuredClone(tweaks);
        case 'tuning_apply': return applyTweaks(args.ids);
        case 'browser_cache_scan': return scanBrowserCaches(args);
        case 'browser_cache_clear': return clearBrowserCaches(args);
        case 'browser_cache_reveal':
          if (!cacheSelection.some((c) => c.id === args.id)) throw 'changed_target';
          return;
        case 'browser_flush_dns':
          return toolWork(() => sleep(450));
        case 'browser_network':
          return;
        case 'monitor_list':
          if (toolBusy || nativeWait) throw 'busy';
          return structuredClone([...monitorSessions.values()].map((r) => r.session).sort((a, b) => b.id - a.id));
        case 'monitor_begin': return beginMonitor(args);
        case 'monitor_finish': return finishMonitor(args);
        case 'monitor_detail': {
          if (typeof args.id !== 'number' || !Number.isSafeInteger(args.id) || args.id <= 0) throw 'invalid_request';
          const result = monitorSessions.get(args.id);
          if (!result) throw 'not_found';
          if (result.session.finishedAt === null) throw 'monitor_not_finished';
          return structuredClone(result);
        }
        case 'is_elevated':
          return false;
        case 'scan':
          return toolWork(() => scan(args.modules as ModuleId[]));
        case 'cancel':
          cancelled = true;
          if (nativeWait) nativeWait.cancelled = true;
          if (updateWait) updateWait.cancelled = true;
          return;
        case 'extensions_list': return listExtensions();
        case 'extensions_remove': return removeExtensions(args.ids);
        case 'extension_manage': {
          const item = extensionCache?.find((e) => e.id === args.id);
          if (!item) throw 'not_found';
          if (item.removal !== 'browser') throw 'protected_app';
          return;
        }
        case 'updates_list': return listUpdates();
        case 'update_prepare': return prepareUpdate(args.id, args.continueWithoutRestore === true);
        case 'update_uninstall': return uninstallUpdate(args.id);
        case 'updates_history': return;
        case 'fix':
          return toolWork(() => fix(args.ids as string[]));
        case 'advise': {
          const never = MODULES.filter((m) => !scanned.has(m)).map((module) => ({ module, reason: 'never' }));
          const pending = MODULES.filter((m) => (scanned.get(m) ?? 0) > 0).map((module) => ({ module, reason: 'pending' }));
          const advice = [...never, ...pending];
          return advice.length > 0 ? advice : [{ module: 'junk', reason: 'routine' }];
        }
        case 'health': {
          const issues = [...scanned.values()].reduce((sum, n) => sum + n, 0);
          return {
            score: scanned.size === 0 ? null : Math.max(0, 100 - issues * 6),
            scanned: scanned.size,
            total: MODULES.length,
            issues,
          };
        }
        case 'rescue_history':
          return structuredClone(history);
        case 'rescue_undo': {
          if (toolBusy || nativeWait) throw 'busy';
          const record = history.find((r) => r.id === args.id);
          if (!record) throw 'not_found';
          const restored = record.undo.length;
          for (const value of record.undo) {
            const tweakUndo = value as Partial<TweakUndo>;
            if (tweakUndo.tool === 'tuning' && tweakUndo.id) {
              const item = tweaks.find((t) => t.id === tweakUndo.id);
              if (item) item.active = tweakUndo.active ?? false;
              continue;
            }
            const undo = value as Partial<ToolUndo>;
            if (undo.tool === 'uninstall' && undo.appId && undo.leftover) {
              removedLeftovers.get(undo.appId)?.delete(undo.leftover.id);
              if (undo.leftover.kind === 'uninstallEntry') installedApps.add(undo.appId);
              leftoverCache.delete(undo.appId);
            }
          }
          record.undo = [];
          record.undone = true;
          return { restored, failed: 0 };
        }
        case 'memory_status':
          return memory;
        case 'release_ram':
        case 'turbo_set': {
          return toolWork(async () => {
            if (cmd === 'turbo_set') turbo.on = args.on as boolean;
            if (cmd === 'turbo_set' && !args.on) { await sleep(300); return null; }
            const before = { ...memory };
            memory = { ...memory, available: memory.available + 1.4e9, load: memory.load - 4 };
            await sleep(900);
            return { before, after: { ...memory }, processes: 142 };
          });
        }
        case 'turbo_status':
          return { ...turbo };
        case 'startup_list':
          return startupItems;
        case 'startup_set': {
          if (toolBusy || nativeWait) throw 'busy';
          const item = startupItems.find((i) => i.id === args.id);
          if (!item) throw 'not_found';
          if (item.needsAdmin) throw 'needs_admin';
          item.enabled = args.enabled as boolean;
          return { ...item };
        }
        case 'apps_list':
          return (async () => {
            if (toolBusy || nativeWait) throw 'busy';
            toolBusy = true;
            try {
              await sleep(400);
              preparation = null;
              leftoverCache.clear();
              forceTargets.clear();
              return structuredClone(apps.filter((app) => installedApps.has(app.id)));
            } finally { toolBusy = false; }
          })();
        case 'app_force_target':
          return makeForceTarget(args.name as string, args.path as string);
        case 'uninstall_prepare':
          return prepareUninstall(args.ids as string[], getMode(args.mode), args.continueWithoutRestore === true);
        case 'apps_uninstall':
          return uninstallBatch(args.ids as string[]);
        case 'app_uninstall':
          return uninstallBatch([args.id as string]).then((report) => {
            const item = report.items[0];
            if (item?.error) throw item.error;
            return item?.result ?? { exitCode: null, stillInstalled: installedApps.has(args.id as string) };
          });
        case 'leftovers_scan':
          return scanAppLeftovers(args.id as string, getMode(args.mode));
        case 'leftovers_remove':
          return removeAppLeftovers(args.id as string, args.ids as string[], getMode(args.mode));
        case 'large_locations':
          return structuredClone(locations);
        case 'large_scan':
          return toolWork(async () => {
            cancelled = false;
            for (let n = 1; n <= 5; n++) {
              await sleep(300);
              if (cancelled) break;
              await emit('large-progress', { path: `C:\\Users\\demo\\Videos\\clip${n}.mp4`, count: n * 4000 });
            }
            return { files: structuredClone(largeFiles), cancelled };
          });
        case 'large_recycle': {
          const paths = args.paths as string[];
          const picked = largeFiles.filter((f) => paths.includes(f.path));
          return toolWork(async () => {
            await sleep(400);
            const bytes = picked.reduce((s, f) => s + f.size, 0);
            const recordId = picked.length ? ++recordNumber : null;
            if (recordId !== null) history.unshift({ id: recordId, time: Math.floor(Date.now() / 1000), modules: [], title: 'largeFiles', done: picked.length, failed: 0, bytes, undo: [], undoable: 0, undone: false });
            return { done: picked.length, failed: 0, bytes, recordId };
          });
        }
        case 'reveal':
          return;
        case 'create_restore_point':
          return toolWork(async () => {
            await sleep(650);
            const error = restoreFailure();
            if (error) throw error;
          });
      }
    },
    { shouldMockEvents: true },
  );
}
