export default {
  slug: 'bcmr',
  install: {
    macos:   { name: 'macOS',   cmd: 'brew install Bengerthelorf/tap/bcmr',            note: 'homebrew; universal binary — arm64 + x86_64' },
    linux:   { name: 'Linux',   cmd: 'curl -fsSL app.snaix.homes/bcmr/install | bash', note: 'debian, ubuntu, arch, alpine, fedora · musl static' },
    windows: { name: 'Windows', cmd: 'scoop install bcmr',                             note: 'or winget install Bengerthelorf.bcmr' },
    source:  { name: 'source',  cmd: 'cargo install bcmr --locked',                    note: 'builds from crates.io · rust 1.78+' },
  },
  sections: [
    {
      label: 'guide',
      items: [
        'getting-started',
        'shell-integration',
        'configuration',
        'progress-display',
        'remote-copy',
      ],
    },
    {
      label: 'internals',
      items: [
        'ablation/index',
        'ablation/scc',
        'ablation/local-perf',
        'ablation/wire-protocol',
        'ablation/path-b-design',
        'ablation/no-rolling-checksum',
        'ablation/open-questions',
      ],
    },
  ],
  linkRewrites: {
    '/ablation/': '/internals/',
    '/guide/': '/docs/',
  },
};
