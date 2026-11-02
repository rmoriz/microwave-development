import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const html = readFileSync(new URL('../build/index.html', import.meta.url), 'utf8');

// The renderer escapes the single quotes of the policy to `&#39;` (the HTML
// parser decodes them again) and may reorder the attributes, so the lookups go
// through the attribute and the value is decoded before matching.
function metaContent(httpEquiv) {
  const tag = new RegExp(`<meta[^>]*http-equiv="${httpEquiv}"[^>]*>`).exec(html);
  assert.ok(tag, `${httpEquiv} meta`);
  const content = /content="([^"]*)"/.exec(tag[0]);
  assert.ok(content, `${httpEquiv} content`);
  return content[1].replaceAll('&#39;', "'");
}

test('the docs head carries the security metas and the favicon', () => {
  const csp = metaContent('Content-Security-Policy');
  assert.match(csp, /default-src 'self'/);
  assert.match(csp, /script-src 'self' 'unsafe-inline'/);
  assert.match(csp, /img-src 'self' data:/);
  assert.match(csp, /frame-ancestors 'none'/);
  assert.doesNotMatch(csp, /'unsafe-eval'/);
  assert.equal(metaContent('X-Content-Type-Options'), 'nosniff');
  assert.equal(metaContent('X-XSS-Protection'), '1');
  assert.equal(metaContent('Referrer-Policy'), 'no-referrer');
  assert.match(html, /<link[^>]*rel="icon"[^>]*img\/favicon\.svg/);
});
