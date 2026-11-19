import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

// ------------------------------------------------------------ Tăng tốc

export interface Memory {
  total: number;
  available: number;
  /** Phần trăm RAM đang dùng. */
  load: number;
}

export interface RamReport {
  before: Memory;
  after: Memory;
  processes: number;
}

export interface Turbo {
  on: boolean;
  /** Máy có gói nguồn Hiệu năng cao hay không. */
  available: boolean;
}

export type StartupLocation = 'hkcuRun' | 'hklmRun' | 'hklmRun32' | 'userFolder' | 'commonFolder';

export interface StartupItem {
  id: string;
  name: string;
  command: string;
  location: StartupLocation;
  enabled: boolean;
  broken: boolean;
  needsAdmin: boolean;
}

export const memoryStatus = () => invoke<Memory>('memory_status');
export const releaseRam = () => invoke<RamReport>('release_ram');
export const turboStatus = () => invoke<Turbo>('turbo_status');
export const turboSet = (on: boolean) => invoke<RamReport | null>('turbo_set', { on });
export const startupList = () => invoke<StartupItem[]>('startup_list');
export const startupSet = (id: string, enabled: boolean) => invoke<StartupItem>('startup_set', { id, enabled });

/** RAM trống tăng thêm sau khi giải phóng. */
export const freed = (r: RamReport) => Math.max(0, r.after.available - r.before.available);

// ------------------------------------------------------------ Gỡ ứng dụng

export type AppKind = 'desktop' | 'store';
export type UninstallMode = 'standard' | 'forced';

export interface AppInfo {
  id: string;
  kind: AppKind;
  builtIn: boolean;
  removable: boolean;
  name: string;
  publisher: string | null;
  version: string | null;
  /** YYYY-MM-DD */
  installDate: string | null;
  size: number | null;
  /** Last observed use, Unix seconds; null means unknown, not unused. */
  lastUsed: number | null;
  /** Peer app IDs installed on the same date; a weak signal, not proof of bundling. */
  installedTogether: string[];
  installLocation: string | null;
  needsAdmin: boolean;
  /** Giây Unix, gần đúng thời điểm cài (lần ghi cuối của khóa Uninstall). */
  installedAt: number | null;
  /** Ứng dụng được cài trong cùng khoảng thời gian; có thể là phần mềm đi kèm. */
  companions: string[];
}

export interface UninstallResult {
  exitCode: number | null;
  stillInstalled: boolean;
}

export interface Leftover {
  id: string;
  kind: 'folder' | 'registry' | 'uninstallEntry' | 'shortcut' | 'startup' | 'service' | 'task';
  path: string;
  bytes: number;
  recommended: boolean;
}

export interface RemoveReport {
  done: number;
  failed: number;
  bytes: number;
  recordId: number | null;
}

export interface UninstallPreparation {
  ready: boolean;
  restoreError: string | null;
}

export interface UninstallProgress {
  current: number;
  total: number;
  name: string;
}

export interface BatchUninstallItem {
  id: string;
  name: string;
  result: UninstallResult | null;
  error: string | null;
}

export interface BatchUninstallReport {
  items: BatchUninstallItem[];
  pending: string[];
  cancelled: boolean;
}

export const appsList = () => invoke<AppInfo[]>('apps_list');
export const appUninstall = (id: string) => invoke<UninstallResult>('app_uninstall', { id });
export const forceTarget = (name: string, path: string) => invoke<AppInfo>('app_force_target', { name, path });
export const uninstallPrepare = (ids: string[], mode: UninstallMode, continueWithoutRestore = false) =>
  invoke<UninstallPreparation>('uninstall_prepare', { ids, mode, continueWithoutRestore });
export const leftoversScan = (id: string, mode: UninstallMode = 'standard') => invoke<Leftover[]>('leftovers_scan', { id, mode });
export const leftoversRemove = (id: string, ids: string[], mode: UninstallMode = 'standard') =>
  invoke<RemoveReport>('leftovers_remove', { id, ids, mode });

export async function appsUninstall(ids: string[], onProgress: (progress: UninstallProgress) => void): Promise<BatchUninstallReport> {
  const unlisten = await listen<UninstallProgress>('uninstall-progress', (e) => onProgress(e.payload));
  try {
    return await invoke<BatchUninstallReport>('apps_uninstall', { ids });
  } finally {
    unlisten();
  }
}

// ------------------------------------------------------------ Tiện ích trình duyệt

export type ExtensionBrowser = 'chrome' | 'edge' | 'brave' | 'firefox';
export type ExtensionSource = 'store' | 'unpacked' | 'external' | 'policy' | 'other';
export type ExtensionRemovalKind = 'registry' | 'browser' | 'none';

export interface BrowserExtension {
  id: string;
  browser: ExtensionBrowser;
  profile: string | null;
  extensionId: string;
  name: string;
  version: string | null;
  source: ExtensionSource;
  enabled: boolean | null;
  removal: ExtensionRemovalKind;
  registry: string[];
  needsAdmin: boolean;
}

