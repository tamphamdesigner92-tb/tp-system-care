import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

// Bản dev mở bằng trình duyệt với `?mock` sẽ dùng backend giả để xem giao diện.
if (import.meta.env.DEV && new URLSearchParams(location.search).has('mock')) {
  (await import('./dev/mock')).install();
}

const app = mount(App, { target: document.getElementById('app')! });

export default app;
