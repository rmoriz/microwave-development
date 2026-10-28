# Agent MCP homepage section — design

- **Date:** 2026-10-28
- **Status:** Approved (2026-10-28)
- **Location:** `website/homepage`, plus extensions to the existing `docs/mcp.md`
- **Scope:** Short change. The site structure, stack, and visual system stay as
  they are.

## Intent

Make the experimental MCP endpoint a visible sales argument for
Microwave without implying that the server contains an AI model.

The angle is “AI-enhanced cooking”: an agent can work in the kitchen, but the
visitor brings the agent and the model. Microwave provides the safe surface —
an MCP endpoint with organization-scoped tokens, Chef ACLs for scoped
principals, and write access only when enabled. Concrete agent workflows carry
the sales argument; the technical and experimental limits stay visible on the
page.

## Goals

- Explain in one scan what an agent can do with Microwave and why that is
  useful to an operator.
- Show three concrete workflows instead of a bare feature list.
- Keep the claim honest: experimental, opt-in, no built-in model, no UI.
- Give evaluators a path to the setup and tool reference in `docs/mcp.md`.
- Add the angle to discovery surfaces (hero, navigation, compatibility, known
  gaps, FAQ) without turning the page into an AI landing page.

## Non-goals

- A live demo, playground, chat UI, or hosted MCP endpoint.
- Naming a single AI vendor as required. The copy says “any MCP-capable client”
  and may give Claude Desktop or Cursor as examples.
- Prompt engineering advice, model comparisons, or benchmark claims.
- New pricing, support packages, or a separate page.

## Anti-goals

- “AI-powered” as a claim about the server itself.
- Implying that writes are on by default or that the endpoint is production
  ready.
- Hiding the experimental status in small print.
- A second visual language: no chat bubbles, neon gradients beyond the existing
  accent, or fake terminal windows.

## Constraints

- English copy, static Astro, no runtime requests, no new dependency.
- The existing visual system applies: dark stage, hairline rules, mono numerals,
  gradient accent, native `<details>`.
- Every factual claim is traceable to the implementation or `docs/mcp.md`:
  endpoint shape, token scope, scoped-principal ACLs, the superuser default,
  read/write split, opt-in write switch, startup warning for the superuser
  default, `allowed_hosts`, reverse-proxy advice, structured tool-call logs
  without a durable audit table, and no token expiry.
- The section must be useful without JavaScript and pass the existing axe gate.
- The site keeps its “no invented claims” rule.

## Approaches

The three directions agreed with the owner, combined rather than chosen:

- **A — “Bring your own agent.”** The framing: Microwave does not ship a model;
  it exposes tools to the agent the visitor already uses. Strongest honesty
  anchor.
- **B — “AI-enhanced cooking” as a visible hook.** Used as the section eyebrow
  and a hero badge so the angle is discoverable without changing the H1.
- **C — Concrete workflows.** Three scenes (“Why did it fail?”, “What is
  drifting?”, “Prepare the rollout”) showing the read and write split.

**Decision:** all three. C supplies the body of a dedicated section; A keeps it
credible; B makes the angle scannable. An additional under-the-hood panel,
compatibility row, known gap, and FAQ entry cover the evaluator and the honest
limits.

## Page changes

### Hero

- Add `Agent-ready` to the eyebrow:
  `Independent · Open source · Rust · Agent-ready`.
- Add a fourth badge: `MCP for AI agents`.
- The H1 and subhead do not change. The proof strip does not gain an MCP stat
  because there is no meaningful number to claim yet.

### New section: `#agents`, after `#product` / Cinc and before Compatibility

Structure:

- Eyebrow: `AI-enhanced cooking`.
- H2: `Your sous-chef speaks MCP.`
- Lead: “Microwave does not ship a model. It ships the kitchen.” Then: an
  experimental MCP endpoint lets an MCP-capable agent read nodes, environments,
  cookbooks, and run reports, and change nodes, environments, and cookbook
  versions when you allow it.
- Three workflow cards with mono numerals:
  1. **Why did web-01 fail?** The agent reads the latest run and converge
     errors, including the failing resource and error class, without an SSH
     session.
  2. **What is drifting?** The agent filters failed runs and compares
     environments and cookbook versions before a rollout.
  3. **Prepare the change.** With writes enabled, the agent creates or updates
     the node, environment, or cookbook version through the same validation
     path as the HTTP API.
- Trust line, set apart: “Per-organization bearer tokens, mapped to a Chef
  principal. Use a scoped user: the superuser default skips ACLs.”
- Opt-in line: “Read first. Writes are off until you turn them on. The server
  warns if a write-enabled organization still uses the superuser default.”
- Endpoint example in inline code:
  `POST /organizations/<org>/mcp`.
- Badge or label: `Experimental`.
- Link: `MCP setup and tool reference ↗` to `docs/mcp.md`.
- Fine print: “Bring your own agent. Microwave provides the endpoint and the
  permissions; the model stays on your side.”
