# Security Policy

## Supported versions

thewiki is in pre-alpha. There are no released versions to support. Once we ship `0.1.0`, this section will list supported version lines and their EOL dates.

## Reporting a vulnerability

**Please do not file public GitHub issues for security vulnerabilities.**

Report security issues privately via GitHub's [private vulnerability reporting](https://github.com/i-doll/thewiki/security/advisories/new).

Include:

- A description of the vulnerability and its potential impact.
- Steps to reproduce, ideally with a minimal proof-of-concept.
- Affected version(s) or commit SHA.
- Any suggested mitigation if you have one.

You can expect:

- **Acknowledgement** within 72 hours.
- **Initial assessment** within 7 days.
- **Coordinated disclosure** once a fix is available, with credit to the reporter unless you prefer anonymity.

## Out of scope

- Issues affecting deployments that disable the bundled auth in favour of a custom integration — report to the integrator.
- Findings that require physical access to the host or a privileged account on the host.
- Self-XSS or attacks that require the victim to paste attacker-controlled content into the editor without provocation.
