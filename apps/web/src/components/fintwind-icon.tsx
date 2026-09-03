import type { ProviderKind } from '@fintwind/client'

export const FINTWIND_ICONS = {
  alert: 'i-fintwind-alert',
  appearance: 'i-fintwind-appearance',
  arrowDown: 'i-fintwind-arrow-down',
  arrowLeft: 'i-fintwind-arrow-left',
  arrowRight: 'i-fintwind-arrow-right',
  arrowUp: 'i-fintwind-arrow-up',
  arrowUpRight: 'i-fintwind-arrow-up-right',
  bot: 'i-fintwind-bot',
  chartColumn: 'i-fintwind-chart-column',
  check: 'i-fintwind-check',
  chevronDown: 'i-fintwind-chevron-down',
  chevronRight: 'i-fintwind-chevron-right',
  cloudUpload: 'i-fintwind-cloud-upload',
  command: 'i-fintwind-command',
  compose: 'i-fintwind-compose',
  copy: 'i-fintwind-copy',
  cornerDownRight: 'i-fintwind-corner-down-right',
  ellipsis: 'i-fintwind-ellipsis',
  eye: 'i-fintwind-eye',
  eyeOff: 'i-fintwind-eye-off',
  file: 'i-fintwind-file',
  fileDiff: 'i-fintwind-file-diff',
  folder: 'i-fintwind-folder',
  folderNew: 'i-fintwind-folder-new',
  fork: 'i-fintwind-fork',
  gauge: 'i-fintwind-gauge',
  gitBranch: 'i-fintwind-git-branch',
  gitCommitHorizontal: 'i-fintwind-git-commit-horizontal',
  globe: 'i-fintwind-globe',
  github: 'i-fintwind-github',
  info: 'i-fintwind-info',
  laptop: 'i-fintwind-laptop',
  list: 'i-fintwind-list',
  loaderCircle: 'i-fintwind-loader-circle',
  lock: 'i-fintwind-lock',
  lockOpen: 'i-fintwind-lock-open',
  package: 'i-fintwind-package',
  paperclip: 'i-fintwind-paperclip',
  panelLeft: 'i-fintwind-panel-left',
  panelRight: 'i-fintwind-panel-right',
  pencil: 'i-fintwind-pencil',
  plus: 'i-fintwind-plus',
  queue: 'i-fintwind-queue',
  rotateCw: 'i-fintwind-rotate-cw',
  rewind: 'i-fintwind-rewind',
  search: 'i-fintwind-search',
  server: 'i-fintwind-server',
  settings: 'i-fintwind-settings',
  sparkle: 'i-fintwind-sparkle',
  star: 'i-fintwind-star',
  starFilled: 'i-fintwind-star-filled',
  stop: 'i-fintwind-stop',
  stopFilled: 'i-fintwind-stop-filled',
  terminal: 'i-fintwind-terminal',
  terminalSquare: 'i-fintwind-terminal-square',
  trash: 'i-fintwind-trash',
  wrench: 'i-fintwind-wrench',
  x: 'i-fintwind-x',
  zap: 'i-fintwind-zap',
} as const

export type FintwindIconName = keyof typeof FINTWIND_ICONS

export function FintwindIcon({
  name,
  className,
  label,
}: {
  name: FintwindIconName
  className?: string
  label?: string
}) {
  return (
    <span
      aria-hidden={label ? undefined : true}
      aria-label={label}
      className={`inline-grid size-4 shrink-0 place-items-center ${className ?? ''}`}
      role={label ? 'img' : undefined}
    >
      <span
        aria-hidden="true"
        className={FINTWIND_ICONS[name]}
        style={{ width: '100%', height: '100%' }}
      />
    </span>
  )
}