- Visual: the section uses a full-width gradient hairline at the top, the three
  cards in a row (stacked on mobile), and the existing section rhythm. No image.

### Navigation

Add `Agents` between `Product` and `Compatibility` in both desktop and mobile
navigation.

### Compatibility

- API list: add `Per-organization MCP endpoint`.
- Tooling list: add `MCP-capable AI agents`.

### Under the hood

Add a seventh panel: **“Agents in the kitchen.”**

- MCP is a Streamable HTTP endpoint per organization, authenticated with a
  per-organization bearer token that maps to a Chef principal.
- Read tools cover nodes, environments, cookbooks, runs, and converge errors;
  write tools cover nodes, environments, and cookbook versions.
- Writes are opt-in per organization and reuse the HTTP API's ACL,
  normalization, and validation paths. The default principal is the superuser
  and bypasses ACLs, so a scoped user is the production setting.
- Tool calls emit one structured log line with organization, principal, tool,
  duration, and result count; writes also log the target. There is no durable
  audit table.
- Link to `docs/mcp.md`.
- Keep it to three short paragraphs.

### Known gaps

Add one item:

`MCP is experimental: no durable audit table, no per-tool scopes or token expiry, and no UI. Tool calls are only structured log lines. Remote agents need allowed_hosts and a reverse-proxy review.`

### FAQ

Add a ninth question:

**Can an AI agent operate the server?**

Answer: “Yes, through the experimental MCP endpoint. Bring an MCP-capable
client, give it a per-organization token and a scoped principal, and choose
whether writes are enabled. ACLs apply to scoped users; the superuser default
skips them. It is not a built-in assistant, and tool calls are only server log
lines, not a durable audit table.”

### Final CTA

Add one sentence below “Bring your own cookbooks.”:
“Then bring your own agent.” No new button.

## Copy rules

- Say “agent”, not “assistant” or “copilot”.
- Say “experimental” in the section and in every navigation-level mention.
- Never say “AI-powered”, “autonomous”, “self-healing”, or “built-in AI”.
- “Read” before “write”; destructive actions are not sold as automatic.
- Examples use role names (`web-01`), not invented customers.

## `docs/mcp.md`

Extend the existing guide the website links to. Sections:

1. What it is and experimental status.
2. Enablement: `[mcp]` config, per-org entry, token via config or
   `microwave-ctl set-secret mcp <org> <token>`, `writes = false`, principal,
   org membership.
3. Remote access: `allowed_hosts`, `server.public_url` fallback, loopback
   default, reverse-proxy recommendation, `allowed_origins` for browser clients.
4. Client setup: generic Streamable HTTP client with `Authorization: Bearer`,
   plus Claude Desktop and Cursor examples.
5. Report storage prerequisite: `[data_collector] store = "local" | "both"` and
   `keep_runs`; reports are pruned.
6. Tool reference: read tools and write tools, arguments, limits,
   `max_results`, `max_result_bytes`, `max_upload_bytes`, upload payload.
7. Permissions: superuser default and its startup warning, scoped principals,
   ACL behavior, deleted-node reports.
8. Limitations: no durable audit table (structured logs only), no expiry, no
   per-tool scopes, no UI, no snapshot coverage of `node_runs`, orphaned blobs,
   experimental status.
9. A short “verify it works” checklist.

The guide follows the approved design
`docs/superpowers/specs/2026-10-27-mcp-run-reports-design.md`, but describes
the shipped code where it differs.

## Tests and verification

- Extend `tests/content/site.test.mjs`:
  - `#agents` exists exactly once and sits between `#product` and
    `#compatibility`.
  - The section contains `Bring your own agent`, `Experimental`, the endpoint
    shape, and the `docs/mcp.md` link.
  - `MCP for AI agents` badge and `Agent-ready` eyebrow are present.
  - `#under-the-hood details` count becomes 7 and `#faq details` becomes 9.
  - Nav contains `/#agents`.
  - The body does not contain `AI-powered` or `built-in AI`.
- Add the known-gap phrase and FAQ text to the existing content assertions.
- Keep the existing forbidden-word check green.
- Run `npm run check`, `npm run test:content`, and `npm run test:e2e`.
- Check the link checker accepts the new relative docs link.
- Verify no horizontal overflow at 375px and keyboard order through the new
  cards and links.
- Run the axe gate; the workflow cards use a real list and headings.

## Acceptance criteria

- A visitor understands within the section that the agent is external, MCP is
  experimental, reads are available, and writes are opt-in.
- Every claim in the section can be traced to `docs/mcp.md` or the code.
- The page still builds statically and passes type, content, browser, and axe
  checks.
- No new runtime request or dependency is introduced.
- The page does not imply a built-in model, default write access, or production
  readiness.

## Left to the builder

- Final headline and card wording, within the copy rules.
- Whether the section CTA is a text link or a secondary button; pick what the
  surrounding sections use for equivalent weight.
- Exact CSS class names and spacing, following `components.css`.
- Whether `docs/mcp.md` needs small code-example fixtures to stay truthful; it
  must match the actual config keys and tool names.