export interface ExtensionScan {
  extensions: BrowserExtension[];
  skipped: number;
}

export interface ExtensionRemoval {
  done: number;
  failed: number;
  recordId: number | null;
  errors: string[];
}

export const extensionsList = () => invoke<ExtensionScan>('extensions_list');
export const extensionsRemove = (ids: string[]) => invoke<ExtensionRemoval>('extensions_remove', { ids });
export const extensionManage = (id: string) => invoke<void>('extension_manage', { id });

// ------------------------------------------------------------ Bản cập nhật Windows

export interface WindowsUpdate {
  id: string;
  description: string | null;
  installedOn: string | null;
}

export interface UpdateResult {
  exitCode: number | null;
  restartRequired: boolean;
  stillInstalled: boolean;
}

export const updatesList = () => invoke<WindowsUpdate[]>('updates_list');
export const updatePrepare = (id: string, continueWithoutRestore = false) =>
  invoke<UninstallPreparation>('update_prepare', { id, continueWithoutRestore });
export const updateUninstall = (id: string) => invoke<UpdateResult>('update_uninstall', { id });
export const updatesHistory = () => invoke<void>('updates_history');

// ------------------------------------------------------------ Tệp lớn

export interface Location {
  path: string;
  kind: 'profile' | 'drive';
  free: number;
  total: number;
}

export type FileCategory = 'video' | 'archive' | 'image' | 'installer' | 'other';

export interface LargeFile {
  path: string;
  size: number;
  modified: number;
  category: FileCategory;
  /** Nằm trong AppData: thường là dữ liệu của một ứng dụng. */
  appData: boolean;
}

export interface LargeScan {
  files: LargeFile[];
  cancelled: boolean;
}

export const largeLocations = () => invoke<Location[]>('large_locations');
export const largeRecycle = (paths: string[]) => invoke<RemoveReport>('large_recycle', { paths });
export const reveal = (path: string) => invoke<void>('reveal', { path });

export async function largeScan(
  root: string,
  minMb: number,
  onProgress: (path: string, count: number) => void,
): Promise<LargeScan> {
  const unlisten = await listen<{ path: string; count: number }>('large-progress', (e) =>
    onProgress(e.payload.path, e.payload.count),
  );
  try {
    return await invoke<LargeScan>('large_scan', { root, minMb });
  } finally {
    unlisten();
  }
}

export interface DuplicateFile {
  id: string;
  path: string;
  modified: number;
}

export interface DuplicateGroup {
  id: string;
  size: number;
  files: DuplicateFile[];
}

export interface DuplicateScan {
  groups: DuplicateGroup[];
  filesChecked: number;
  skipped: number;
  cancelled: boolean;
  truncated: boolean;
}

export interface DuplicateRemoval extends RemoveReport {
  removedIds: string[];
  cancelled: boolean;
  recovery: string[];
}

export interface ComponentAnalysis {
  actualSize: number;
  sharedSize: number;
  backupsSize: number;
  cacheSize: number;
  reclaimablePackages: number;
  cleanupRecommended: boolean;
  lastCleanup: string;
  output: string;
}

export interface ComponentCleanup {
  exitCode: number;
  rebootRequired: boolean;
  output: string;
}

export interface ComponentStatus {
  state: 'idle' | 'running' | 'completed' | 'failed';
  report: ComponentCleanup | null;
  error: string | null;
}

export interface Tweak {
  id: string;
  area: 'deep' | 'browser';
  active: boolean;
  available: boolean;
  reason: string | null;
  needsRestart: boolean;
}

export interface TweakReport {
  done: number;
  failed: number;
  recordId: number | null;
  needsRestart: boolean;
}

export interface BrowserCache {
  id: string;
  browser: 'chrome' | 'edge' | 'brave' | 'firefox' | 'vscode' | 'discord' | 'slack';
  path: string;
  bytes: number;
  files: number;
}

export interface BrowserCacheScan {
  caches: BrowserCache[];
  skipped: number;
  cancelled: boolean;
}

export interface BrowserCacheReport extends RemoveReport {
  cancelled: boolean;
  recovery: string[];
}

export interface MonitorSummary {
  id: number;
  name: string;
  root: string;
  vendor: string;
  startedAt: number;
  finishedAt: number | null;
  filesBefore: number;
  changeCount: number;
  complete: boolean;
}

export interface MonitorChange {
  kind: 'file' | 'registry';
  change: 'added' | 'modified' | 'removed';
  path: string;
  valueName: string | null;
}

export interface MonitorReport {
  session: MonitorSummary;
  changes: MonitorChange[];
  warnings: string[];
  truncated: boolean;
}

const pendingMaintenance = new Map<string, { cancelled: boolean }>();

