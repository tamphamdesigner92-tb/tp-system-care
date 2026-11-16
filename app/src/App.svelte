<script lang="ts">
  import Sidebar from './lib/components/Sidebar.svelte';
  import TitleBar from './lib/components/TitleBar.svelte';
  import { pages, type PageId } from './lib/pages';
  import { isElevated } from './lib/tauri';
  import Care from './pages/Care.svelte';
  import Placeholder from './pages/Placeholder.svelte';
  import Rescue from './pages/Rescue.svelte';

  /** Rescue Center mở từ thanh tiêu đề, không nằm trong thanh bên. */
  let view = $state<PageId | 'rescue'>('care');
  let elevated = $state<boolean | null>(null);

  const page = $derived(pages.find((p) => p.id === view));

  isElevated()
    .then((value) => (elevated = value))
    .catch(() => (elevated = null));
</script>

<div class="shell">
  <Sidebar current={view === 'rescue' ? null : view} {elevated} onselect={(id) => (view = id)} />
  <main>
    <TitleBar rescueOpen={view === 'rescue'} onrescue={() => (view = view === 'rescue' ? 'care' : 'rescue')} />
    <!-- Trang Chăm sóc luôn được giữ lại để lần quét đang chạy không mất khi chuyển trang. -->
    <div class="keep" class:off={view !== 'care'}>
      <Care {elevated} onrescue={() => (view = 'rescue')} />
    </div>
    {#if view === 'rescue'}
      <Rescue />
    {:else if page && view !== 'care'}
      {#key view}
        <Placeholder {page} />
      {/key}
    {/if}
  </main>
</div>

<style>
  .shell {
    height: 100%;
    display: flex;
  }

  main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-main);
  }

  .keep {
    display: contents;
  }

  .keep.off {
    display: none;
  }
</style>
