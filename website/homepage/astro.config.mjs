import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import { BASE_PATH, SITE_URL } from './src/data/config.ts';

export default defineConfig({
  site: SITE_URL,
  base: BASE_PATH,
  i18n: {
    defaultLocale: 'en',
    locales: ['en', 'de', 'fr', 'it', 'es', 'nl'],
    routing: {
      prefixDefaultLocale: false,
      fallbackType: 'rewrite',
    },
    fallback: {
      de: 'en',
      fr: 'en',
      it: 'en',
      es: 'en',
      nl: 'en',
    },
  },
  integrations: [sitemap()],
  build: { inlineStylesheets: 'auto' },
});
