<script lang="ts">
  import LargeFiles from '../lib/components/LargeFiles.svelte';
  import DuplicateFiles from '../lib/components/DuplicateFiles.svelte';
  import ComponentStore from '../lib/components/ComponentStore.svelte';
  import { t } from '../lib/i18n.svelte';

  let tab = $state<'large' | 'duplicates' | 'components'>('large');
</script>

<section class="clean-host">
  <nav aria-label={t('nav.clean')}>
    <button class="btn" class:primary={tab === 'large'} aria-pressed={tab === 'large'} onclick={() => (tab = 'large')}>{t('clean.title')}</button>
    <button class="btn" class:primary={tab === 'duplicates'} aria-pressed={tab === 'duplicates'} onclick={() => (tab = 'duplicates')}>{t('duplicate.title')}</button>
    <button class="btn" class:primary={tab === 'components'} aria-pressed={tab === 'components'} onclick={() => (tab = 'components')}>{t('component.title')}</button>
  </nav>
  <div class="large" class:off={tab !== 'large'}><LargeFiles /></div>
  {#if tab === 'duplicates'}<div class="content"><DuplicateFiles /></div>
  {:else if tab === 'components'}<div class="content"><ComponentStore /></div>{/if}
</section>

<style>
  .clean-host { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 12px; }
  nav { display: flex; flex-wrap: wrap; gap: 8px; padding: 4px 24px 0; }
  .large { display: contents; }
  .large.off { display: none; }
  .content { flex: 1; min-height: 0; display: flex; padding: 0 24px 16px; }
</style>
