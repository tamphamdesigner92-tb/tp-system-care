<script lang="ts">
  import { formatBytes, formatCount, formatTime } from '../lib/format';
  import { t, tOr } from '../lib/i18n.svelte';
  import { createRestorePoint, rescueHistory, rescueUndo, type RescueRecord } from '../lib/scan';
  import { inTauri } from '../lib/tauri';

  let records = $state<RescueRecord[]>([]);
  let busy = $state(false);
  let loaded = $state(false);
  let message = $state<{ text: string; ok: boolean } | null>(null);

  const errorText = (error: unknown) => tOr(`rescue.err.${error}`, String(error));

  function describe(record: RescueRecord): string {
    if (record.title?.startsWith('uninstall:')) return t('rescue.uninstall', { name: record.title.slice(10) });
    if (record.title) {
      const title = /^(duplicateFiles|browserCache); /.test(record.title) ? record.title.split('; ', 1)[0] : record.title;
      return tOr(`rescue.${title}`, title);
    }
    return record.modules.map((m) => tOr(`item.${m}`, m)).join(', ') || t('rescue.unknown');
  }

  function recovery(record: RescueRecord): string[] {
    if (!record.title || !/^(duplicateFiles|browserCache); /.test(record.title)) return [];
    try {
      const value: unknown = JSON.parse(record.title.slice(record.title.indexOf('; ') + 2));
      return Array.isArray(value) && value.every((detail) => typeof detail === 'string') ? value : [];
    } catch {
      return [record.title];
    }
  }

  async function load() {
    if (!inTauri()) return;
    try {
      records = await rescueHistory();
      loaded = true;
    } catch (error) {
      loaded = false;
      message = { text: errorText(error), ok: false };
    }
  }

  async function undo(record: RescueRecord) {
    busy = true;
    try {
      const result = await rescueUndo(record.id);
      message = { text: t('rescue.undoResult', { ...result }), ok: result.failed === 0 };
      await load();
    } catch (error) {
      message = { text: errorText(error), ok: false };
    } finally {
      busy = false;
    }
  }

  async function restorePoint() {
    busy = true;
    message = { text: t('rescue.restoreWorking'), ok: true };
    try {
      await createRestorePoint();
      message = { text: t('rescue.restoreOk'), ok: true };
    } catch (error) {
      message = { text: errorText(error), ok: false };
    } finally {
      busy = false;
    }
  }

  load();
</script>

<section class="rescue">
  <header>
    <div>
      <h1>{t('rescue.title')}</h1>
      <p>{t('rescue.desc')}</p>
    </div>
    <button class="btn" disabled={busy || !inTauri()} onclick={restorePoint}>{t('rescue.restorePoint')}</button>
  </header>

  {#if message}
    <p class="message" class:bad={!message.ok} role="status">{message.text}</p>
  {/if}

  {#if !inTauri()}
    <p class="empty">{t('rescue.needApp')}</p>
  {:else if !loaded}
    {#if !message}<p class="empty">{t('uninstall.working')}</p>{/if}
  {:else if records.length === 0}
    <p class="empty">{t('rescue.empty')}</p>
  {:else}
    <ul>
      {#each records as record (record.id)}
        {@const pending = recovery(record)}
        <li>
          <div class="info">
            <span class="time">{formatTime(record.time)}</span>
            <span class="what">{describe(record)}</span>
            {#if pending.length}<details class="recovery"><summary>{t(pending.every((detail) => detail.startsWith('recycled: ')) ? 'maintenance.recycledLocation' : 'maintenance.recovery')}</summary>
              {#each pending as detail}<p>{detail}</p>{/each}
            </details>{/if}
          </div>
          <span class="amount">
            {t('rescue.items', { n: formatCount(record.done) })}{record.bytes > 0 ? ` · ${formatBytes(record.bytes)}` : ''}
          </span>
          {#if record.undo.length > 0}
            <button class="btn" disabled={busy} onclick={() => undo(record)}>
              {t('rescue.undo', { n: formatCount(record.undo.length) })}
            </button>
          {:else}
            <span class="tag">{record.undone ? t('rescue.undone') : t('rescue.permanent')}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .rescue {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 24px 20px;
  }

  header {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
  }

  header p {
    margin: 4px 0 0;
    font-size: 13px;
    color: var(--text-dim);
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

  .empty {
    flex: 1;
    display: grid;
    place-items: center;
    margin: 0;
    color: var(--text-faint);
  }

  ul {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    margin: 0;
    padding: 0 6px 0 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 14px;
    min-height: 52px;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: var(--surface);
  }

  .info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .time {
    font-size: 13px;
  }

  .what {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text-faint);
  }

  .recovery {
    color: var(--warn);
    font-size: 12px;
    overflow-wrap: anywhere;
    user-select: text;
  }

  .amount {
    flex: none;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .tag {
    flex: none;
    padding: 4px 12px;
    border-radius: 999px;
    border: 1px solid var(--border);
    font-size: 12px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  li .btn {
    flex: none;
    height: 30px;
    padding: 0 14px;
  }
</style>
