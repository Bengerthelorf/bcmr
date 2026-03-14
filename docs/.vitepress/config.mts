import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'BCMR',
  description: 'Better Copy Move Remove — A modern CLI tool for file operations',
  base: '/bcmr/',

  locales: {
    root: {
      label: 'English',
      lang: 'en',
      themeConfig: {
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
      },
    },
    zh: {
      label: '简体中文',
      lang: 'zh-Hans',
      themeConfig: {
        nav: [
          { text: '指南', link: '/zh/guide/getting-started' },
          { text: 'CLI', link: '/zh/cli/' },
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
  },

  themeConfig: {
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
