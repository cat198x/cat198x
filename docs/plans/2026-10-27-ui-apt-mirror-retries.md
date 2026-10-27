# UI dependency installation

PR #104's UI check exhausted three five-minute attempts downloading from
azure.archive.ubuntu.com. The retry loop bounded time but never changed mirror.

Reuse Emu198x's `scripts/ci/apt-install.sh` in Cat198x and call it from the UI job
in `.github/workflows/ci.yml`. The dependency list stays the same. Validate shell
syntax, workflow lint and a hosted UI run before merging; the hosted run must
reach Clippy and tests, not merely finish apt successfully.
