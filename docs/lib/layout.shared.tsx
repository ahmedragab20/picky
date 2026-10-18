import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared';
import { Logo } from '@/components/logo';
import { repoUrl } from './shared';

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: <Logo />,
    },
    githubUrl: repoUrl,
    links: [
      { text: 'Docs', url: '/docs', active: 'nested-url' },
      { text: 'Benchmarks', url: '/docs/benchmarks' },
      { text: 'For agents', url: '/docs/agents' },
      { text: 'Changelog', url: '/docs/changelog' },
    ],
  };
}
