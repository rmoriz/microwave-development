import type { APIRoute } from 'astro';
import { BASE_PATH, SITE_URL } from '../data/config';

export const GET: APIRoute = () => {
  const sitemap = new URL(`${BASE_PATH}/sitemap-index.xml`, SITE_URL);
  const docsSitemap = new URL(`${BASE_PATH}/docs/sitemap.xml`, SITE_URL);
  return new Response(
    `User-agent: *\nAllow: /\n\nSitemap: ${sitemap.href}\nSitemap: ${docsSitemap.href}\n`,
    { headers: { 'Content-Type': 'text/plain; charset=utf-8' } },
  );
};
