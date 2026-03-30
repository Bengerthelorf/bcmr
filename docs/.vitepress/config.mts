import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'BCMR',
  description: 'Better Copy Move Remove — A modern CLI tool for file operations',
  base: '/bcmr/',

  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: '/bcmr/images/icon.svg' }],
  ],

  markdown: {
    math: true,
  },

  locales: {
    root: {
      label: 'English',
      lang: 'en',
      themeConfig: {
        nav: [
          { text: 'Guide', link: '/guide/getting-started' },
          { text: 'CLI', link: '/cli/' },
          { text: 'Internals', link: '/ablation/' },
          {
            text: 'Download',
            link: 'https://github.com/Bengerthelorf/bcmr/releases/latest',
          },
        ],
        sidebar: {
          '/guide/': [
            {
              text: 'Getting Started',
              items: [
                { text: 'Installation', link: '/guide/getting-started' },
                { text: 'Shell Integration', link: '/guide/shell-integration' },
              ],
            },
            {
              text: 'Usage',
              items: [
                { text: 'Configuration', link: '/guide/configuration' },
                { text: 'Progress Display', link: '/guide/progress-display' },
                { text: 'Remote Copy (SSH)', link: '/guide/remote-copy' },
              ],
            },
          ],
          '/cli/': [
            {
              text: 'CLI Reference',
              items: [
                { text: 'Overview', link: '/cli/' },
                { text: 'Command Reference', link: '/cli/commands' },
              ],
            },
          ],
          '/ablation/': [
            {
              text: 'Internals',
              items: [
                { text: 'Streaming Checkpoint Copy', link: '/ablation/' },
              ],
            },
          ],
        },
      },
    },
    zh: {
      label: '简体中文',
      lang: 'zh-Hans',
      themeConfig: {
        nav: [
          { text: '指南', link: '/zh/guide/getting-started' },
          { text: 'CLI', link: '/zh/cli/' },
          { text: '技术内幕', link: '/ablation/' },
          {
            text: '下载',
            link: 'https://github.com/Bengerthelorf/bcmr/releases/latest',
          },
        ],
        sidebar: {
          '/zh/guide/': [
            {
              text: '入门',
              items: [
                { text: '安装', link: '/zh/guide/getting-started' },
                { text: 'Shell 集成', link: '/zh/guide/shell-integration' },
              ],
            },
            {
              text: '使用',
              items: [
                { text: '配置', link: '/zh/guide/configuration' },
                { text: '进度显示', link: '/zh/guide/progress-display' },
                { text: '远程复制 (SSH)', link: '/zh/guide/remote-copy' },
              ],
            },
          ],
          '/zh/cli/': [
            {
              text: 'CLI 参考',
              items: [
                { text: '概览', link: '/zh/cli/' },
                { text: '命令参考', link: '/zh/cli/commands' },
              ],
            },
          ],
        },
      },
    },
    'zh-Hant': {
      label: '正體中文',
      lang: 'zh-Hant',
      themeConfig: {
        nav: [
          { text: '指南', link: '/zh-Hant/guide/getting-started' },
          { text: 'CLI', link: '/zh-Hant/cli/' },
          { text: '技術內幕', link: '/ablation/' },
          {
            text: '下載',
            link: 'https://github.com/Bengerthelorf/bcmr/releases/latest',
          },
        ],
        sidebar: {
          '/zh-Hant/guide/': [
            {
              text: '入門',
              items: [
                { text: '安裝', link: '/zh-Hant/guide/getting-started' },
                { text: 'Shell 整合', link: '/zh-Hant/guide/shell-integration' },
              ],
            },
            {
              text: '使用',
              items: [
                { text: '設定', link: '/zh-Hant/guide/configuration' },
                { text: '進度顯示', link: '/zh-Hant/guide/progress-display' },
                { text: '遠端複製 (SSH)', link: '/zh-Hant/guide/remote-copy' },
              ],
            },
          ],
          '/zh-Hant/cli/': [
            {
              text: 'CLI 參考',
              items: [
                { text: '概覽', link: '/zh-Hant/cli/' },
                { text: '命令參考', link: '/zh-Hant/cli/commands' },
              ],
            },
          ],
        },
      },
    },
  },

  themeConfig: {
    logo: '/images/icon.svg',

    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'CLI', link: '/cli/' },
      {
        text: 'Download',
        link: 'https://github.com/Bengerthelorf/bcmr/releases/latest',
      },
    ],

    sidebar: {
      '/guide/': [
        {
          text: 'Getting Started',
          items: [
            { text: 'Installation', link: '/guide/getting-started' },
            { text: 'Shell Integration', link: '/guide/shell-integration' },
          ],
        },
        {
          text: 'Usage',
          items: [
            { text: 'Configuration', link: '/guide/configuration' },
            { text: 'Progress Display', link: '/guide/progress-display' },
            { text: 'Remote Copy (SSH)', link: '/guide/remote-copy' },
          ],
        },
      ],
      '/cli/': [
        {
          text: 'CLI Reference',
          items: [
            { text: 'Overview', link: '/cli/' },
            { text: 'Command Reference', link: '/cli/commands' },
          ],
        },
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/Bengerthelorf/bcmr' },
    ],

    editLink: {
      pattern: 'https://github.com/Bengerthelorf/bcmr/edit/main/docs/:path',
    },

    footer: {
      message: 'Released under the GPL-3.0 License.',
      copyright: 'Copyright © 2024-present Zane Leong',
    },

    search: {
      provider: 'local',
    },
  },
})
