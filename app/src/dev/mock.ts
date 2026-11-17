// Backend giả để xem giao diện trong trình duyệt: mở http://localhost:1420/?mock
// Chỉ được nạp ở bản dev (xem main.ts); bản phát hành không chứa tệp này.

import { emit } from '@tauri-apps/api/event';
import { mockIPC } from '@tauri-apps/api/mocks';
import { MODULES, type Finding, type FixReport, type ModuleId, type RescueRecord, type ScanReport } from '../lib/scan';

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
const apps = [
  { id: 'HKLM\\a', name: '7-Zip 23.01 (x64)', publisher: 'Igor Pavlov', version: '23.01', installDate: '2026-03-02', size: 5.6e6, installLocation: 'C:\\Program Files\\7-Zip', needsAdmin: true },
  { id: 'HKLM\\b', name: 'Old Game', publisher: 'Game Studio', version: '1.2', installDate: '2024-08-19', size: 42e9, installLocation: 'D:\\Games\\Old Game', needsAdmin: true },
  { id: 'HKCU\\c', name: 'Chat App', publisher: 'Chat Inc.', version: '5.0.1', installDate: '2026-11-10', size: 320e6, installLocation: null, needsAdmin: false },
];
const installedApps = new Set(apps.map((app) => app.id));
const leftovers = [
  { id: 'leftover:0', kind: 'folder', path: 'C:\\Program Files\\7-Zip', bytes: 1.2e6, recommended: true },
  { id: 'leftover:1', kind: 'folder', path: 'C:\\Users\\demo\\AppData\\Roaming\\7-Zip', bytes: 24e3, recommended: false },
  { id: 'leftover:2', kind: 'registry', path: 'HKCU\\Software\\7-Zip', bytes: 0, recommended: true },
];
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
    report.recordId = Date.now();
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

export function install(): void {
  mockIPC(
    (cmd, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case 'is_elevated':
          return false;
        case 'scan':
          return scan(args.modules as ModuleId[]);
        case 'cancel':
          cancelled = true;
          return;
        case 'fix':
          return fix(args.ids as string[]);
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
          const record = history.find((r) => r.id === args.id)!;
          const restored = record.undo.length;
          record.undo = [];
          record.undone = true;
          return { restored, failed: 0 };
        }
        case 'memory_status':
          return memory;
        case 'release_ram':
        case 'turbo_set': {
          if (cmd === 'turbo_set') turbo.on = args.on as boolean;
          if (cmd === 'turbo_set' && !args.on) return sleep(300).then(() => null);
          const before = { ...memory };
          memory = { ...memory, available: memory.available + 1.4e9, load: memory.load - 4 };
          return sleep(900).then(() => ({ before, after: memory, processes: 142 }));
        }
        case 'turbo_status':
          return { ...turbo };
        case 'startup_list':
          return startupItems;
        case 'startup_set': {
          const item = startupItems.find((i) => i.id === args.id)!;
          if (item.needsAdmin) throw 'needs_admin';
          item.enabled = args.enabled as boolean;
          return { ...item };
        }
        case 'apps_list':
          return sleep(400).then(() => apps.filter((app) => installedApps.has(app.id)));
        case 'app_uninstall':
          return (async () => {
            cancelled = false;
            for (let n = 0; n < 15; n++) {
              await sleep(100);
              if (cancelled) return { exitCode: null, stillInstalled: true };
            }
            installedApps.delete(args.id as string);
            return { exitCode: 0, stillInstalled: false };
          })();
        case 'leftovers_scan':
          if (installedApps.has(args.id as string)) throw 'still_installed';
          return sleep(600).then(() => leftovers);
        case 'leftovers_remove': {
          if (installedApps.has(args.id as string)) throw 'still_installed';
          const picked = leftovers.filter((l) => (args.ids as string[]).includes(l.id));
          return sleep(500).then(() => ({ done: picked.length, failed: 0, bytes: picked.reduce((s, l) => s + l.bytes, 0), recordId: Date.now() }));
        }
        case 'large_locations':
          return [
            { path: 'C:\\Users\\demo', kind: 'profile', free: 120e9, total: 500e9 },
            { path: 'C:\\', kind: 'drive', free: 120e9, total: 500e9 },
            { path: 'D:\\', kind: 'drive', free: 800e9, total: 1000e9 },
          ];
        case 'large_scan':
          return (async () => {
            for (let n = 1; n <= 5; n++) {
              await sleep(300);
              await emit('large-progress', { path: `C:\\Users\\demo\\Videos\\clip${n}.mp4`, count: n * 4000 });
            }
            return { files: largeFiles, cancelled: false };
          })();
        case 'large_recycle': {
          const paths = args.paths as string[];
          const picked = largeFiles.filter((f) => paths.includes(f.path));
          return sleep(400).then(() => ({ done: picked.length, failed: 0, bytes: picked.reduce((s, f) => s + f.size, 0), recordId: Date.now() }));
        }
        case 'reveal':
          return;
        case 'create_restore_point':
          throw 'needs_admin';
      }
    },
    { shouldMockEvents: true },
  );
}
