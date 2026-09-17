import { defineConfig } from 'astro/config';
import node from '@astrojs/node';

export default defineConfig({
  output: 'server',
  adapter: node({ mode: 'standalone' }),
  server: { port: 4321, host: true },
  // Компресію вимикаємо з обох боків: міряємо рендер, а не gzip.
  compressHTML: false,
});
