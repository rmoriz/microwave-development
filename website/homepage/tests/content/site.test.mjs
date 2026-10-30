import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';
import { BASE_PATH } from '../../src/data/config.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const html = fs.readFileSync(path.join(root, 'dist/index.html'), 'utf8');
const $ = cheerio.load(html);

test('page shell: title, description, skip link, main landmark', () => {
  assert.equal($('title').text(), 'Microwave Server — Get cooking without becoming a chef.');
  assert.match($('meta[name="description"]').attr('content'), /independent, open-source Chef Infra Server API implementation in Rust/i);
  assert.equal($('a.skip-link').attr('href'), '#main');
  assert.equal($('main#main').length, 1);
});

test('every page carries exactly one decorative grid layer', () => {
  assert.equal($('.grid-field').length, 1);
  for (const name of ['privacy/index.html', 'imprint/index.html', '404.html']) {
    const page = cheerio.load(
      fs.readFileSync(path.join(root, 'dist', name), 'utf8'),
    );
    assert.equal(page('.grid-field[aria-hidden="true"]').length, 1, name);
  }
});

test('hero and navigation', () => {
  assert.equal($('h1').text().trim(), 'Get cooking without becoming a chef.');
  assert.match($('main').text(), /independent, open-source Chef Infra Server API implementation written in Rust/i);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 1, true);
  assert.equal($('a[href="https://github.com/rmoriz/microwave-development#run-locally"]').length >= 1, true);
  assert.match($('body').text(), /Cinc Client 19/);
  assert.match($('body').text(), /Chef Infra Client 18\/19/);
  assert.match($('body').text(), /AGPL-3\.0/);
  assert.match($('body').text(), /Agent-ready/);
  assert.match($('body').text(), /Experimental MCP for AI agents/);
  assert.equal($(`.nav-desktop a[href="${BASE_PATH}/#agents"]`).length, 1);
  assert.equal($(`.nav-mobile a[href="${BASE_PATH}/#agents"]`).length, 1);
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
  assert.match(text, /tested with Cinc Client 19 on Debian, Ubuntu, and Fedora/i);
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
  assert.equal($('#under-the-hood details').length, 7);
  assert.match(text, /Experimental MCP endpoint/);
  assert.match(text, /MCP-capable AI agents/);
  assert.match(text, /Agents in the kitchen\./);
  assert.match(text, /MCP is experimental: no durable audit table/);
  assert.match(text, /Tool calls are only structured log lines/);
  assert.match(text, /one structured log line per call/);
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

test('known gaps link to catalogued claims', () => {
  const claims = fs.readFileSync(path.resolve(root, '../../docs/claims.toml'), 'utf8');
  const ids = $('.gaps-list a')
    .map((_, a) => $(a).attr('href').split('#')[1])
    .get();
  assert.ok(ids.length >= 7, 'every known gap carries a claim link');
  for (const id of ids) assert.match(claims, new RegExp(`id = "${id}"`));
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
  assert.equal($('#faq details').length, 9);
  assert.match(text, /Can an AI agent operate the server\?/);
  assert.match(text, /not a built-in assistant/);
  assert.match(text, /superuser default skips them/);
  assert.match(text, /only server log lines, not a durable audit table/);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 2, true);
});

test('agent MCP sales section', () => {
  const text = $('body').text();
  const guide = 'https://github.com/rmoriz/microwave-development/blob/main/docs/mcp.md';

  assert.equal($('#agents').length, 1);
  assert.equal($('#product').nextAll('#agents').length, 1, '#agents follows #product');
  assert.equal($('#agents').nextAll('#compatibility').length, 1, '#agents precedes #compatibility');
  assert.match(text, /AI-enhanced cooking/);
  assert.match(text, /Your sous-chef speaks MCP\./);
  assert.match(text, /Microwave does not ship a model\. It ships the kitchen:/);
  assert.match(text, /Bring your own agent/);
  assert.match(text, /Experimental/);
  assert.match(text, /POST \/organizations\/<org>\/mcp/);
  assert.match(text, /Why did web-01 fail\?/);
  assert.match(text, /What is drifting\?/);
  assert.match(text, /Prepare the change\./);
  assert.match(text, /Use a scoped user: the superuser default skips ACLs/);
  assert.match(text, /Writes are on by default/);
  assert.match(text, /writes = false/);
  assert.match(text, /warns if a write-enabled organization still uses the superuser default/);
  assert.equal(
    $('#agents .agent-note').text().trim(),
    'Bring your own agent. Microwave provides the endpoint and the permissions; the model stays on your side.',
  );
  assert.equal($(`#agents a[href="${guide}"]`).length, 1);
  assert.equal($('#agents h2').length, 1);
  assert.equal($('#agents h3').length, 3);
  assert.equal($('#agents ol.agent-grid > li').length, 3);
  assert.doesNotMatch(text, /AI-powered|built-in AI|autonomous|self-healing/);
  assert.equal(
    fs.existsSync(path.join(root, '../../docs/mcp.md')),
    true,
    'docs/mcp.md exists in the repository',
  );

  const guideText = fs.readFileSync(path.join(root, '../../docs/mcp.md'), 'utf8');
  assert.match(guideText, /mcp-remote/, 'Claude Desktop uses the stdio bridge');
  assert.match(guideText, /keep_runs/, 'the guide documents report pruning');
  assert.match(guideText, /one structured log line per call/);
});

test('final CTA, footer, and legal pages', () => {
  const text = $('body').text();
  assert.match(text, /Bring your own cookbooks\./);
  assert.match(text, /Then bring your own agent\./);
  assert.match(text, /not affiliated with or endorsed by Progress Software Corporation/i);
  assert.match(text, /trademarks of Progress Software Corporation/i);
  assert.match(text, /AGPL-3\.0-only/);
  assert.match(text, /No cookies, no analytics, no third-party requests/i);
  assert.equal($(`a[href="${BASE_PATH}/privacy/"]`).length, 1);
  assert.equal($(`a[href="${BASE_PATH}/imprint/"]`).length, 1);
  // Footer nav; the Migration section links to the same docs page.
  assert.equal($(`.footer-nav a[href="${BASE_PATH}/docs/"]`).length, 1);
  assert.doesNotMatch(text, /cheaper than|drop the license|end of life|official partner|invented tiers/i);
  assert.equal($('link[rel="preload"][as="image"]').length, 1);

  for (const page of ['privacy', 'imprint']) {
    const pageHtml = fs.readFileSync(path.join(root, `dist/${page}/index.html`), 'utf8');
    const pageDoc = cheerio.load(pageHtml);
    assert.match(pageHtml, /hello@microwave\.io/);
    assert.equal(pageDoc('header.site-header').length, 1);
  }
});
