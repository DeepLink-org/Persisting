const config = {
  title: 'Persisting',
  tagline: 'Persistent Infrastructure for the Agent Era',
  favicon: 'img/logo-mark.svg',
  url: 'https://deeplink-org.github.io',
  baseUrl: '/Persisting/',
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
      path: 'docs/en', routeBasePath: 'docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/website/'
    }],
    [require.resolve('@docusaurus/plugin-content-docs'), {
      id: 'zh', path: 'docs/zh', routeBasePath: 'zh/docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/website/'
    }],
    [require.resolve('@easyops-cn/docusaurus-search-local'), {
      hashed: true,
      language: ['en', 'zh'],
      indexDocs: true,
      indexBlog: false,
      indexPages: true,
    }],
  ],
  themeConfig: {
    image: 'img/diagrams/persisting/zodiac-sky.svg',
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
      logo: { alt: 'Persisting', src: 'img/logo-mark.svg' },
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
