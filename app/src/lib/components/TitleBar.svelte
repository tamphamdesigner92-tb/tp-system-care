<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n, setLang, t } from '../i18n.svelte';
  import { windowAction } from '../tauri';

  let { rescueOpen, onrescue }: { rescueOpen: boolean; onrescue: () => void } = $props();
</script>

<header class="titlebar" data-tauri-drag-region>
  <div class="controls">
    <button
      class="ctl"
      class:active={rescueOpen}
      title={t('win.rescue')}
      aria-label={t('win.rescue')}
      aria-pressed={rescueOpen}
      onclick={onrescue}
    >
      <Icon name="rescue" size={17} />
    </button>
    <button
      class="lang"
      title={t('win.language')}
      aria-label={t('win.language')}
      onclick={() => setLang(i18n.lang === 'vi' ? 'en' : 'vi')}
    >
      {i18n.lang === 'vi' ? 'VI' : 'EN'}
    </button>
    <button class="ctl" title={t('win.minimize')} aria-label={t('win.minimize')} onclick={() => windowAction('minimize')}>
      <Icon name="minimize" size={16} />
    </button>
    <button
      class="ctl"
      title={t('win.maximize')}
      aria-label={t('win.maximize')}
      onclick={() => windowAction('toggleMaximize')}
    >
      <Icon name="maximize" size={14} />
    </button>
    <button class="ctl close" title={t('win.close')} aria-label={t('win.close')} onclick={() => windowAction('close')}>
      <Icon name="close" size={16} />
    </button>
  </div>
</header>

<style>
  .titlebar {
    height: var(--titlebar-h);
    flex: none;
    display: flex;
    justify-content: flex-end;
    align-items: stretch;
  }

  .controls {
    display: flex;
    align-items: stretch;
  }

  .ctl,
  .lang {
    width: 44px;
    display: grid;
    place-items: center;
    color: var(--text-dim);
    transition:
      background 0.15s,
      color 0.15s;
  }

  .lang {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }

  .ctl:hover,
  .lang:hover {
    background: var(--surface-hover);
    color: var(--text);
  }

  .ctl.active {
    color: var(--accent-2);
  }

  .ctl.close:hover {
    background: var(--danger);
    color: #fff;
  }
</style>
