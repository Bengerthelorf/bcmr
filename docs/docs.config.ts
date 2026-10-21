export default {
  slug: 'bcmr',
  install: {
    macos:   { name: 'macOS',   cmd: 'brew install Bengerthelorf/tap/bcmr',            note: 'homebrew; native arm64 or x86_64 binary' },
    linux:   { name: 'Linux',   cmd: 'curl -fsSL https://app.snaix.homes/bcmr/install.sh | bash', note: 'debian, ubuntu, arch, alpine, fedora · musl static · SHA-256 verified' },
    windows: { name: 'Windows', cmd: 'cargo install bcmr --locked',                    note: 'prebuilt zips on GitHub Releases · x86_64 + arm64' },
    source:  { name: 'source',  cmd: 'cargo install bcmr --locked',                    note: 'builds from crates.io · rust 1.91+' },
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
        'ssh-options',
        'transfer-backends',
        'scp-compatibility',
        'local-remote-semantics',
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
        'ablation/json-schema',
        'ablation/open-questions',
      ],
    },
  ],
  linkRewrites: {
    '/ablation/': '/internals/',
    '/guide/': '/docs/',
  },
};
