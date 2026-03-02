# Slack Integration IT Requirements

Status: initial IT review received (Business+; custom app creation allowed; org deployment available via IT).

## Confirmed / Requested Answers

1. Can we create/install an internal Slack app in this org?
- IT response: yes, app can be created; IT can deploy org-wide.

2. Is this Enterprise Grid with required org-wide deploy?
- IT response: Business+ (not Enterprise Grid in the response), but IT can deploy to org.

3. Are custom apps allowed or only marketplace apps?
- IT response: custom/internal app path is allowed.

4. Are user tokens permitted for read-only use cases?
- IT response: yes; user OAuth scope is preferred to avoid broader app access.

5. Is token rotation required?
- IT response: relaxed security currently; rotation is recommended if feasible.

6. Redirect URI restrictions?
- IT response: no explicit allow/deny list currently; configure in app settings.

7. Channel/data access restrictions?
- IT response: user-scope access should map to the user’s existing channel visibility.

8. Compliance/data handling constraints?
- IT response: avoid PII/sensitive data in derived artifacts; ensure encrypted storage/transit and audit trail in downstream systems.

## Minimum Approved Scope Set

- `channels:read`
- `groups:read`
- `im:read`
- `mpim:read`
- `channels:history`
- `groups:history`
- `im:history`
- `mpim:history`
- `users:read`

## Nice-to-Have Scope Set

- `search:read`
- `users:read.email` (only if explicitly needed)

## Remaining Governance Decisions

- Token rotation policy and cadence for production.
- Credential ownership (who rotates/revokes app credentials).
- Data classification/storage policy for conversation-derived todos.
