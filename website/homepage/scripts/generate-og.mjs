import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.join(root, 'src/assets/microwave-hero.png');
const target = path.join(root, 'public/og.png');

if (!fs.existsSync(source)) {
  throw new Error(`Missing hero image: ${source}`);
}

const overlay = Buffer.from(`
<svg width="1200" height="630" viewBox="0 0 1200 630" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="fade" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#0a0a0b" stop-opacity="0.12" />
      <stop offset="0.5" stop-color="#0a0a0b" stop-opacity="0.55" />
      <stop offset="1" stop-color="#0a0a0b" stop-opacity="0.96" />
    </linearGradient>
    <linearGradient id="bar" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#ff3ea5" />
      <stop offset="0.25" stop-color="#ffe14d" />
      <stop offset="0.5" stop-color="#4dff9e" />
      <stop offset="0.75" stop-color="#3ddcff" />
      <stop offset="1" stop-color="#8b5cff" />
    </linearGradient>
  </defs>
  <rect width="1200" height="630" fill="url(#fade)" />
  <rect x="64" y="44" width="112" height="4" rx="2" fill="url(#bar)" />
  <text x="64" y="118" fill="#a3a39b" font-family="Arial, Helvetica, sans-serif" font-size="18" letter-spacing="4">MICROWAVE SERVER</text>
  <text x="64" y="286" fill="#f4f1e8" font-family="Arial, Helvetica, sans-serif" font-size="68" font-weight="700" letter-spacing="-2">Get cooking</text>
  <text x="64" y="366" fill="#f4f1e8" font-family="Arial, Helvetica, sans-serif" font-size="68" font-weight="700" letter-spacing="-2">without becoming</text>
  <text x="64" y="446" fill="#f4f1e8" font-family="Arial, Helvetica, sans-serif" font-size="68" font-weight="700" letter-spacing="-2">a chef.</text>
  <text x="64" y="566" fill="#a3a39b" font-family="Arial, Helvetica, sans-serif" font-size="20">Independent · Open source · Rust</text>
  <text x="1136" y="566" text-anchor="end" fill="#a3a39b" font-family="Arial, Helvetica, sans-serif" font-size="20">microwave.io</text>
</svg>
`);

await sharp(source)
  .resize(1200, 630, { fit: 'cover', position: 'centre' })
  .modulate({ brightness: 0.72, saturation: 0.9 })
  .composite([{ input: overlay }])
  .png()
  .toFile(target);

console.log(`Wrote ${target}`);
