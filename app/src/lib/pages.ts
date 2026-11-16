import type { Key } from './i18n.svelte';
import type { IconName } from './components/Icon.svelte';

export type PageId = 'care' | 'clean' | 'speed' | 'protect' | 'update' | 'toolbox';

export interface PageDef {
  id: PageId;
  icon: IconName;
  label: Key;
  /** Mô tả cho các trang chưa xây dựng. */
  desc?: Key;
}

export const pages: PageDef[] = [
  { id: 'care', icon: 'care', label: 'nav.care' },
  { id: 'clean', icon: 'clean', label: 'nav.clean', desc: 'page.clean.desc' },
  { id: 'speed', icon: 'speed', label: 'nav.speed', desc: 'page.speed.desc' },
  { id: 'protect', icon: 'protect', label: 'nav.protect', desc: 'page.protect.desc' },
  { id: 'update', icon: 'update', label: 'nav.update', desc: 'page.update.desc' },
  { id: 'toolbox', icon: 'toolbox', label: 'nav.toolbox', desc: 'page.toolbox.desc' },
];
