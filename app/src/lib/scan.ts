import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** Các hạng mục engine đã hỗ trợ. */
export const MODULES = [
  'junk',
  'privacy',
  'shortcut',
  'registry',
  'startup',
  'system',
  'internet',
  'disk',
  'vuln',
  'security',
] as const;
export type ModuleId = (typeof MODULES)[number];

export interface Finding {
  id: string;
  module: ModuleId;
  /** Loại mục, tra nhãn bằng khóa `kind.<kind>`. */
  kind: string;
  name: string | null;
  detail: string | null;
  count: number;
  bytes: number;
  recommended: boolean;
  undoable: boolean;
  needsAdmin: boolean;
}

export interface ModuleResult {
  module: ModuleId;
  findings: Finding[];
}

export interface ScanReport {
  modules: ModuleResult[];
  cancelled: boolean;
}

export type ScanProgress =
  | { type: 'moduleStarted'; module: ModuleId }
  | { type: 'path'; path: string }
  | { type: 'moduleFinished'; module: ModuleId; count: number; bytes: number };

export interface FixItem {
  id: string;
  module: ModuleId | null;
  done: number;
  failed: number;
  bytes: number;
  manual: number;
  error: string | null;
}

export interface FixReport {
  items: FixItem[];
  done: number;
  failed: number;
  bytes: number;
  /** Số mục đã mở trang cài đặt Windows để người dùng tự hoàn tất. */
  manual: number;
  undoable: number;
  recordId: number | null;
}

export type AdviceReason = 'never' | 'pending' | 'stale' | 'lowDisk' | 'routine';

/** Một hạng mục Chế độ AI chọn quét, kèm lý do. */
export interface Advice {
  module: ModuleId;
  reason: AdviceReason;
}

export interface Health {
  /** 0–100; null khi chưa quét hạng mục nào. */
  score: number | null;
  scanned: number;
  total: number;
  issues: number;
}

export interface RescueRecord {
  id: number;
  /** Giây tính từ mốc Unix. */
  time: number;
  modules: ModuleId[];
  /** Thao tác của công cụ ngoài luồng quét, ví dụ `uninstall:7-Zip`. */
  title?: string;
  done: number;
  failed: number;
  bytes: number;
  /** Các bước hoàn tác còn lại. */
  undo: unknown[];
  undoable: number;
  undone: boolean;
}

export interface UndoReport {
  restored: number;
  failed: number;
}

export async function scan(modules: ModuleId[], onProgress: (p: ScanProgress) => void): Promise<ScanReport> {
  const unlisten = await listen<ScanProgress>('scan-progress', (event) => onProgress(event.payload));
  try {
    return await invoke<ScanReport>('scan', { modules });
  } finally {
    unlisten();
  }
}

export function cancel(): Promise<void> {
  return invoke('cancel');
}

export async function fix(ids: string[], onProgress: (current: number, total: number) => void): Promise<FixReport> {
  const unlisten = await listen<{ current: number; total: number }>('fix-progress', (event) =>
    onProgress(event.payload.current, event.payload.total),
  );
  try {
    return await invoke<FixReport>('fix', { ids });
  } finally {
    unlisten();
  }
}

export function advise(): Promise<Advice[]> {
  return invoke('advise');
}

export function health(): Promise<Health> {
  return invoke('health');
}

export function rescueHistory(): Promise<RescueRecord[]> {
  return invoke('rescue_history');
}

export function rescueUndo(id: number): Promise<UndoReport> {
  return invoke('rescue_undo', { id });
}

export function createRestorePoint(): Promise<void> {
  return invoke('create_restore_point');
}
