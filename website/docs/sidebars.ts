import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    'index',
    {
      type: 'category',
      label: 'Operator guides',
      collapsed: false,
      items: ['deployment', 'mcp', 'debian-package'],
    },
    {
      type: 'category',
      label: 'Verification',
      collapsed: false,
      items: ['pedant-report', 'pedant-extended-report', 'remaining-work'],
    },
  ],
};

export default sidebars;
