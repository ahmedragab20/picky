# picky hook. Bash sources this before every `run:` step through BASH_ENV.
# Steps return immediately and run unchanged unless the step or its workflow file has a
# `# picky` marker, or the step runs an install command (for job-level skips).
# Compatible with bash 3.2+.
__picky_wf="${GITHUB_WORKFLOW_REF:-}"; __picky_wf="${__picky_wf#*/}"; __picky_wf="${GITHUB_WORKSPACE:-.}/${__picky_wf#*/}"; __picky_wf="${__picky_wf%%@*}"
if [ -z "${PICKY_HOOK_ACTIVE:-}" ] && [ -f "$0" ] \
  && { grep -Eq '(^|[[:space:]])#[[:space:]]*picky([[:space:]]|$)' "$__picky_wf" 2>/dev/null || grep -Eq '(^|[[:space:]])#[[:space:]]*picky([[:space:]]|$)|(^|[[:space:]])(npm +(ci|clean-install|install|i)|pnpm +(install|i)|yarn|bun +(install|i)|composer +(install|i)|playwright +install)([[:space:]]|$)' "$0" 2>/dev/null; }; then
  __picky_bin="${PICKY_BIN:-$(command -v picky 2>/dev/null)}"
  if [ -n "$__picky_bin" ] && [ -x "$__picky_bin" ]; then
    export PICKY_HOOK_ACTIVE=1
    export PICKY_SHELL_FLAGS="$-"
    if shopt -qo pipefail; then export PICKY_PIPEFAIL=1; else export PICKY_PIPEFAIL=0; fi
    exec "$__picky_bin" hook-exec "$0" "$@"
  fi
  echo "picky: binary not found; running this step unchanged" >&2
fi
