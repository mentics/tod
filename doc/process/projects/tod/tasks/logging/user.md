# Logging

Project: `doc/process/projects/tod/`

## Goal

Users can view tod’s own diagnostic logs for troubleshooting.

## Requirements

1. Discoverable log directory — The diagnostic log directory path is visible in application settings so the user can open it with an external tool.
2. On by default — The default logging level is info.
3. Verbosity control — Users can raise or lower diagnostic logging verbosity in application settings.
4. Survive quit and relaunch — Diagnostic logs written before a tod quit remain available after relaunch.
5. Size-bounded storage — Maximum on-disk diagnostic log storage size is configurable in application settings as an integer kilobytes value from 1 through 104857600 inclusive (100 GiB), default 51200 KB, with rotation or pruning when the configured cap is exceeded.

## Constraints

1. No secrets in logs — Diagnostic logs must not contain secrets or credentials (including API tokens and passwords).
2. Local-only logs — Diagnostic logs remain on the user’s machine only; tod does not automatically upload or ship them off-machine.
