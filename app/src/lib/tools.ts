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

export interface AppInfo {
  id: string;
  name: string;
  publisher: string | null;
  version: string | null;
  /** YYYY-MM-DD */
  installDate: string | null;
  size: number | null;
  installLocation: string | null;
  needsAdmin: boolean;
}

export interface UninstallResult {
  exitCode: number | null;
  stillInstalled: boolean;
}

export interface Leftover {
  id: string;
  kind: 'folder' | 'registry' | 'uninstallEntry';
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

export const appsList = () => invoke<AppInfo[]>('apps_list');
export const appUninstall = (id: string) => invoke<UninstallResult>('app_uninstall', { id });
export const leftoversScan = (id: string) => invoke<Leftover[]>('leftovers_scan', { id });
export const leftoversRemove = (id: string, ids: string[]) => invoke<RemoveReport>('leftovers_remove', { id, ids });

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
