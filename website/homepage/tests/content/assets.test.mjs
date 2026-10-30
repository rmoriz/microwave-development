import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

test('OG image is 1200x630', async () => {
  const meta = await sharp(path.join(root, 'public/og.png')).metadata();
  assert.equal(meta.width, 1200);
  assert.equal(meta.height, 630);
});

test('largest hero variant is under 300KB', () => {
  const files = fs.readdirSync(path.join(root, 'dist/_astro'));
  const heroes = files.filter((file) => /microwave-hero.*\.(avif|webp)$/.test(file));
  assert.ok(heroes.length > 0);
  for (const file of heroes) {
    const size = fs.statSync(path.join(root, 'dist/_astro', file)).size;
    assert.ok(size < 300 * 1024, `${file} is ${size} bytes`);
  }
});

test('the MCP recording is self-hosted, small, and anonymized', () => {
  const cast = path.join(root, 'public/demo/mcp-agent.cast');
  const size = fs.statSync(cast).size;
  assert.ok(size < 400 * 1024, `mcp-agent.cast is ${size} bytes`);

  const lines = fs.readFileSync(cast, 'utf8').trim().split('\n');
  const header = JSON.parse(lines[0]);
  assert.equal(header.version, 3);
  assert.equal(header.term.cols, 80);
  assert.ok(lines.length > 2);

  assert.doesNotMatch(lines.join('\n'), /rmoriz|utopia|\/Users\//);
  assert.match(lines.join('\n'), /workstation\.local/);
});
