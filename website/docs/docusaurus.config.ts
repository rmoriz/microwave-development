import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';
import githubRelativeLinks from './plugins/github-relative-links.mjs';

const config: Config = {
  title: 'Microwave Server',
  tagline: 'A Rust reimplementation of the Chef Infra Server API',
  favicon: 'img/favicon.svg',

  url: 'https://microwave.io',
  baseUrl: '/docs/',
  organizationName: 'rmoriz',
  projectName: 'microwave-development',

  onBrokenLinks: 'throw',
  // `docs/remaining-work.md` marks its gap anchors with raw `<a id="...">`
  // elements, which Docusaurus does not count as headings; the anchors work,
  // so warn instead of failing the build.
  onBrokenAnchors: 'warn',
  onDuplicateRoutes: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          // One source of truth: the guides stay in the repository's `docs/`
          // folder, the site renders a curated subset of them.
          path: '../../docs',
          routeBasePath: '/',
          sidebarPath: './sidebars.ts',
          include: [
            'index.md',
            'deployment.md',
            'mcp.md',
            'debian-package.md',
            'remaining-work.md',
            'pedant-report.md',
            'pedant-extended-report.md',
          ],
          remarkPlugins: [githubRelativeLinks],
          editUrl:
            'https://github.com/rmoriz/microwave-development/edit/main/docs/',
        },
        blog: false,
        pages: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/favicon.svg',
    colorMode: {
      defaultMode: 'dark',
      disableSwitch: false,
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'Microwave Server',
      logo: {
        alt: 'Microwave Server',
        src: 'img/favicon.svg',
      },
      items: [
        {type: 'doc', docId: 'index', label: 'Overview', position: 'left'},
        {
          type: 'doc',
          docId: 'deployment',
          label: 'Deploy',
          position: 'left',
        },
        {type: 'doc', docId: 'mcp', label: 'MCP', position: 'left'},
        {href: 'https://microwave.io', label: 'microwave.io', position: 'right'},
        {
          href: 'https://github.com/rmoriz/microwave-development',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Guides',
          items: [
            {label: 'Deployment', to: '/deployment'},
            {label: 'MCP endpoint', to: '/mcp'},
            {label: 'Debian packages', to: '/debian-package'},
          ],
        },
        {
          title: 'Verification',
          items: [
            {label: 'Pedant results', to: '/pedant-report'},
            {label: 'Extended results', to: '/pedant-extended-report'},
            {label: 'Remaining work', to: '/remaining-work'},
          ],
        },
        {
          title: 'Project',
          items: [
            {
              label: 'microwave.io',
              href: 'https://microwave.io',
            },
            {
              label: 'Source',
              href: 'https://github.com/rmoriz/microwave-development',
            },
          ],
        },
      ],
      copyright: `Microwave Server is an independent project, not affiliated with or endorsed by Progress. Built with Docusaurus.`,
    },
    prism: {
      additionalLanguages: ['bash', 'toml', 'ruby', 'rust', 'json'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
