# Security

Wrec records the screen, so treat any vulnerability that captures content
without consent, escapes the daemon socket boundary, or tampers with recording
output as high severity.

## Reporting

Open a [GitHub issue](https://github.com/shivamhwp/wrec/issues/new) and
include:

- what the issue is and where (app, CLI, daemon, capture engine, installer)
- steps to reproduce
- the impact you believe it has

You will get a response within a week.

## Scope notes

- The daemon listens on a unix socket at `~/.wrec/wrec.sock` and trusts local
  processes running as the same user. Anything that lets a *different* user or
  a sandboxed process drive recordings is a bug.
- Release artifacts are not notarized and only ad-hoc signed, so macOS
  Gatekeeper warns on the first launch of the app DMG. Download only from the
  official GitHub Releases page and clear the quarantine as described in the
  README.
- Downloads are verified automatically: the CLI installer checks the archive
  against the release's `SHA256SUMS` before installing, and `wrec update` and
  the app's in-place updater verify each asset's published SHA-256 digest and
  refuse to install on any mismatch. For a manual download, fetch `SHA256SUMS`
  from the same release and verify the file you downloaded
  (`grep "  <asset-name>$" SHA256SUMS | shasum -a 256 -c -`; plain
  `shasum -a 256 -c SHA256SUMS` expects every listed asset to be present).
  Confirm provenance with `gh attestation verify <file> --repo shivamhwp/wrec
  --signer-workflow shivamhwp/wrec/.github/workflows/release.yml`, which
  proves the asset was built by this repository's release workflow.
