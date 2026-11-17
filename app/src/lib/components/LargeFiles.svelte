<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatBytes, formatCount, formatTime } from '../format';
  import { t, tOr, type Key } from '../i18n.svelte';
  import { cancel } from '../scan';
  import { inTauri } from '../tauri';
  import {
    largeLocations,
    largeRecycle,
    largeScan,
    reveal,
    type LargeFile,
    type Location,
    type RemoveReport,
  } from '../tools';

  const SIZES = [100, 500, 1024];

  let locations = $state<Location[]>([]);
  let root = $state('');
  let minMb = $state(500);
  let phase = $state<'idle' | 'scanning' | 'results'>('idle');
  let progress = $state({ path: '', count: 0 });
  let files = $state<LargeFile[]>([]);
  let cancelled = $state(false);
  let picked = $state<Record<string, boolean>>({});
  let report = $state<RemoveReport | null>(null);
  let busy = $state(false);
  let error = $state('');

  const chosen = $derived(files.filter((f) => picked[f.path]));
  const chosenBytes = $derived(chosen.reduce((sum, f) => sum + f.size, 0));
  const errorText = (e: unknown) => tOr(`err.${e}`, String(e));

  function locationLabel(l: Location): string {
    const name = l.kind === 'profile' ? t('clean.profile') : l.path;
    return l.total > 0 ? `${name} · ${t('clean.free', { size: formatBytes(l.free) })}` : name;
  }

  async function load() {
    if (!inTauri()) return;
    locations = await largeLocations();
    root = locations[0]?.path ?? '';
  }

  async function start() {
    phase = 'scanning';
    report = null;
    error = '';
    progress = { path: '', count: 0 };
    try {
      const result = await largeScan(root, minMb, (path, count) => (progress = { path, count }));
      files = result.files;
      cancelled = result.cancelled;
      // Không chọn sẵn tệp nào: người dùng tự quyết định.
      picked = {};
      phase = 'results';
    } catch (e) {
      error = errorText(e);
      phase = 'idle';
    }
  }

  async function recycle() {
    busy = true;
    try {
      report = await largeRecycle(chosen.map((f) => f.path));
      const removed = new Set(chosen.map((f) => f.path));
      // Backend cũng bỏ các tệp này khỏi kết quả; tệp xóa không được sẽ hiện lại khi quét lại.
      files = files.filter((f) => !removed.has(f.path));
      picked = {};
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  load();
</script>

<section class="clean">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else}
    <header>
      <div>
        <h1>{t('clean.title')}</h1>
        <p class="desc">{t('clean.desc')}</p>
      </div>
    </header>

    <div class="controls">
      <label>
        <span>{t('clean.where')}</span>
        <select bind:value={root} disabled={phase === 'scanning'}>
          {#each locations as l (l.path)}
            <option value={l.path}>{locationLabel(l)}</option>
          {/each}
        </select>
      </label>
      <label>
        <span>{t('clean.minSize')}</span>
        <select bind:value={minMb} disabled={phase === 'scanning'}>
          {#each SIZES as size (size)}
            <option value={size}>{formatBytes(size * 1024 * 1024)}</option>
          {/each}
        </select>
      </label>
      {#if phase === 'scanning'}
        <button class="btn" onclick={() => cancel()}>{t('scan.stop')}</button>
      {:else}
        <button class="btn primary" disabled={!root} onclick={start}>{t('clean.find')}</button>
      {/if}
    </div>

    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if report}
      <p class="message" class:bad={report.failed > 0} role="status">
        {t('clean.recycled', { n: formatCount(report.done), size: formatBytes(report.bytes) })}
        {#if report.failed > 0}{t('clean.recycleFailed', { n: formatCount(report.failed) })}{/if}
      </p>
    {/if}

    {#if phase === 'scanning'}
      <div class="progress">
        <span class="spinner" aria-hidden="true"></span>
        <p>{t('clean.scanning', { n: formatCount(progress.count) })}</p>
        <p class="path" title={progress.path}>{progress.path}</p>
      </div>
    {:else if phase === 'results'}
      <div class="list-head">
        <span class="desc">
          {t('clean.found', { n: formatCount(files.length) })}{cancelled ? ` · ${t('result.cancelled')}` : ''}
        </span>
        <button class="btn primary small" disabled={busy || chosen.length === 0} onclick={recycle}>
          {t('clean.recycle', { n: formatCount(chosen.length), size: formatBytes(chosenBytes) })}
        </button>
      </div>
      <ul class="list">
        {#each files as f (f.path)}
          <li>
            <button class="check" role="checkbox" aria-label={t('clean.selectFile', { path: f.path })} aria-checked={!!picked[f.path]} disabled={busy} onclick={() => (picked[f.path] = !picked[f.path])}>
              <span class="box" class:on={picked[f.path]}><Icon name="check" size={12} /></span>
            </button>
            <div class="text">
              <span class="name" title={f.path}>{f.path.split('\\').pop()}</span>
              <span class="detail" title={f.path}>
                {t(`clean.cat.${f.category}` as Key)} · {formatTime(f.modified)} · {f.path}
              </span>
            </div>
            {#if f.appData}<span class="tag warn" title={t('clean.appDataHint')}>{t('clean.appData')}</span>{/if}
            <button class="link" onclick={() => reveal(f.path)}>{t('tool.reveal')}</button>
            <span class="amount">{formatBytes(f.size)}</span>
          </li>
        {:else}
          <li class="none">{t('clean.none')}</li>
        {/each}
      </ul>
      <p class="note">{t('clean.note')}</p>
    {:else}
      <p class="empty">{t('clean.hint')}</p>
    {/if}
  {/if}
</section>

<style>
  .clean {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 24px 16px;
  }

  .empty {
    flex: 1;
    display: grid;
    place-items: center;
    margin: 0;
    padding: 0 40px;
    text-align: center;
    line-height: 1.5;
    color: var(--text-faint);
  }

  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
  }

  .desc {
    margin: 4px 0 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  .controls {
    flex: none;
    display: flex;
    align-items: flex-end;
    gap: 12px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-dim);
  }

  label:first-child {
    flex: 1;
    min-width: 0;
  }

  select {
    height: 36px;
    padding: 0 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: #1a2040;
    color: var(--text);
    font: inherit;
    font-size: 13px;
  }

  .message {
    flex: none;
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: rgba(70, 201, 139, 0.12);
    font-size: 13px;
  }

  .message.bad {
    background: rgba(240, 179, 69, 0.12);
    color: var(--warn);
  }

  .progress {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    font-size: 13px;
  }

  .progress p {
    margin: 0;
  }

  .spinner {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    border: 3px solid var(--surface-active);
    border-top-color: var(--accent);
    animation: spin 0.9s linear infinite;
  }

  .path {
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text-faint);
  }

  .list-head {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .list-head .desc {
    margin: 0;
  }

  .btn.small {
    height: 30px;
    padding: 0 14px;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    margin: 0;
    padding: 0 6px 0 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .list li {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 46px;
    padding: 6px 12px;
    border-radius: var(--radius);
    background: var(--surface);
  }

  .list li.none {
    justify-content: center;
    color: var(--text-faint);
    font-size: 13px;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .name,
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .name {
    font-size: 13px;
  }

  .detail {
    font-size: 12px;
    color: var(--text-faint);
  }

  .tag {
    flex: none;
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--border);
    font-size: 11px;
    white-space: nowrap;
  }

  .tag.warn {
    border-color: rgba(240, 179, 69, 0.4);
    color: var(--warn);
  }

  .link {
    flex: none;
    font-size: 12px;
    color: var(--accent-2);
  }

  .link:hover {
    text-decoration: underline;
  }

  .amount {
    flex: none;
    min-width: 70px;
    text-align: right;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .check {
    flex: none;
    display: grid;
    place-items: center;
  }

  .box {
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    border: 1.5px solid var(--text-faint);
    color: transparent;
  }

  .box.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .note {
    flex: none;
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
