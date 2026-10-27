import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const html = fs.readFileSync(path.join(root, 'dist/index.html'), 'utf8');
const $ = cheerio.load(html);

test('page shell: title, description, skip link, main landmark', () => {
  assert.equal($('title').text(), 'Microwave Server — Get cooking without becoming a chef.');
  assert.match($('meta[name="description"]').attr('content'), /independent, open-source Chef Infra Server API implementation in Rust/i);
  assert.equal($('a.skip-link').attr('href'), '#main');
  assert.equal($('main#main').length, 1);
});

test('hero and navigation', () => {
  assert.equal($('h1').text().trim(), 'Get cooking without becoming a chef.');
  assert.match($('main').text(), /independent, open-source Chef Infra Server API implementation written in Rust/i);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 1, true);
  assert.equal($('a[href="https://github.com/rmoriz/microwave-development#run-locally"]').length >= 1, true);
  assert.match($('body').text(), /Cinc Client 19/);
  assert.match($('body').text(), /Chef Infra Client 18\/19/);
  assert.match($('body').text(), /AGPL-3\.0/);
  assert.match($('picture img').attr('alt'), /rainbow neon microwave/i);
  assert.equal($('body').text().includes('docker run'), false);
});
