import { createGetUrl } from 'fumadocs-core/source';
import meta from '@/data/meta.json';

export const appName = 'picky';
export const docsRoute = '/docs';
export const docsContentRoute = '/llms.mdx/docs';
export const basePath = process.env.NEXT_PUBLIC_BASE_PATH ?? '';

export const gitConfig = {
  user: 'ahmedragab20',
  repo: 'picky',
  branch: 'main',
};

export const repoUrl = `https://github.com/${gitConfig.user}/${gitConfig.repo}`;
export const version = meta.version;
export const releaseUrl = `${repoUrl}/releases/download/v${version}`;
/** Picks the release binary for the machine it runs on: picky-<os>-<cpu>. */
export const platformShell = `os=$(uname -s | tr A-Z a-z); cpu=$(uname -m | sed 's/x86_64/x64/; s/aarch64/arm64/')`;

const getContentUrl = createGetUrl(docsContentRoute);

export function getPageMarkdownUrl(page: { slugs: string[]; locale?: string }) {
  const segments = [...page.slugs, 'content.md'];

  return { segments, url: getContentUrl(segments, page.locale) };
}
