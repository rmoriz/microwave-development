import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BASE_PATH, SITE_URL } from '../../src/data/config.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const config = fs.readFileSync(
  path.join(root, '../docs/docusaurus.config.ts'),
  'utf8',
);

const escapeDots = (value) => value.replaceAll('.', '\\.');

test('the docs site lives under the homepage base path and origin', () => {
  assert.match(config, new RegExp(`url: '${escapeDots(SITE_URL)}'`));
  assert.match(config, new RegExp(`baseUrl: '${escapeDots(`${BASE_PATH}/docs/`)}'`));
});