const FILE_TYPE_ICONS = {
  angular: 'i-fintwind-file-type-angular',
  astro: 'i-fintwind-file-type-astro',
  audio: 'i-fintwind-file-type-audio',
  babel: 'i-fintwind-file-type-babel',
  biome: 'i-fintwind-file-type-biome',
  bun: 'i-fintwind-file-type-bun',
  c: 'i-fintwind-file-type-c',
  certificate: 'i-fintwind-file-type-certificate',
  clojure: 'i-fintwind-file-type-clojure',
  cmake: 'i-fintwind-file-type-cmake',
  coffee: 'i-fintwind-file-type-coffee',
  console: 'i-fintwind-file-type-console',
  cpp: 'i-fintwind-file-type-cpp',
  crystal: 'i-fintwind-file-type-crystal',
  csharp: 'i-fintwind-file-type-csharp',
  css: 'i-fintwind-file-type-css',
  dart: 'i-fintwind-file-type-dart',
  database: 'i-fintwind-file-type-database',
  deno: 'i-fintwind-file-type-deno',
  diff: 'i-fintwind-file-type-diff',
  docker: 'i-fintwind-file-type-docker',
  editorconfig: 'i-fintwind-file-type-editorconfig',
  elixir: 'i-fintwind-file-type-elixir',
  elm: 'i-fintwind-file-type-elm',
  erlang: 'i-fintwind-file-type-erlang',
  eslint: 'i-fintwind-file-type-eslint',
  exe: 'i-fintwind-file-type-exe',
  file: 'i-fintwind-file-type-file',
  firebase: 'i-fintwind-file-type-firebase',
  git: 'i-fintwind-file-type-git',
  gitlab: 'i-fintwind-file-type-gitlab',
  go: 'i-fintwind-file-type-go',
  gradle: 'i-fintwind-file-type-gradle',
  graphql: 'i-fintwind-file-type-graphql',
  haskell: 'i-fintwind-file-type-haskell',
  haxe: 'i-fintwind-file-type-haxe',
  helm: 'i-fintwind-file-type-helm',
  html: 'i-fintwind-file-type-html',
  image: 'i-fintwind-file-type-image',
  java: 'i-fintwind-file-type-java',
  javascript: 'i-fintwind-file-type-javascript',
  jinja: 'i-fintwind-file-type-jinja',
  json: 'i-fintwind-file-type-json',
  julia: 'i-fintwind-file-type-julia',
  kotlin: 'i-fintwind-file-type-kotlin',
  kubernetes: 'i-fintwind-file-type-kubernetes',
  lock: 'i-fintwind-file-type-lock',
  lua: 'i-fintwind-file-type-lua',
  makefile: 'i-fintwind-file-type-makefile',
  markdown: 'i-fintwind-file-type-markdown',
  nest: 'i-fintwind-file-type-nest',
  next: 'i-fintwind-file-type-next',
  nginx: 'i-fintwind-file-type-nginx',
  nix: 'i-fintwind-file-type-nix',
  nodejs: 'i-fintwind-file-type-nodejs',
  npm: 'i-fintwind-file-type-npm',
  nuxt: 'i-fintwind-file-type-nuxt',
  ocaml: 'i-fintwind-file-type-ocaml',
  pdf: 'i-fintwind-file-type-pdf',
  perl: 'i-fintwind-file-type-perl',
  php: 'i-fintwind-file-type-php',
  pnpm: 'i-fintwind-file-type-pnpm',
  powershell: 'i-fintwind-file-type-powershell',
  prettier: 'i-fintwind-file-type-prettier',
  prisma: 'i-fintwind-file-type-prisma',
  proto: 'i-fintwind-file-type-proto',
  pug: 'i-fintwind-file-type-pug',
  python: 'i-fintwind-file-type-python',
  react: 'i-fintwind-file-type-react',
  readme: 'i-fintwind-file-type-readme',
  rollup: 'i-fintwind-file-type-rollup',
  ruby: 'i-fintwind-file-type-ruby',
  rust: 'i-fintwind-file-type-rust',
  sass: 'i-fintwind-file-type-sass',
  scala: 'i-fintwind-file-type-scala',
  settings: 'i-fintwind-file-type-settings',
  solidity: 'i-fintwind-file-type-solidity',
  storybook: 'i-fintwind-file-type-storybook',
  stylelint: 'i-fintwind-file-type-stylelint',
  supabase: 'i-fintwind-file-type-supabase',
  svelte: 'i-fintwind-file-type-svelte',
  svg: 'i-fintwind-file-type-svg',
  swift: 'i-fintwind-file-type-swift',
  tailwindcss: 'i-fintwind-file-type-tailwindcss',
  terraform: 'i-fintwind-file-type-terraform',
  tex: 'i-fintwind-file-type-tex',
  turborepo: 'i-fintwind-file-type-turborepo',
  typescript: 'i-fintwind-file-type-typescript',
  video: 'i-fintwind-file-type-video',
  vite: 'i-fintwind-file-type-vite',
  vitest: 'i-fintwind-file-type-vitest',
  vue: 'i-fintwind-file-type-vue',
  webassembly: 'i-fintwind-file-type-webassembly',
  webpack: 'i-fintwind-file-type-webpack',
  xaml: 'i-fintwind-file-type-xaml',
  xml: 'i-fintwind-file-type-xml',
  yaml: 'i-fintwind-file-type-yaml',
  yarn: 'i-fintwind-file-type-yarn',
  zig: 'i-fintwind-file-type-zig',
  zip: 'i-fintwind-file-type-zip',
} as const

