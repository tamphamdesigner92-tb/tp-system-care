import type { AppInfo } from './tools';

export type AppSort = 'name' | 'size' | 'date' | 'lastUsed';
export type AppFilter = 'all' | 'desktop' | 'store' | 'builtIn';
export const APP_SIGNALS = ['large', 'recent', 'unused', 'unknown', 'peers'] as const;
export type AppSignal = typeof APP_SIGNALS[number];

export function knownUsage(value: number | null): value is number {
  return value !== null && Number.isFinite(value) && value >= 0;
}

// Compare local calendar dates, not elapsed hours (DST days need not be 24 hours).
function calendarDay(value: string | null): number | null {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
  const parsed = new Date(`${value}T00:00:00Z`);
  if (!Number.isFinite(parsed.getTime()) || parsed.toISOString().slice(0, 10) !== value) return null;
  return parsed.getTime() / 86400000;
}

export function appSignals(app: AppInfo, nowSeconds: number): AppSignal[] {
  const now = new Date(nowSeconds * 1000);
  const today = Date.UTC(now.getFullYear(), now.getMonth(), now.getDate()) / 86400000;
  const installed = calendarDay(app.installDate);
  const known = knownUsage(app.lastUsed);
  return APP_SIGNALS.filter((signal) => {
    switch (signal) {
      case 'large': return app.size !== null && app.size >= 500 * 1024 * 1024;
      case 'recent': return installed !== null && today - installed >= 0 && today - installed <= 30;
      case 'unused': return known && nowSeconds - app.lastUsed! >= 90 * 86400;
      case 'unknown': return !known;
      case 'peers': return app.installedTogether.some((id) => id !== app.id);
    }
  });
}

export function filterApps(
  apps: AppInfo[], query: string, filter: AppFilter, advanced: Partial<Record<AppSignal, boolean>>,
  sort: AppSort, nowSeconds: number, direction?: 'asc' | 'desc',
): AppInfo[] {
  const q = query.trim().toLowerCase();
  const list = apps.filter((app) =>
    (filter === 'all' || (filter === 'builtIn' ? app.builtIn : app.kind === filter)) &&
    (!q || app.name.toLowerCase().includes(q) || (app.publisher ?? '').toLowerCase().includes(q)) &&
    APP_SIGNALS.filter((signal) => advanced[signal]).every((signal) => appSignals(app, nowSeconds).includes(signal)),
  );
  const factor = (direction ?? (sort === 'size' || sort === 'date' ? 'desc' : 'asc')) === 'asc' ? 1 : -1;
  return list.sort((a, b) => {
    const byName = a.name.localeCompare(b.name);
    if (sort === 'size') {
      if ((a.size === null) !== (b.size === null)) return a.size === null ? 1 : -1;
      return ((a.size ?? 0) - (b.size ?? 0)) * factor || byName;
    }
    if (sort === 'date') {
      if ((a.installDate === null) !== (b.installDate === null)) return a.installDate === null ? 1 : -1;
      return (a.installDate ?? '').localeCompare(b.installDate ?? '') * factor || byName;
    }
    if (sort === 'lastUsed') {
      const aKnown = knownUsage(a.lastUsed); const bKnown = knownUsage(b.lastUsed);
      if (aKnown !== bKnown) return aKnown ? -1 : 1;
      return aKnown && bKnown ? (a.lastUsed! - b.lastUsed!) * factor || byName : byName;
    }
    return byName * factor;
  });
}
