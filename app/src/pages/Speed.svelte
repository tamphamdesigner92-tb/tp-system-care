<script lang="ts">
  import Icon from '../lib/components/Icon.svelte';
  import Toggle from '../lib/components/Toggle.svelte';
  import { formatBytes } from '../lib/format';
  import { t, tOr, type Key } from '../lib/i18n.svelte';
  import { inTauri } from '../lib/tauri';
  import {
    freed,
    memoryStatus,
    releaseRam,
    startupList,
    startupSet,
    turboSet,
    turboStatus,
    type Memory,
    type StartupItem,
    type Turbo,
  } from '../lib/tools';

  let { onuninstall }: { onuninstall: () => void } = $props();

  let memory = $state<Memory | null>(null);
  let turbo = $state<Turbo | null>(null);
  let items = $state<StartupItem[]>([]);
  let working = $state<'ram' | 'turbo' | string | null>(null);
  let message = $state<{ text: string; ok: boolean } | null>(null);

  const enabledCount = $derived(items.filter((i) => i.enabled).length);
  const errorText = (error: unknown) => tOr(`err.${error}`, String(error));

  async function load() {
    if (!inTauri()) return;
    [memory, turbo, items] = await Promise.all([memoryStatus(), turboStatus(), startupList()]);
  }

  async function ram() {
    working = 'ram';
    try {
      const report = await releaseRam();
      memory = report.after;
      message = { text: t('speed.ramDone', { size: formatBytes(freed(report)) }), ok: true };
    } catch (error) {
      message = { text: errorText(error), ok: false };
    } finally {
      working = null;
    }
  }

  async function toggleTurbo() {
    if (!turbo) return;
    working = 'turbo';
    try {
      const report = await turboSet(!turbo.on);
      turbo = await turboStatus();
      if (report) {
        memory = report.after;
        message = { text: t('speed.turboOnDone', { size: formatBytes(freed(report)) }), ok: true };
      } else {
        message = { text: t('speed.turboOffDone'), ok: true };
      }
    } catch (error) {
      message = { text: errorText(error), ok: false };
    } finally {
      working = null;
    }
  }

  async function toggleStartup(item: StartupItem) {
    working = item.id;
    try {
      const updated = await startupSet(item.id, !item.enabled);
      items = items.map((i) => (i.id === updated.id ? updated : i));
    } catch (error) {
      message = { text: errorText(error), ok: false };
    } finally {
      working = null;
    }
  }

  load();
</script>

<section class="speed">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else}
    <div class="cards">
      <div class="card">
        <div class="card-head">
          <h2>{t('speed.turbo')}</h2>
          <Toggle
            checked={turbo?.on ?? false}
            disabled={!turbo?.available || working !== null}
            label={t('speed.turbo')}
            onchange={toggleTurbo}
          />
        </div>
        <p class="desc">
          {turbo && !turbo.available ? t('speed.turboUnavailable') : t('speed.turboDesc')}
        </p>
      </div>

      <div class="card">
        <div class="card-head">
          <h2>{t('speed.ram')}</h2>
          <button class="btn small" disabled={working !== null} onclick={ram}>
            {working === 'ram' ? t('speed.ramWorking') : t('speed.ramAction')}
          </button>
        </div>
        {#if memory}
          <div class="meter" role="meter" aria-valuenow={memory.load} aria-valuemin={0} aria-valuemax={100}>
            <span style:width="{memory.load}%" class:high={memory.load >= 80}></span>
          </div>
          <p class="desc">
            {t('speed.ramUsage', {
              load: memory.load,
              free: formatBytes(memory.available),
              total: formatBytes(memory.total),
            })}
          </p>
        {/if}
        <p class="desc faint">{t('speed.ramNote')}</p>
      </div>
    </div>

    {#if message}
      <p class="message" class:bad={!message.ok} role="status">{message.text}</p>
    {/if}

    <div class="list-head">
      <h2>{t('speed.startup')} <span class="count">{enabledCount}/{items.length}</span></h2>
      <button class="link" onclick={onuninstall}>{t('speed.openUninstaller')} →</button>
    </div>
    <ul class="list">
      {#each items as item (item.id)}
        <li class:off={!item.enabled}>
          <div class="text">
            <span class="name">{item.name}</span>
            <span class="detail" title={item.command}>
              {t(`startup.loc.${item.location}` as Key)} · {item.command}
            </span>
          </div>
          {#if item.broken}<span class="tag warn">{t('startup.brokenTag')}</span>{/if}
          <Toggle
            checked={item.enabled}
            disabled={working !== null}
            label={item.name}
            onchange={() => toggleStartup(item)}
          />
        </li>
      {:else}
        <li class="none">{t('speed.startupEmpty')}</li>
      {/each}
    </ul>
    <p class="note"><Icon name="check" size={12} /> {t('speed.startupNote')}</p>
  {/if}
</section>

<style>
  .speed {
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
    color: var(--text-faint);
  }

  .cards {
    flex: none;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
    border-radius: var(--radius);
    background: var(--surface);
    border: 1px solid var(--border);
  }

  .card-head,
  .list-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .desc {
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-dim);
  }

  .faint {
    color: var(--text-faint);
  }

  .meter {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-active);
    overflow: hidden;
  }

  .meter span {
    display: block;
    height: 100%;
    background: var(--accent-grad);
    transition: width 0.4s ease;
  }

  .meter span.high {
    background: var(--warn);
  }

  .btn.small {
    height: 30px;
    padding: 0 14px;
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

  .count {
    margin-left: 6px;
    font-size: 12px;
    font-weight: 400;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .link {
    font-size: 13px;
    color: var(--accent-2);
  }

  .link:hover {
    text-decoration: underline;
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

  .list li.off .name {
    color: var(--text-dim);
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

  .note {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
