import { llms, loader } from 'fumadocs-core/source';
import { flattenTree } from 'fumadocs-core/page-tree';
import { appName, basePath, docsRoute, getPageMarkdownUrl, gitConfig, releaseUrl, repoUrl, version } from './shared';
import { defineDocs } from 'fumadocs-mdx/macro';
import { metaSchema, pageSchema } from 'fumadocs-core/source/schema';

const docs = defineDocs({
  dir: 'content/docs',
  docs: {
    schema: pageSchema,
    postprocess: {
      includeProcessedMarkdown: true,
    },
  },
  meta: {
    schema: metaSchema,
  },
});

// See https://fumadocs.dev/docs/headless/source-api for more info
export const source = loader({
  baseUrl: docsRoute,
  source: docs.toFumadocsSource(),
  plugins: [],
});

type Page = NonNullable<ReturnType<typeof source.getNodePage>>;

// Agents read these files outside the site, so every link in them is absolute.
const siteUrl = `https://${gitConfig.user}.github.io${basePath}`;
const skillUrl = `${siteUrl}/skill/SKILL.md`;
const rawSkillUrl = `https://raw.githubusercontent.com/${gitConfig.user}/${gitConfig.repo}/${gitConfig.branch}/skills/picky/SKILL.md`;

/** Page bodies link to `/docs/...`; outside the site those links need the origin and base path. */
function absoluteLinks(markdown: string) {
  return markdown.replace(/(\]\(|href=")(\/docs[^\s")]*)/g, `$1${siteUrl}$2`);
}

/** Drops the self-closing visual components (`<Terminal />`, `<BenchExplorer />`, ...): they have no text form. */
function withoutVisuals(markdown: string) {
  return markdown.replace(/^<[A-Z]\w*\s*\/>[ \t]*\n+/gm, '');
}

export const docsLlms = llms(source, {
  renderPage: async (page) => `# ${page.data.title} (${siteUrl}${page.url}/)

${absoluteLinks(withoutVisuals(await page.data.getText('processed')))}`,
});

/** Pages in the order of the sidebar, grouped by its separators. */
function sections() {
  const groups: { title: string; pages: Page[] }[] = [{ title: 'Start here', pages: [] }];
  for (const node of source.getPageTree().children) {
    if (node.type === 'separator') {
      // The last sidebar group is the changelog: the llms.txt standard calls skippable links "Optional".
      groups.push({ title: node.name === 'Project' ? 'Optional' : String(node.name), pages: [] });
      continue;
    }
    for (const item of node.type === 'page' ? [node] : flattenTree([node])) {
      const page = source.getNodePage(item);
      if (page) groups[groups.length - 1].pages.push(page);
    }
  }
  return groups;
}

/** `/llms.txt`: the index in the llms.txt format, linking to the markdown of every page. */
export function llmsIndex() {
  const link = (page: Page) => {
    const description = page.data.description ? `: ${page.data.description}` : '';
    return `- [${page.data.title}](${siteUrl}${getPageMarkdownUrl(page).url})${description}`;
  };

  return [
    `# ${appName}`,
    '',
    '> picky runs only the tests, lint and type checks a pull request can affect in GitHub Actions, for JS/TS, PHP, Kotlin and Rust. A dependency graph decides what must run, a model (Jev) judges the tests the graph cannot link, and any doubt, error or timeout runs everything.',
    '',
    `- Version v${version}, MIT license. Source: ${repoUrl}`,
    `- Install: download \`picky.mjs\` and \`picky.mjs.sha256\` from ${releaseUrl}/, verify the checksum, run \`node picky.mjs init\` (prints a diff), then \`node picky.mjs init --write\`. Needs Node 20+.`,
    '- CI setup: `init` puts a `# picky` marker on each test, lint and type-check command it recognizes. Config is an optional `.picky.yml` with `mode`, `model`, `critical` and `low`.',
    '- Rollout: it starts in `mode: shadow` (everything runs, picky logs what it would skip). Switch to `enforce` only after PRs show no `picky shadow miss`.',
    '- The model needs a `PICKY_API_KEY` repository secret. `model: none` is graph-only: no network call, and no diff leaves the runner.',
    '- Only `pull_request` events are narrowed. Any other event, a missing key or any doubt runs everything.',
    '',
    'Page links below point to the markdown version of each page.',
    '',
    '## For agents',
    '',
    `- [Agent skill (SKILL.md)](${skillUrl}): how to install picky, migrate workflows, debug a decision and tune \`.picky.yml\`. Also at ${rawSkillUrl}`,
    `- [Full documentation](${siteUrl}/llms-full.txt): every page below in one markdown file`,
    '',
    ...sections().flatMap(({ title, pages }) => (pages.length ? [`## ${title}`, '', ...pages.map(link), ''] : [])),
  ].join('\n');
}

/** `/llms-full.txt`: every page in sidebar order, so the quick start and install come first. */
export async function llmsFull() {
  const pages = sections().flatMap(({ pages }) => pages);
  return (await Promise.all(pages.map((page) => docsLlms.page(page)))).join('\n\n');
}
