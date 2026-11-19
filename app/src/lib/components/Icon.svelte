<script lang="ts" module>
  const paths = {
    care: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7zM5.7 5.7l3.8 3.8M14.5 14.5l3.8 3.8M18.3 5.7l-3.8 3.8M9.5 14.5l-3.8 3.8',
    clean:
      'M11 3l1.9 5.1L18 10l-5.1 1.9L11 17l-1.9-5.1L4 10l5.1-1.9zM18.5 15l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z',
    speed: 'M4.6 17.5a8.5 8.5 0 1 1 14.8 0M12 13.5l4-5.5M12 12.3a1.2 1.2 0 1 0 0 2.4 1.2 1.2 0 0 0 0-2.4z',
    protect: 'M12 3l7 2.8v5.4c0 4.5-3 8.2-7 9.8-4-1.6-7-5.3-7-9.8V5.8zM9 12l2.2 2.2 3.9-4.2',
    update:
      'M7.5 18a4.5 4.5 0 0 1-.7-8.95 6 6 0 0 1 11.6 1.5A3.75 3.75 0 0 1 17.5 18M12 20.5v-8M9 15l3-3 3 3',
    toolbox:
      'M5 4h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM15 4h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM5 14h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1zM15 14h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-4a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1z',
    apps:
      'M5 4h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM15 4h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM5 14h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1zM15 14h4a1 1 0 0 1 1 1v4a1 1 0 0 1-1 1h-4a1 1 0 0 1-1-1v-4a1 1 0 0 1 1-1z',
    search: 'M10.5 3.5a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM15.5 15.5l5 5',
    trash: 'M3 6h18M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2M5 6l1 13a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2l1-13M10 10v7M14 10v7',
    filter: 'M3 4h18l-7 8v7l-4 2v-9z',
    calendar: 'M5 5h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2zM8 3v4M16 3v4M3 10h18M7 14h2M15 14h2M7 17h2',
    clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2',
    package: 'M12 3l9 5v8l-9 5-9-5V8zM3 8l9 5 9-5M12 13v8M7.5 5.5l9 5V14',
    refresh: 'M20 7a9 9 0 0 0-15-1M20 3v4h-4M4 17a9 9 0 0 0 15 1M4 21v-4h4',
    chevronDown: 'M6 9l6 6 6-6',
    info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7.5v.01M11 11h1v6M10 17h4',
    drive: 'M3 14l3-9h12l3 9M5 13h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2zM7 17h.01M11 17h.01',
    minimize: 'M5 12h14',
    maximize: 'M6 6h12v12H6z',
    close: 'M6 6l12 12M18 6L6 18',
    check: 'M5 12.5l4.5 4.5L19 7.5',
    rescue: 'M4 12a8 8 0 1 0 2.6-5.9M4 4.5V8.5h4M12 8v4.5l3 1.8',
    back: 'M11 6l-6 6 6 6M5 12h14',
  } as const;

  export type IconName = keyof typeof paths;
</script>

<script lang="ts">
  let { name, size = 20 }: { name: IconName; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={paths[name]} />
</svg>
