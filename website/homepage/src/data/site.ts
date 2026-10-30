import { SITE_URL } from './config';
import { withBase } from '../lib/paths';

export const site = {
  name: 'Microwave Server',
  url: SITE_URL,
  title: 'Microwave Server — Get cooking without becoming a chef.',
  description:
    'Microwave Server is an independent, open-source Chef Infra Server API implementation in Rust. Works with Cinc Client, keeps your cookbooks and CLI, and is free to self-host under AGPL-3.0.',
  email: 'hello@microwave.io',
  github: 'https://github.com/rmoriz/microwave-development',
  runFromSource: 'https://github.com/rmoriz/microwave-development#run-locally',
  docs: withBase('/docs/'),
  mcp: 'https://github.com/rmoriz/microwave-development/blob/main/docs/mcp.md',
  license: 'https://github.com/rmoriz/microwave-development/blob/main/LICENSE',
  cinc: 'https://cinc.sh/',
  legal: {
    operator: '',
    address: '',
    email: 'hello@microwave.io',
  },
};
