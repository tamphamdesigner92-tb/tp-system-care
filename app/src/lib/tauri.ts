/** true khi chạy trong cửa sổ Tauri, false khi mở bằng trình duyệt (vite dev). */
export function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export type WindowAction = 'minimize' | 'toggleMaximize' | 'close';

export async function windowAction(action: WindowAction): Promise<void> {
  if (!inTauri()) return;
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  await getCurrentWindow()[action]();
}

/** null = không xác định được (không chạy trong Tauri). */
export async function isElevated(): Promise<boolean | null> {
  if (!inTauri()) return null;
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<boolean>('is_elevated');
}
