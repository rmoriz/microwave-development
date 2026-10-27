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

test('proof strip and Cinc section', () => {
  const text = $('body').text();
  assert.match(text, /3,284/);
  assert.match(text, /extended Pedant examples passing/);
  assert.match(text, /compatibility shims/);
  assert.match(text, /Debian, Ubuntu, and Fedora/);
  assert.match(text, /Cinc Client 19/);
  assert.match(text, /ships an end-to-end Compose stack|tested with Cinc Client 19/i);
  assert.equal($('a[href="https://cinc.sh/"]').length >= 1, true);
  assert.match(text, /not affiliated with or endorsed by the Cinc Project/i);
  assert.doesNotMatch(text, /official partner/i);
});

test('compatibility, under the hood, and known gaps', () => {
  const text = $('body').text();
  assert.equal($('#compatibility').length, 1);
  assert.equal($('#under-the-hood').length, 1);
  assert.match(text, /Same recipe\. New kitchen\./);
  assert.match(text, /X-Ops-Sign/);
  assert.match(text, /knife/);
  assert.match(text, /chef-server-ctl/);
  assert.equal($('#under-the-hood details').length, 6);
  for (const phrase of [
    'No automatic `chef-server.rb` importer',
    'password reset',
    'No LDAP/Active Directory bind',
    'Multi-replica HA and automated failover are not validated',
    'relational scan',
  ]) {
    assert.equal(text.includes(phrase.replaceAll('`', '')), true, phrase);
  }
});

test('migration, support, and FAQ', () => {
  const text = $('body').text();
  assert.match(text, /knife ec backup/i);
  assert.match(text, /from-api/);
  assert.match(text, /cannot copy password hashes or the admin flag/i);
  assert.match(text, /migration help/i);
  assert.match(text, /deployment review/i);
  assert.match(text, /ongoing support/i);
  assert.equal($('#support').length, 1);
  assert.equal($('#faq').length, 1);
  assert.equal($('#faq details').length, 8);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 2, true);
});

test('final CTA, footer, and legal pages', () => {
  const text = $('body').text();
  assert.match(text, /Bring your own cookbooks\./);
  assert.match(text, /not affiliated with or endorsed by Progress Software Corporation/i);
  assert.match(text, /trademarks of Progress Software Corporation/i);
  assert.match(text, /AGPL-3\.0-only/);
  assert.match(text, /No cookies, no analytics, no third-party requests/i);
  assert.equal($('a[href="/privacy/"]').length, 1);
  assert.equal($('a[href="/imprint/"]').length, 1);
  assert.doesNotMatch(text, /cheaper than|drop the license|end of life|official partner/i);

  for (const page of ['privacy', 'imprint']) {
    const pageHtml = fs.readFileSync(path.join(root, `dist/${page}/index.html`), 'utf8');
    assert.match(pageHtml, /hello@microwave\.io/);
  }
});
