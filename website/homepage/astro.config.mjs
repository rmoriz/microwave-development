import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

export default defineConfig({
  site: 'https://microwave.io',
  integrations: [sitemap()],
  build: { inlineStylesheets: 'auto' },
});
