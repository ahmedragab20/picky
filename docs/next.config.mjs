import { createMDX } from 'fumadocs-mdx/next';

const withMDX = createMDX();

// GitHub Pages serves the site under /<repo>; the deploy workflow sets DOCS_BASE_PATH.
const basePath = process.env.DOCS_BASE_PATH ?? '';

/** @type {import('next').NextConfig} */
const config = {
  output: 'export',
  reactStrictMode: true,
  basePath,
  trailingSlash: true,
  images: { unoptimized: true },
  env: { NEXT_PUBLIC_BASE_PATH: basePath },
  // The repo root has its own lockfile; this app is self-contained.
  turbopack: { root: import.meta.dirname },
};

export default withMDX(config);