type FileTypeIconName = keyof typeof FILE_TYPE_ICONS

export function FileTypeIcon({
  path,
  className,
}: {
  path: string
  className?: string
}) {
  const name = fileTypeIconName(path)
  return (
    <span
      aria-hidden="true"
      className={`inline-grid size-4 shrink-0 place-items-center ${className ?? ''}`}
    >
      <span className={FILE_TYPE_ICONS[name]} style={{ width: '100%', height: '100%' }} />
    </span>
  )
}

function fileTypeIconName(path: string): FileTypeIconName {
  const name = path.split(/[\\/]/).at(-1)?.toLocaleLowerCase() ?? path.toLocaleLowerCase()
  if (name.startsWith('readme')) return 'readme'
  if (/^(license|licence|copying)/.test(name)) return 'certificate'
  if (name.startsWith('dockerfile') || name.startsWith('compose.')) return 'docker'
  if (name === 'cmakelists.txt' || name.startsWith('cmake.')) return 'cmake'
  if (name === 'makefile' || name.startsWith('makefile.') || name === 'justfile') return 'makefile'
  if (['cargo.toml', 'cargo.lock', 'rust-toolchain.toml'].includes(name)) return 'rust'
  if (['go.mod', 'go.sum', 'go.work'].includes(name)) return 'go'
  if (name === 'pyproject.toml' || name === 'pipfile' || name.startsWith('requirements')) return 'python'
  if (['bun.lock', 'bun.lockb', 'bunfig.toml'].includes(name)) return 'bun'
  if (name.startsWith('pnpm-') || name === '.pnpmfile.cjs') return 'pnpm'
  if (name === 'yarn.lock' || name.startsWith('.yarnrc')) return 'yarn'
  if (name === 'package.json') return 'nodejs'
  if (name === 'package-lock.json') return 'npm'
  if (name === 'tsconfig.json' || name.startsWith('tsconfig.')) return 'typescript'
  if (name === 'jsconfig.json' || name.startsWith('jsconfig.')) return 'javascript'
  if (['.gitignore', '.gitattributes', '.gitmodules', '.gitconfig'].includes(name)) return 'git'
  if (name === '.editorconfig') return 'editorconfig'
  if (name.startsWith('.env')) return 'settings'
  if (name.startsWith('.prettier') || name.startsWith('prettier.config.')) return 'prettier'
  if (name.startsWith('.eslint') || name.startsWith('eslint.config.')) return 'eslint'
  if (name.startsWith('biome.json')) return 'biome'
  if (name.startsWith('.babel') || name.startsWith('babel.config.')) return 'babel'
  if (name.startsWith('.stylelint') || name.startsWith('stylelint.config.')) return 'stylelint'
  if (name.startsWith('vite.config.')) return 'vite'
  if (name.startsWith('vitest.config.') || name.startsWith('vitest.workspace.')) return 'vitest'
  if (name.startsWith('webpack.')) return 'webpack'
  if (name.startsWith('rollup.config.')) return 'rollup'
  if (name.startsWith('next.config.') || name === 'next-env.d.ts') return 'next'
  if (name.startsWith('nuxt.config.') || name === '.nuxtrc') return 'nuxt'
  if (name.startsWith('astro.config.')) return 'astro'
  if (name === 'angular.json' || name.endsWith('.component.ts')) return 'angular'
  if (name === 'nest-cli.json') return 'nest'
  if (name.startsWith('tailwind.config.')) return 'tailwindcss'
  if (name.startsWith('svelte.config.')) return 'svelte'
  if (name.startsWith('vue.config.')) return 'vue'
  if (name === 'firebase.json' || name === '.firebaserc') return 'firebase'
  if (name === 'supabase.toml') return 'supabase'
  if (name.startsWith('prisma.config.')) return 'prisma'
  if (name === 'turbo.json') return 'turborepo'
  if (name.startsWith('deno.json') || name === 'deno.lock') return 'deno'
  if (name === '.gitlab-ci.yml' || name === '.gitlab-ci.yaml') return 'gitlab'
  if (name === 'kustomization.yaml' || name === 'kustomization.yml') return 'kubernetes'
  if (name === 'chart.yaml' || name === 'values.yaml') return 'helm'
  if (name === 'nginx.conf') return 'nginx'
  if (name === '.nvmrc' || name === '.node-version') return 'nodejs'
  if (['build.gradle', 'settings.gradle', 'gradlew', 'gradlew.bat'].includes(name)) return 'gradle'
  if (name.includes('.stories.') || name.includes('.story.')) return 'storybook'
  if (name === 'gemfile' || name === 'gemfile.lock') return 'ruby'
  if (name === 'pom.xml') return 'java'

  const extension = name.includes('.') ? name.split('.').at(-1) ?? '' : ''
  if (extension === 'rs') return 'rust'
  if (['js', 'mjs', 'cjs'].includes(extension)) return 'javascript'
  if (['ts', 'mts', 'cts'].includes(extension)) return 'typescript'
  if (['jsx', 'tsx'].includes(extension)) return 'react'
  if (['py', 'pyi', 'pyw'].includes(extension)) return 'python'
  if (extension === 'go') return 'go'
  if (['c', 'h', 'm'].includes(extension)) return 'c'
  if (['cc', 'cpp', 'cxx', 'hh', 'hpp', 'hxx', 'mm'].includes(extension)) return 'cpp'
  if (extension === 'cs') return 'csharp'
  if (extension === 'swift') return 'swift'
  if (['kt', 'kts'].includes(extension)) return 'kotlin'
  if (['java', 'class'].includes(extension)) return 'java'
  if (extension === 'rb') return 'ruby'
  if (extension === 'php') return 'php'
  if (['html', 'htm'].includes(extension)) return 'html'
  if (['css', 'less'].includes(extension)) return 'css'
  if (['scss', 'sass'].includes(extension)) return 'sass'
  if (['json', 'jsonc', 'jsonl'].includes(extension)) return 'json'
  if (['yaml', 'yml'].includes(extension)) return 'yaml'
  if (['toml', 'ini', 'cfg', 'conf', 'config'].includes(extension)) return 'settings'
  if (['xml', 'xsl', 'plist'].includes(extension)) return 'xml'
  if (['md', 'mdx', 'markdown'].includes(extension)) return 'markdown'
  if (['sh', 'bash', 'zsh', 'fish'].includes(extension)) return 'console'
  if (['ps1', 'psm1'].includes(extension)) return 'powershell'
  if (['sql', 'db', 'sqlite', 'sqlite3', 'csv', 'xls', 'xlsx'].includes(extension)) return 'database'
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'ico', 'tiff'].includes(extension)) return 'image'
  if (extension === 'svg') return 'svg'
  if (extension === 'pdf') return 'pdf'
  if (['mp3', 'wav', 'flac', 'ogg', 'm4a'].includes(extension)) return 'audio'
  if (['mp4', 'mov', 'avi', 'webm', 'mkv'].includes(extension)) return 'video'
  if (['zip', 'gz', 'tgz', 'bz2', 'xz', '7z', 'rar', 'tar', 'jar'].includes(extension)) return 'zip'
  if (['wasm', 'wat'].includes(extension)) return 'webassembly'
  if (['svelte', 'vue', 'lua', 'dart', 'astro', 'prisma', 'xaml', 'zig', 'nix', 'proto'].includes(extension)) return extension as FileTypeIconName
  if (['tf', 'tfvars'].includes(extension)) return 'terraform'
  if (['graphql', 'gql'].includes(extension)) return 'graphql'
  if (['coffee', 'cson'].includes(extension)) return 'coffee'
  if (extension === 'cr') return 'crystal'
  if (['ex', 'exs'].includes(extension)) return 'elixir'
  if (extension === 'elm') return 'elm'
  if (['erl', 'hrl'].includes(extension)) return 'erlang'
  if (['clj', 'cljs', 'cljc', 'edn'].includes(extension)) return 'clojure'
  if (['hs', 'lhs'].includes(extension)) return 'haskell'
  if (['hx', 'hxml'].includes(extension)) return 'haxe'
  if (['jinja', 'jinja2', 'j2'].includes(extension)) return 'jinja'
  if (extension === 'jl') return 'julia'
  if (['ml', 'mli'].includes(extension)) return 'ocaml'
  if (['pl', 'pm'].includes(extension)) return 'perl'
  if (['pug', 'jade'].includes(extension)) return 'pug'
  if (['scala', 'sbt', 'sc'].includes(extension)) return 'scala'
  if (extension === 'sol') return 'solidity'
  if (['tex', 'sty', 'cls'].includes(extension)) return 'tex'
  if (['diff', 'patch'].includes(extension)) return 'diff'
  if (['exe', 'dll', 'so', 'dylib'].includes(extension)) return 'exe'
  if (extension === 'lock') return 'lock'
  return 'file'
}