async function maintenanceWork<T>(
  command: string,
  payload: Record<string, unknown>,
  requestId: string,
  onProgress: (path: string, count: number) => void,
): Promise<T> {
  const pending = { cancelled: false };
  pendingMaintenance.set(requestId, pending);
  let unlisten: (() => void) | undefined;
  try {
    unlisten = await listen<{ requestId: string; path: string; count: number }>('maintenance-progress', (e) => {
      if (e.payload.requestId === requestId) onProgress(e.payload.path, e.payload.count);
    });
    if (pending.cancelled) throw 'cancelled';
    return await invoke<T>(command, { ...payload, requestId });
  } finally {
    pendingMaintenance.delete(requestId);
    unlisten?.();
  }
}

export const maintenanceCancel = (requestId: string) => {
  const pending = pendingMaintenance.get(requestId);
  if (pending) pending.cancelled = true;
  return invoke<void>('maintenance_cancel', { requestId });
};
export const duplicateScan = (root: string, minMb: number, requestId: string, progress: (path: string, count: number) => void) =>
  maintenanceWork<DuplicateScan>('duplicate_scan', { root, minMb }, requestId, progress);
export const duplicateRecycle = (ids: string[], requestId: string) => invoke<DuplicateRemoval>('duplicate_recycle', { ids, requestId });
export const duplicateReveal = (id: string) => invoke<void>('duplicate_reveal', { id });
export const componentAnalyze = (requestId: string) => invoke<ComponentAnalysis>('component_analyze', { requestId });
export const componentPrepare = (continueWithoutRestore = false) => invoke<UninstallPreparation>('component_prepare', { continueWithoutRestore });
export const componentStart = (confirmed: boolean) => invoke<ComponentStatus>('component_start', { confirmed });
export const componentStatus = () => invoke<ComponentStatus>('component_status');
export const tuningList = () => invoke<Tweak[]>('tuning_list');
export const tuningApply = (ids: string[]) => invoke<TweakReport>('tuning_apply', { ids });
export const browserCacheScan = (requestId: string, progress: (path: string, count: number) => void) =>
  maintenanceWork<BrowserCacheScan>('browser_cache_scan', {}, requestId, progress);
export const browserCacheClear = (ids: string[], requestId: string) => invoke<BrowserCacheReport>('browser_cache_clear', { ids, requestId });
export const browserCacheReveal = (id: string) => invoke<void>('browser_cache_reveal', { id });
export const appCacheScan = (requestId: string, progress: (path: string, count: number) => void) =>
  maintenanceWork<BrowserCacheScan>('app_cache_scan', {}, requestId, progress);
export const appCacheClear = (ids: string[], requestId: string) => invoke<BrowserCacheReport>('app_cache_clear', { ids, requestId });
export const appCacheReveal = (id: string) => invoke<void>('app_cache_reveal', { id });

export interface Extension {
  id: string;
  browser: string;
  profile: string;
  name: string;
  version: string;
  enabled: boolean | null;
  managed: boolean;
  permissions: string[];
  /** Measured lower bound, not CPU/RAM; zero means unmeasured. */
  bytes: number;
}
export interface ReviewExtensionScan {
  extensions: Extension[];
  skipped: number;
  truncated: boolean;
}
export interface InstalledUpdate {
  id: string;
  title: string;
  kb: string[];
  uninstallable: boolean;
}
export const reviewExtensionsList = () => invoke<ReviewExtensionScan>('review_extensions_list');
/** Returns a fixed internal URI; does not launch a browser or change extensions. */
export const extensionManager = (browser: string) => invoke<string>('extension_manager', { browser });
export const windowsUpdatesList = (requestId: string) => invoke<InstalledUpdate[]>('windows_updates_list', { requestId });
/** Opens only the Windows installed-update removal page. */
export const windowsUpdatesOpen = () => invoke<void>('windows_updates_open');
export const browserFlushDns = () => invoke<void>('browser_flush_dns');
export const browserNetwork = () => invoke<void>('browser_network');
export const monitorList = () => invoke<MonitorSummary[]>('monitor_list');
export const monitorBegin = (name: string, root: string, vendor: string, requestId: string, progress: (path: string, count: number) => void) =>
  maintenanceWork<MonitorSummary>('monitor_begin', { name, root, vendor }, requestId, progress);
export const monitorFinish = (id: number, requestId: string, progress: (path: string, count: number) => void) =>
  maintenanceWork<MonitorReport>('monitor_finish', { id }, requestId, progress);
export const monitorDetail = (id: number) => invoke<MonitorReport>('monitor_detail', { id });

/**
 * Chạy một lệnh chỉ-đọc, thử lại khi backend đang bận với tác vụ khác (ví dụ danh
 * sách của tab vừa rời còn đang tải). `alive` trả false thì ngừng thử.
 */
export async function whenIdle<T>(work: () => Promise<T>, alive: () => boolean, attempts = 40): Promise<T> {
  for (let i = 1; ; i++) {
    try {
      return await work();
    } catch (e) {
      if (String(e) !== 'busy' || i >= attempts || !alive()) throw e;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  }
}
