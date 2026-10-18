import { releaseUrl } from './shared';

/** The setup prompt for coding agents. Keep it in sync with the one in content/docs/agents.mdx. */
export const agentPrompt = `Install picky in this repo and migrate its GitHub Actions workflows to it.
picky runs only the tests, lint and type checks a pull request can affect.

1. Read the instructions first. Do not guess flags or behavior.
   - Skill: https://raw.githubusercontent.com/ahmedragab20/picky/main/skills/picky/SKILL.md
   - If you cannot fetch it, use the full docs: https://ahmedragab20.github.io/picky/llms-full.txt
2. Download picky.mjs and picky.mjs.sha256 from
   ${releaseUrl}/
   and verify the checksum. Stop if it fails.
3. Run \`node picky.mjs init\` (a dry run). Show me the full diff and the notes it
   prints, and explain anything it skipped.
4. Wait for my OK. Then run \`node picky.mjs init --write\` and delete the downloaded files.
5. Keep \`mode: shadow\` in .picky.yml. Do not switch to enforce.
6. Do not change or delete any test. Do not remove existing path filters or
   --changedSince-style flags without asking me. Do not mark commands picky does not recognize.
7. Do not commit, push or open a PR unless I ask.
8. Finish with a list of what I must do myself. At minimum: add the PICKY_API_KEY
   repository secret (Settings -> Secrets and variables -> Actions), or set
   \`model: none\` for graph-only mode.`;
