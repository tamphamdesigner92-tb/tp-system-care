<script lang="ts">
  import Icon from '../lib/components/Icon.svelte';
  import Uninstaller from '../lib/components/Uninstaller.svelte';
  import Tuning from '../lib/components/Tuning.svelte';
  import InstallMonitor from '../lib/components/InstallMonitor.svelte';
  import { t, type Key } from '../lib/i18n.svelte';

  /** Công cụ đang mở; `null` là lưới công cụ. */
  let { tool = $bindable(null) }: { tool: 'uninstaller' | 'browser' | 'monitor' | null } = $props();

  const upcoming: Key[] = [
    'toolbox.lock',
    'toolbox.shredder',
    'toolbox.undelete',
    'toolbox.diskCheck',
    'toolbox.processes',
    'toolbox.repair',
    'toolbox.hardware',
  ];
</script>

<section class="toolbox">
  {#if tool === 'uninstaller'}
    <header>
      <button class="link" onclick={() => (tool = null)}><Icon name="back" size={14} /> {t('nav.toolbox')}</button>
      <h1>{t('toolbox.uninstaller')}</h1>
    </header>
    <Uninstaller />
  {:else if tool === 'browser' || tool === 'monitor'}
    <header>
      <button class="link" onclick={() => (tool = null)}><Icon name="back" size={14} /> {t('nav.toolbox')}</button>
      <h1>{t(tool === 'browser' ? 'toolbox.browser' : 'toolbox.monitor')}</h1>
    </header>
    {#if tool === 'browser'}<Tuning area="browser" />{:else}<InstallMonitor />{/if}
  {:else}
    <h1>{t('nav.toolbox')}</h1>
    <div class="grid">
      <button class="tool" onclick={() => (tool = 'uninstaller')}>
        <span class="tool-name">{t('toolbox.uninstaller')}</span>
        <span class="tool-desc">{t('toolbox.uninstallerDesc')}</span>
      </button>
      <button class="tool" onclick={() => (tool = 'browser')}>
        <span class="tool-name">{t('toolbox.browser')}</span>
        <span class="tool-desc">{t('toolbox.browserDesc')}</span>
      </button>
      <button class="tool" onclick={() => (tool = 'monitor')}>
        <span class="tool-name">{t('toolbox.monitor')}</span>
        <span class="tool-desc">{t('toolbox.monitorDesc')}</span>
      </button>
      {#each upcoming as key (key)}
        <div class="tool off">
          <span class="tool-name">{t(key)}</span>
          <span class="tool-desc">{t('page.soon')}</span>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .toolbox {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 24px 16px;
  }

  header {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
  }

  .link {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    color: var(--accent-2);
  }

  .grid {
    overflow-y: auto;
    min-height: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 10px;
  }

  .tool {
    min-height: 84px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 16px;
    border-radius: var(--radius);
    background: var(--surface);
    border: 1px solid var(--border);
    text-align: left;
    transition: background 0.15s;
  }

  button.tool:hover {
    background: var(--surface-hover);
  }

  .tool.off {
    background: none;
    border-style: dashed;
    color: var(--text-faint);
  }

  .tool-name {
    font-size: 14px;
    font-weight: 600;
  }

  .tool.off .tool-name {
    font-weight: 400;
  }

  .tool-desc {
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-dim);
  }

  .tool.off .tool-desc {
    color: var(--text-faint);
  }
</style>
