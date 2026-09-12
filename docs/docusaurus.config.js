const path = require('path');
const { themes } = require('prism-react-renderer');
const codeTheme = {
  ...themes.nightOwl,
  plain: { ...themes.nightOwl.plain, backgroundColor: '#0c121e' },
};

const baseUrl = process.env.DOCUSAURUS_BASE_URL || '/';
const activeRoot = `^${baseUrl.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`;

const config = {
  title: 'Persisting',
  tagline: 'Persistent Infrastructure for the Agent Era',
  favicon: 'img/logos/persisting-icon.png',
  url: 'https://deeplink-org.github.io',
  // Use `/` for local previews; GitHub Pages sets DOCUSAURUS_BASE_URL=/Persisting/.
  baseUrl,
  organizationName: 'DeepLink-org',
  projectName: 'Persisting',
  onBrokenLinks: 'throw',
  markdown: { hooks: { onBrokenMarkdownLinks: 'throw' } },
  presets: [
    ['classic', {
      docs: false,
      blog: false,
      theme: { customCss: require.resolve('./src/css/custom.css') },
    }],
  ],
  plugins: [
    [require.resolve('@docusaurus/plugin-content-docs'), {
      path: path.resolve(__dirname, 'src/en'), routeBasePath: 'docs', sidebarPath: require.resolve('./sidebars.en.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/'
    }],
    [require.resolve('@docusaurus/plugin-content-docs'), {
      id: 'zh', path: path.resolve(__dirname, 'src/zh'), routeBasePath: 'zh/docs', sidebarPath: require.resolve('./sidebars.zh.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/'
    }],
    [require.resolve('@easyops-cn/docusaurus-search-local'), {
      hashed: true,
      docsDir: [path.resolve(__dirname, 'src/en'), path.resolve(__dirname, 'src/zh')],
      language: ['en', 'zh'],
      indexDocs: true,
      indexBlog: false,
      indexPages: true,
    }],
  ],
  themeConfig: {
    colorMode: { defaultMode: 'dark', disableSwitch: true, respectPrefersColorScheme: false },
    docs: {
      sidebar: {
        hideable: true,
        autoCollapseCategories: true,
      },
    },
    navbar: {
      title: 'Persisting',
      logo: { alt: 'Persisting', src: 'img/logos/persisting-icon.png' },
      items: [
        { to: '/docs/', label: 'Start here', position: 'left', activeBaseRegex: `${activeRoot}(zh/)?docs/?$` },
        { to: '/docs/pvisor/', label: 'pVisor', position: 'left', activeBaseRegex: `${activeRoot}(zh/)?docs/pvisor/` },
        { to: '/docs/pchronicle/', label: 'pChronicle', position: 'left', activeBaseRegex: `${activeRoot}(zh/)?docs/pchronicle/` },
        { href: 'https://github.com/DeepLink-org/Persisting', label: 'GitHub', position: 'right' },
        { to: '/', label: 'English', position: 'right', activeBaseRegex: `${activeRoot}$` },
        { to: '/zh/', label: '中文', position: 'right' },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        { title: 'Start here', items: [{ label: 'Choose a workflow', to: '/docs/overview' }, { label: 'Installation', to: '/docs/installation' }] },
        { title: 'Products', items: [{ label: 'pVisor', to: '/docs/pvisor/' }, { label: 'pChronicle', to: '/docs/pchronicle/' }] },
        { title: 'Project', items: [{ label: 'System design', to: '/docs/system-design/' }, { label: 'GitHub', href: 'https://github.com/DeepLink-org/Persisting' }] },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} DeepLink-org`,
    },
    prism: { theme: codeTheme, darkTheme: codeTheme },
  },
};
module.exports = config;
