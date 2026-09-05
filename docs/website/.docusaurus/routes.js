import React from 'react';
import ComponentCreator from '@docusaurus/ComponentCreator';

export default [
  {
    path: '/Persisting/zh',
    component: ComponentCreator('/Persisting/zh', '8d7'),
    exact: true
  },
  {
    path: '/Persisting/docs',
    component: ComponentCreator('/Persisting/docs', '07c'),
    routes: [
      {
        path: '/Persisting/docs',
        component: ComponentCreator('/Persisting/docs', '27d'),
        routes: [
          {
            path: '/Persisting/docs',
            component: ComponentCreator('/Persisting/docs', 'c2e'),
            routes: [
              {
                path: '/Persisting/docs/',
                component: ComponentCreator('/Persisting/docs/', 'da4'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/installation',
                component: ComponentCreator('/Persisting/docs/installation', 'a40'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/overview',
                component: ComponentCreator('/Persisting/docs/overview', 'd13'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/',
                component: ComponentCreator('/Persisting/docs/pchronicle/', 'acf'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/concepts/',
                component: ComponentCreator('/Persisting/docs/pchronicle/concepts/', 'abd'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/concepts/dataset-and-source',
                component: ComponentCreator('/Persisting/docs/pchronicle/concepts/dataset-and-source', '841'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/concepts/facts-and-projections',
                component: ComponentCreator('/Persisting/docs/pchronicle/concepts/facts-and-projections', '542'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/design/',
                component: ComponentCreator('/Persisting/docs/pchronicle/design/', '1d1'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/design/architecture',
                component: ComponentCreator('/Persisting/docs/pchronicle/design/architecture', '124'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/design/catalog',
                component: ComponentCreator('/Persisting/docs/pchronicle/design/catalog', '7e6'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/design/storyline-lance',
                component: ComponentCreator('/Persisting/docs/pchronicle/design/storyline-lance', '223'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/design/trajectory-storage',
                component: ComponentCreator('/Persisting/docs/pchronicle/design/trajectory-storage', '0a5'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/get-started',
                component: ComponentCreator('/Persisting/docs/pchronicle/get-started', '7a3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/', 'ac3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/discover-and-query',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/discover-and-query', 'ad7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/exchange',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/exchange', '9c2'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/serve',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/serve', '93d'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/serve-gateway',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/serve-gateway', '3e0'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/guides/ui',
                component: ComponentCreator('/Persisting/docs/pchronicle/guides/ui', 'f0b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/', 'c42'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/agenticmd',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/agenticmd', 'ed7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/cli',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/cli', 'aa3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/formats/',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/formats/', 'edc'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/query-model',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/query-model', '846'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pchronicle/reference/terminology',
                component: ComponentCreator('/Persisting/docs/pchronicle/reference/terminology', '4eb'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/project/',
                component: ComponentCreator('/Persisting/docs/project/', '66e'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/project/engineering',
                component: ComponentCreator('/Persisting/docs/project/engineering', '3a0'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/project/examples',
                component: ComponentCreator('/Persisting/docs/project/examples', '64b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/project/releasing',
                component: ComponentCreator('/Persisting/docs/project/releasing', 'ae8'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/',
                component: ComponentCreator('/Persisting/docs/pvisor/', 'd04'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/concepts/',
                component: ComponentCreator('/Persisting/docs/pvisor/concepts/', 'ca7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/concepts/agentvisor',
                component: ComponentCreator('/Persisting/docs/pvisor/concepts/agentvisor', '419'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/concepts/capabilities-and-evidence',
                component: ComponentCreator('/Persisting/docs/pvisor/concepts/capabilities-and-evidence', 'd46'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/concepts/run-model',
                component: ComponentCreator('/Persisting/docs/pvisor/concepts/run-model', '0ec'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/design/',
                component: ComponentCreator('/Persisting/docs/pvisor/design/', '1de'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/design/cli',
                component: ComponentCreator('/Persisting/docs/pvisor/design/cli', 'a8d'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/design/gateway',
                component: ComponentCreator('/Persisting/docs/pvisor/design/gateway', '9bc'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/design/isolation',
                component: ComponentCreator('/Persisting/docs/pvisor/design/isolation', 'ea3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/design/overlaynet',
                component: ComponentCreator('/Persisting/docs/pvisor/design/overlaynet', 'b4e'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/get-started',
                component: ComponentCreator('/Persisting/docs/pvisor/get-started', '618'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/', '101'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/capture',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/capture', '64c'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/execution',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/execution', '51f'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/network',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/network', '5ec'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/review-apply',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/review-apply', '367'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/guides/sandbox-replay',
                component: ComponentCreator('/Persisting/docs/pvisor/guides/sandbox-replay', '398'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/reference/',
                component: ComponentCreator('/Persisting/docs/pvisor/reference/', '964'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/reference/cases',
                component: ComponentCreator('/Persisting/docs/pvisor/reference/cases', 'b96'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/pvisor/reference/cli',
                component: ComponentCreator('/Persisting/docs/pvisor/reference/cli', 'e5b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/',
                component: ComponentCreator('/Persisting/docs/rfcs/', '799'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/actf-format',
                component: ComponentCreator('/Persisting/docs/rfcs/actf-format', 'a62'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/agent-corpus-lance-layout',
                component: ComponentCreator('/Persisting/docs/rfcs/agent-corpus-lance-layout', '53f'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/atif-format',
                component: ComponentCreator('/Persisting/docs/rfcs/atif-format', '4ce'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/compact-jsonl',
                component: ComponentCreator('/Persisting/docs/rfcs/compact-jsonl', '65b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/events-contract-pchronicle-sidecar',
                component: ComponentCreator('/Persisting/docs/rfcs/events-contract-pchronicle-sidecar', 'cfd'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/events-format',
                component: ComponentCreator('/Persisting/docs/rfcs/events-format', 'd18'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/openai-messages-format',
                component: ComponentCreator('/Persisting/docs/rfcs/openai-messages-format', '0f6'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/pchronicle-find-query-syntax',
                component: ComponentCreator('/Persisting/docs/rfcs/pchronicle-find-query-syntax', '2f5'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/pchronicle-ownership',
                component: ComponentCreator('/Persisting/docs/rfcs/pchronicle-ownership', 'c13'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/pchronicle-revision-lineage',
                component: ComponentCreator('/Persisting/docs/rfcs/pchronicle-revision-lineage', '325'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/pchronicle-vortex-backend',
                component: ComponentCreator('/Persisting/docs/rfcs/pchronicle-vortex-backend', '55e'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/pchronicle-warehouse-catalog',
                component: ComponentCreator('/Persisting/docs/rfcs/pchronicle-warehouse-catalog', 'dc7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/rfcs/storyline-format',
                component: ComponentCreator('/Persisting/docs/rfcs/storyline-format', '0cb'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/system-design/',
                component: ComponentCreator('/Persisting/docs/system-design/', 'ca9'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/system-design/architecture',
                component: ComponentCreator('/Persisting/docs/system-design/architecture', '6df'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/system-design/local-to-fleet',
                component: ComponentCreator('/Persisting/docs/system-design/local-to-fleet', '63d'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/docs/system-design/security-evidence',
                component: ComponentCreator('/Persisting/docs/system-design/security-evidence', '558'),
                exact: true,
                sidebar: "zhSidebar"
              }
            ]
          }
        ]
      }
    ]
  },
  {
    path: '/Persisting/zh/docs',
    component: ComponentCreator('/Persisting/zh/docs', '33e'),
    routes: [
      {
        path: '/Persisting/zh/docs',
        component: ComponentCreator('/Persisting/zh/docs', 'cc7'),
        routes: [
          {
            path: '/Persisting/zh/docs',
            component: ComponentCreator('/Persisting/zh/docs', '090'),
            routes: [
              {
                path: '/Persisting/zh/docs/',
                component: ComponentCreator('/Persisting/zh/docs/', 'fa2'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/installation',
                component: ComponentCreator('/Persisting/zh/docs/installation', '55a'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/overview',
                component: ComponentCreator('/Persisting/zh/docs/overview', '419'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/', '179'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/concepts/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/concepts/', 'acc'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/concepts/dataset-and-source',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/concepts/dataset-and-source', '360'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/concepts/facts-and-projections',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/concepts/facts-and-projections', '20f'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/design/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/design/', '2ce'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/design/architecture',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/design/architecture', '47b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/design/catalog',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/design/catalog', '2e0'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/design/storyline-lance',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/design/storyline-lance', 'bfd'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/design/trajectory-storage',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/design/trajectory-storage', '648'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/get-started',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/get-started', 'e28'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/', '4ec'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/discover-and-query',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/discover-and-query', '6ac'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/exchange',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/exchange', 'f66'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/serve',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/serve', '6b8'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/serve-gateway',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/serve-gateway', 'd85'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/guides/ui',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/guides/ui', '2b9'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/', 'cd0'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/agenticmd',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/agenticmd', '11a'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/cli',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/cli', 'e26'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/formats/',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/formats/', '413'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/query-model',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/query-model', '95a'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pchronicle/reference/terminology',
                component: ComponentCreator('/Persisting/zh/docs/pchronicle/reference/terminology', '277'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/project/',
                component: ComponentCreator('/Persisting/zh/docs/project/', '938'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/project/engineering',
                component: ComponentCreator('/Persisting/zh/docs/project/engineering', 'a4f'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/project/examples',
                component: ComponentCreator('/Persisting/zh/docs/project/examples', 'b3b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/project/releasing',
                component: ComponentCreator('/Persisting/zh/docs/project/releasing', '6ce'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/', 'd8b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/concepts/',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/concepts/', '31c'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/concepts/agentvisor',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/concepts/agentvisor', 'e8a'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/concepts/capabilities-and-evidence',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/concepts/capabilities-and-evidence', '653'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/concepts/run-model',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/concepts/run-model', '2b3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/design/',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/design/', 'f68'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/design/cli',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/design/cli', '409'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/design/gateway',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/design/gateway', 'd78'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/design/isolation',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/design/isolation', '0bc'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/design/overlaynet',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/design/overlaynet', '3b8'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/get-started',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/get-started', '08d'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/', 'b9b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/capture',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/capture', '2b7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/execution',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/execution', 'b18'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/network',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/network', '095'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/review-apply',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/review-apply', '542'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/guides/sandbox-replay',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/guides/sandbox-replay', 'b90'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/reference/',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/reference/', 'd6f'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/reference/cases',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/reference/cases', 'dff'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/pvisor/reference/cli',
                component: ComponentCreator('/Persisting/zh/docs/pvisor/reference/cli', '661'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/', '379'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/actf-format',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/actf-format', '569'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/agent-corpus-lance-layout',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/agent-corpus-lance-layout', '320'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/atif-format',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/atif-format', '5d2'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/compact-jsonl',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/compact-jsonl', '24b'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/events-contract-pchronicle-sidecar',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/events-contract-pchronicle-sidecar', 'e0c'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/events-format',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/events-format', '2aa'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/openai-messages-format',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/openai-messages-format', '5c7'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/pchronicle-find-query-syntax',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/pchronicle-find-query-syntax', '25e'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/pchronicle-ownership',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/pchronicle-ownership', '7a8'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/pchronicle-revision-lineage',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/pchronicle-revision-lineage', '056'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/pchronicle-vortex-backend',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/pchronicle-vortex-backend', 'ba8'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/pchronicle-warehouse-catalog',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/pchronicle-warehouse-catalog', 'a45'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/rfcs/storyline-format',
                component: ComponentCreator('/Persisting/zh/docs/rfcs/storyline-format', 'a00'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/system-design/',
                component: ComponentCreator('/Persisting/zh/docs/system-design/', 'cdb'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/system-design/architecture',
                component: ComponentCreator('/Persisting/zh/docs/system-design/architecture', 'dd3'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/system-design/local-to-fleet',
                component: ComponentCreator('/Persisting/zh/docs/system-design/local-to-fleet', '6cc'),
                exact: true,
                sidebar: "zhSidebar"
              },
              {
                path: '/Persisting/zh/docs/system-design/security-evidence',
                component: ComponentCreator('/Persisting/zh/docs/system-design/security-evidence', 'efc'),
                exact: true,
                sidebar: "zhSidebar"
              }
            ]
          }
        ]
      }
    ]
  },
  {
    path: '/Persisting/',
    component: ComponentCreator('/Persisting/', '272'),
    exact: true
  },
  {
    path: '*',
    component: ComponentCreator('*'),
  },
];
