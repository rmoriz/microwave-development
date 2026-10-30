import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';
import { BASE_PATH, SITE_URL } from '../../src/data/config.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const $ = cheerio.load(fs.readFileSync(path.join(root, 'dist/index.html'), 'utf8'));

const home = `${SITE_URL}${BASE_PATH}/`;

test('SEO metadata and JSON-LD', () => {
  assert.equal($('link[rel="canonical"]').attr('href'), home);
  assert.equal($('meta[property="og:image"]').attr('content'), `${home}og.png`);
  assert.equal($('meta[name="twitter:image"]').attr('content'), `${home}og.png`);
  assert.equal($('meta[name="twitter:card"]').attr('content'), 'summary_large_image');
  const json = JSON.parse($('script[type="application/ld+json"]').text());
  assert.equal(json['@type'], 'SoftwareApplication');
  assert.equal(json.name, 'Microwave Server');
  assert.equal(json.license, 'https://github.com/rmoriz/microwave-development/blob/main/LICENSE');
});

test('theme colour matches the steel-blue background', () => {
  assert.equal($('meta[name="theme-color"]').attr('content'), '#2a4d6e');
});

test('sitemap and robots exist', () => {
  assert.equal(fs.existsSync(path.join(root, 'dist/sitemap-index.xml')), true);
  const robots = fs.readFileSync(path.join(root, 'dist/robots.txt'), 'utf8');
  const origin = SITE_URL.replaceAll('.', '\\.');
  assert.match(robots, new RegExp(`Sitemap: ${origin}${BASE_PATH}/sitemap-index.xml`));
  assert.match(robots, new RegExp(`Sitemap: ${origin}${BASE_PATH}/docs/sitemap.xml`));
});

test('every sitemap URL sits under the base path', () => {
  const sitemap = fs.readFileSync(path.join(root, 'dist/sitemap-0.xml'), 'utf8');
  const locs = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1]);
  assert.ok(locs.length >= 3, 'the sitemap lists the pages');
  for (const loc of locs) {
    assert.ok(loc.startsWith(`${SITE_URL}${BASE_PATH}/`), loc);
  }
});

test('404 page exists and is noindex', () => {
  const html = fs.readFileSync(path.join(root, 'dist/404.html'), 'utf8');
  const page = cheerio.load(html);
  assert.match(page('body').text(), /This page went cold\./);
  assert.equal(page('meta[name="robots"]').attr('content'), 'noindex');
});
