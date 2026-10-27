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
