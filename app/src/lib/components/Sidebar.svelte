<script lang="ts">
  import Icon from './Icon.svelte';
  import { t } from '../i18n.svelte';
  import { pages, type PageId } from '../pages';

  let {
    current,
    elevated,
    onselect,
  }: { current: PageId | null; elevated: boolean | null; onselect: (id: PageId) => void } = $props();
</script>

<aside class="sidebar">
  <div class="brand" data-tauri-drag-region>
    <div class="name">TP System Care</div>
    <div class="edition">{t('app.edition')}</div>
  </div>

  <nav>
    {#each pages as page (page.id)}
      <button class="item" class:active={current === page.id} title={t(page.label)} onclick={() => onselect(page.id)}>
        <span class="icon"><Icon name={page.icon} size={22} /></span>
        <span class="label">{t(page.label)}</span>
      </button>
    {/each}
  </nav>

  <div class="status">
    <span class="dot" class:ok={elevated === true} class:warn={elevated === false}></span>
    <span>
      {t('admin.label')}:
      {elevated === null ? t('admin.unknown') : elevated ? t('admin.yes') : t('admin.no')}
    </span>
  </div>
</aside>

<style>
  .sidebar {
    width: var(--sidebar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-side);
    padding: 0 8px 12px;
  }

  .brand {
    padding: 12px 8px 16px;
  }

  .brand > * {
    pointer-events: none;
  }

  .name {
    font-size: 13px;
    font-weight: 600;
  }

  .edition {
    margin-top: 2px;
    font-size: 12px;
    font-style: italic;
    color: var(--accent-2);
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }

  .item {
    height: 40px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 10px;
    border-radius: var(--radius);
    color: var(--text-dim);
    text-align: left;
    transition:
      background 0.15s,
      color 0.15s;
  }

  .item:hover {
    background: var(--surface-hover);
    color: var(--text);
  }

  .item.active {
    background: var(--surface-active);
    color: var(--text);
    font-weight: 600;
  }

  .icon {
    flex: none;
    display: grid;
    place-items: center;
  }

  .label {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 13px;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 0;
    font-size: 12px;
    color: var(--text-faint);
  }

  .dot {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--text-faint);
  }

  .dot.ok {
    background: var(--ok);
  }

  .dot.warn {
    background: var(--warn);
  }
</style>
