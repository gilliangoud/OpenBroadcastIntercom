# Contributing

Thanks for helping improve RedLine.

Before opening a pull request:

1. Run `cargo fmt --all -- --check`.
2. Run the focused test suites for the crates you touched.
3. Run `tools/check-generated-artifacts.sh`.
4. Run `tools/check-public-secrets.sh`.

Do not commit local state, credentials, signing material, debug recordings,
generated build output, or unreviewed model assets. Large approved model assets
must be tracked through Git LFS and documented in `THIRD_PARTY_NOTICES.md`.

For iOS development, use local environment variables such as
`APPLE_DEVELOPMENT_TEAM`; do not hard-code team identifiers into source files.

