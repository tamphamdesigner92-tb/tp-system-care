import { i18n } from './i18n.svelte';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

function locale(): string {
  return i18n.lang === 'vi' ? 'vi-VN' : 'en-US';
}

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toLocaleString(locale(), { maximumFractionDigits: digits })} ${UNITS[unit]}`;
}

export function formatCount(n: number): string {
  return n.toLocaleString(locale());
}

export function formatTime(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleString(locale(), { dateStyle: 'medium', timeStyle: 'short' });
}
