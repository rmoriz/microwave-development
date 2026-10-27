import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const dist = path.join(root, 'dist');

function walk(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    return entry.isDirectory() ? walk(full) : [full];
  });
}

function routeFor(file) {
  const rel = path.relative(dist, file).split(path.sep).join('/');
  if (rel === 'index.html') return '/';
  if (rel.endsWith('/index.html')) return `/${rel.slice(0, -'index.html'.length)}`;
  return `/${rel}`;
}

const pages = walk(dist).filter((file) => file.endsWith('.html'));
const pageMap = new Map(pages.map((file) => [routeFor(file), file]));

function fileForRoute(route) {
  return pageMap.get(route) ?? pageMap.get(`${route}/`) ?? pageMap.get(`${route}.html`);
}

test('all internal links and fragments resolve; external links are secure', () => {
  assert.ok(pages.length >= 3, 'expected the homepage and legal pages');

  for (const page of pages) {
    const from = routeFor(page);
    const $ = cheerio.load(fs.readFileSync(page, 'utf8'));

    $('a[href]').each((_, element) => {
      const href = $(element).attr('href');
      const where = `${from} → ${href}`;

      assert.ok(href, `empty href at ${where}`);
      assert.notEqual(href, '#', `placeholder href at ${where}`);
      assert.ok(!href.startsWith('javascript:'), `javascript href at ${where}`);
      assert.ok(!href.startsWith('http://'), `insecure external href at ${where}`);

      if (href.startsWith('https://') || href.startsWith('mailto:') || href.startsWith('tel:')) {
        return;
      }

      assert.ok(href.startsWith('/') || href.startsWith('#'), `unexpected href at ${where}`);

      const [rawPath, hash] = href.split('#');
      const targetRoute = rawPath === '' ? from : rawPath;
      const targetFile = fileForRoute(targetRoute);
      assert.ok(targetFile, `missing target at ${where}`);

      if (hash) {
        const $target = cheerio.load(fs.readFileSync(targetFile, 'utf8'));
        const found = $target('[id]')
          .toArray()
          .some((node) => $target(node).attr('id') === hash);
        assert.ok(found, `missing fragment #${hash} at ${where}`);
      }
    });
  }
});
