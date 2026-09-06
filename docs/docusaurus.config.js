const path = require('path');

const config = {
  title: 'Persisting',
  tagline: 'Persistent Infrastructure for the Agent Era',
  favicon: 'img/logos/persisting-icon.png',
  url: 'https://deeplink-org.github.io',
  // Use `/` for local previews; GitHub Pages sets DOCUSAURUS_BASE_URL=/Persisting/.
  baseUrl: process.env.DOCUSAURUS_BASE_URL || '/',
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
      path: path.resolve(__dirname, 'src/en'), routeBasePath: 'docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/'
    }],
    [require.resolve('@docusaurus/plugin-content-docs'), {
      id: 'zh', path: path.resolve(__dirname, 'src/zh'), routeBasePath: 'zh/docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/'
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
    docs: {
      sidebar: {
        hideable: true,
        autoCollapseCategories: true,
      },
    },
    announcementBar: {
      id: 'agent-era',
      content: 'Persistent Infrastructure for the Agent Era · Start with a Run or a Dataset',
      isCloseable: true,
    },
    navbar: {
      title: 'Persisting',
      logo: { alt: 'Persisting', src: 'img/logos/persisting-icon.png' },
      items: [
        { to: '/docs/', label: 'Start here', position: 'left' },
        { to: '/docs/pvisor/', label: 'pVisor', position: 'left' },
        { to: '/docs/pchronicle/', label: 'pChronicle', position: 'left' },
        { href: 'https://github.com/DeepLink-org/Persisting', label: 'GitHub', position: 'right' },
        { to: '/docs/', label: 'English', position: 'right' },
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
    prism: { theme: require('prism-react-renderer').themes.github, darkTheme: require('prism-react-renderer').themes.dracula },
  },
};
module.exports = config;
