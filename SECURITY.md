# Security policy

## Supported versions

None yet. Iceblink is a pre-alpha spike with no releases, so there are no versions to patch. Fixes
land on `main`.

## Reporting a vulnerability

Report it privately through GitHub's security advisories: open this repository's **Security**
tab and choose **Report a vulnerability**.

Please don't open a public issue, pull request or discussion for a suspected vulnerability.

Include what you can:

- what an attacker can do, and the impact
- the steps or a minimal example that reproduces it
- the commit or version you tested
- any suggested fix

Iceblink's contract is read-only, so anything that lets a client write, delete or commit data, read
outside the configured warehouse, run SQL outside the allow-list or leak a secret is in scope.
We'll keep you updated on the advisory and credit you in it unless you'd rather stay anonymous.
