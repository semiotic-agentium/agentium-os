# Security Policy

Agentium OS executes agents, brokers tools, and handles secrets and provenance
data, so we take security reports seriously and appreciate coordinated
disclosure.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.** A public
report discloses the problem to everyone before a fix exists.

Instead, report privately through GitHub's private vulnerability reporting:

- Go to the [**Security** tab](https://github.com/semiotic-agentium/agentium-os/security/advisories/new)
  and open a new draft advisory, or
- Use the **Report a vulnerability** button on the repository's Security tab.

Please include enough detail to reproduce: affected component or crate, the
version or commit, a description of the impact, and step-by-step reproduction
where possible.

## Supported versions

The project is pre-1.0 and ships from the `main` branch. Security fixes are
applied to `main`; there are no separately maintained release branches at this
time.

| Branch | Supported |
| ------ | --------- |
| `main` | ✅        |
| Older commits / tags | ❌ |

## What to expect

- **Acknowledgement** within **3 business days** of your report.
- An initial assessment and, where confirmed, a remediation plan.
- Coordination with you on disclosure timing once a fix is available, and
  credit in the advisory if you would like it.

Thank you for helping keep Agentium OS and its users safe.
