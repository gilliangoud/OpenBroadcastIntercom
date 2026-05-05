# Security Policy

## Reporting

Please report security issues privately to the project maintainers. Do not open
public issues for vulnerabilities, leaked credentials, or device provisioning
material.

## Supported Scope

This project is an early development intercom system intended for trusted local
networks. The server control and admin surfaces can be protected with bearer or
basic-auth tokens, but the project does not currently provide WAN-safe TLS,
identity federation, NAT traversal, or push-notification infrastructure.

## Public Repository Rules

- Never commit Wi-Fi credentials, Apple signing files, provisioning profiles,
  private keys, local state JSON, debug audio, or generated build output.
- Rotate any credential that has appeared in the repository or terminal logs.
- Run `tools/check-public-secrets.sh` and `tools/check-generated-artifacts.sh`
  before publishing a branch.

