import { defineConfig, devices } from '@playwright/test';
import { BASE_PATH } from './src/data/config';

export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  use: {
    baseURL: 'http://127.0.0.1:4321',
    trace: 'retain-on-failure',
  },
  webServer: {
    command:
      'npm run build && rm -rf .preview && mkdir -p .preview/microwave-development && cp -R dist/. .preview/microwave-development/ && python3 -m http.server 4321 --bind 127.0.0.1 --directory .preview',
    url: `http://127.0.0.1:4321${BASE_PATH}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
});
