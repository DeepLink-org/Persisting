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
      id: 'en', path: 'docs/en', routeBasePath: 'docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/website/'
    }],
    [require.resolve('@docusaurus/plugin-content-docs'), {
      id: 'zh', path: 'docs/zh', routeBasePath: 'zh/docs', sidebarPath: require.resolve('./sidebars.js'), editUrl: 'https://github.com/DeepLink-org/Persisting/edit/main/docs/website/'
    }],
  ],
  themeConfig: {
    image: 'img/diagrams/persisting/hero-cyberpunk.svg',
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
    prism: { theme: require('prism-react-renderer').themes.github, darkTheme: require('prism-react-renderer').themes.dracula },
  },
};
module.exports = config;
