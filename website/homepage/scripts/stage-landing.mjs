// Stages the server's embedded landing page into the preview root.
//
// The page is served at `/` with root-absolute asset URLs, so it lands at
// `.preview/index.html` and its assets at the preview root — never under
// `.preview${BASE_PATH}/`, which keeps the site's own copy intact.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repo = path.resolve(root, '../..');
const assets = path.join(repo, 'crates/microwave-api/assets');
const preview = path.join(root, '.preview');

/** [source, destination] — destinations are relative to the preview root. */
const files = [
  [path.join(assets, 'landing/index.html'), 'index.html'],
  [path.join(assets, 'landing/fonts.css'), 'css/fonts.css'],
  [path.join(assets, 'landing/landing.css'), 'css/landing.css'],
  [path.join(assets, 'landing/favicon.svg'), 'favicon.svg'],
  [path.join(assets, 'fonts/inter-latin.woff2'), 'fonts/inter-latin.woff2'],
  [path.join(assets, 'fonts/space-grotesk-latin.woff2'), 'fonts/space-grotesk-latin.woff2'],
  [path.join(root, 'src/styles/tokens.css'), 'css/tokens.css'],
  [path.join(root, 'src/styles/global.css'), 'css/global.css'],
  [path.join(root, 'src/styles/components.css'), 'css/components.css'],
];

const missing = files.filter(([source]) => !fs.existsSync(source)).map(([source]) => source);

if (missing.length > 0) {
  for (const source of missing) {
    console.error(`stage-landing: missing source ${source}`);
  }
  process.exit(1);
}

for (const [source, target] of files) {
  const destination = path.join(preview, target);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.copyFileSync(source, destination);
}

// The server substitutes this placeholder from `landing::WEBSITE_URL`; the
// staged copy needs the same so its links are real.
const WEBSITE_URL = 'https://rmoriz.github.io/microwave-development/';
const page = path.join(preview, 'index.html');
fs.writeFileSync(page, fs.readFileSync(page, 'utf8').replaceAll('{{website_url}}', WEBSITE_URL));

console.log(`Staged landing page and ${files.length - 1} assets into ${preview}`);