const PROVIDER_ICONS: Record<ProviderKind, string> = {
  amp: 'i-fintwind-provider-amp',
  claude: 'i-fintwind-provider-claude',
  codex: 'i-fintwind-provider-openai',
  cursor: 'i-fintwind-provider-cursor',
  deepSeek: 'i-fintwind-provider-deepseek',
  openCode: 'i-fintwind-provider-opencode',
  grok: 'i-fintwind-provider-grok',
  pi: 'i-fintwind-provider-pi',
}

export const PROVIDERS: Array<{
  id: ProviderKind
  name: string
  shortName: string
  command: string
}> = [
  { id: 'amp', name: 'Amp', shortName: 'Amp', command: 'amp' },
  { id: 'claude', name: 'Claude Code', shortName: 'Claude', command: 'claude' },
  { id: 'codex', name: 'Codex CLI', shortName: 'Codex', command: 'codex' },
  { id: 'cursor', name: 'Cursor CLI', shortName: 'Cursor', command: 'cursor-agent' },
  { id: 'deepSeek', name: 'DeepSeek Harness', shortName: 'DeepSeek', command: 'dsh' },
  { id: 'openCode', name: 'OpenCode', shortName: 'OpenCode', command: 'opencode' },
  { id: 'grok', name: 'Grok Build', shortName: 'Grok', command: 'grok' },
  { id: 'pi', name: 'Pi', shortName: 'Pi', command: 'pi' },
]

export function providerMeta(provider: ProviderKind) {
  return PROVIDERS.find((candidate) => candidate.id === provider) ?? PROVIDERS[2]!
}

export function ProviderIcon({
  provider,
  className,
  label,
}: {
  provider: ProviderKind
  className?: string
  label?: string
}) {
  return (
    <span
      aria-hidden={label ? undefined : true}
      aria-label={label}
      className={`inline-grid size-4 shrink-0 place-items-center ${providerColor(provider)} ${className ?? ''}`}
      role={label ? 'img' : undefined}
    >
      <span
        aria-hidden="true"
        className={PROVIDER_ICONS[provider]}
        style={{ width: '100%', height: '100%' }}
      />
    </span>
  )
}

function providerColor(provider: ProviderKind) {
  if (provider === 'amp') return 'text-[#f34e3f]'
  if (provider === 'claude') return 'text-[#d97757]'
  if (provider === 'deepSeek') return 'text-[#4d6bfe]'
  return 'text-[#34363b] dark:text-[#f3f3f3]'
}
