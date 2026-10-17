# Audit evidence

Reference: wonderwhy-er/DesktopCommanderMCP at f508e524469c26f6366e17dbfd3ee5a69d112127.

The reference directory is a partial snapshot of inspected source and tests. It is not a full checkout and cannot be used to claim an upstream build passed. All included bytes match the Git blob hashes listed in reference-manifest.json. The original MIT license is retained under reference/DesktopCommanderMCP/LICENSE.

## Reproduce

From the extracted package directory, use Node 24 or newer with node:module stripTypeScriptTypes available:

```bash
node audit/probes/process-output.mjs
node audit/probes/path-validation.mjs
```

The Node experimental warning about type erasure is expected. The process probe uses the real TerminalManager source body with a fake child process and stubbed dependencies. The path probe runs the real validation functions against disposable directories and a symlink. It requires a case-sensitive filesystem with symlink creation permitted. It removes only its own temporary fixture.

Results intentionally demonstrate incorrect behavior in the reference. A zero process exit code means the reproduction completed; it does not mean the upstream behavior is correct.

The recorded Linux path results show the inside path accepted, unrelated and symlink-outside paths rejected, and a distinct differently cased sibling incorrectly accepted.

No product implementation, complete upstream test execution, Windows validation, or production relay test is included.
