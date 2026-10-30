import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    {type: 'doc', id: 'index', label: 'Overview'},
    {
      type: 'category',
      label: 'Operator guides',
      collapsed: false,
      items: [
        {type: 'doc', id: 'deployment', label: 'Deployment'},
        {type: 'doc', id: 'mcp', label: 'MCP endpoint'},
        {type: 'doc', id: 'debian-package', label: 'Debian packages'},
        {type: 'doc', id: 'http-parity', label: 'HTTP API parity'},
      ],
    },
    {
      type: 'category',
      label: 'Verification',
      collapsed: false,
      items: [
        {type: 'doc', id: 'pedant-report', label: 'Pedant results'},
        {
          type: 'doc',
          id: 'pedant-extended-report',
          label: 'Extended pedant results',
        },
        {type: 'doc', id: 'remaining-work', label: 'Remaining work'},
      ],
    },
  ],
};

export default sidebars;
