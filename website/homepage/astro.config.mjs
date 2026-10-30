import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import { BASE_PATH, SITE_URL } from './src/data/config.ts';

export default defineConfig({
  site: SITE_URL,
  base: BASE_PATH,
  integrations: [sitemap()],
  build: { inlineStylesheets: 'auto' },
});
