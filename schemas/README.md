# Pinned herdr API schemas

The herdr JSON API schema is generated from the herdr binary and embedded in it
(`herdr api schema --json`). This directory pins the exact schemas herdr-hub is
built and tested against.

| File | Source | protocol | schema_version |
| --- | --- | --- | --- |
| `herdr-api-0.9.0-protocol22.json` | herdr repo checkout, commit `61ca85d5` (`docs/next/api/herdr-api.schema.json`) — the commit SPEC.md was written against | 22 | 1 |
| `herdr-api-0.8.2-protocol20-live.json` | exported from the installed herdr 0.8.2 binary on this machine (2026-09-12) | 20 | 1 |

herdr-hub treats the 0.9.0 schema as the canonical wire contract and verifies
protocol compatibility at connect time. The running server used for live
testing is 0.8.2 (protocol 20); its method list is a strict subset of 0.9.0's,
so newer methods (e.g. `pane.scroll`) are availability-gated at runtime and
fail with `hub.action_not_allowed` on older servers.

Re-export with:

```bash
herdr api schema --json > schemas/herdr-api-<version>-protocol<N>.json
```
