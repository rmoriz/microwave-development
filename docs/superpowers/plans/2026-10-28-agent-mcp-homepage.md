# Agent MCP Homepage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an honest, sales-oriented MCP section to the homepage so operators see agent-driven cooking as a reason to evaluate Microwave.

**Architecture:** One new Astro section between the Cinc and compatibility blocks, backed by a repository guide. Discovery surfaces (hero, nav, compatibility, under-the-hood, known gaps, FAQ, final CTA) carry the angle without changing the H1 or visual language. Content tests pin the section order, exact claims, link target, and the absence of “AI-powered”/“built-in AI”.

**Tech Stack:** Astro 7, vanilla CSS, Node test runner with Cheerio, Playwright + axe.

**Spec:** `docs/superpowers/specs/2026-10-28-agent-mcp-homepage-design.md`

## Global Constraints

- English copy. The body must not contain `AI-powered`, `built-in AI`, `autonomous`, `self-healing`, or “assistant” as a product term.
- MCP is called `Experimental` in the section and every navigation-level mention.
- Read before write; writes are described as off until enabled.
- No new runtime request, dependency, client-side framework, or build step.
- All claims trace to `docs/mcp.md` or the implementation. Endpoint shape: `POST /organizations/<org>/mcp`. Limits: `allowed_hosts`, reverse-proxy review, no durable audit table (structured tool-call logs only), scoped principals required for ACLs, no token expiry, no per-tool scopes, no UI.
- Existing H1 stays exactly `Get cooking without becoming a chef.`
- Every task ends with a passing verification and a commit.

## Review Focus

- Long endpoint text in a narrow card at 375px — it must wrap without horizontal overflow.
- JavaScript disabled — the section, cards, and details content stay visible.
- Screen-reader structure — one `h2`, three `h3` workflow cards in a real list.
- The guide link — `docs/mcp.md` must exist on `main` before launch and use the GitHub `blob/main` URL.
- Copy drift — writes must not read as default or automatic, the section must not imply a built-in model, and ACL wording must mention the superuser default.

---

### Task 1: Complete the MCP guide

**Files:**
- Modify: `docs/mcp.md` (already exists; extend, do not replace)

**Interfaces:**
- Produces: the `docs/mcp.md` file and anchors used by the website link and content test.
- Consumes: implementation facts from `crates/microwave-config/src/config.rs`, `crates/microwave-api/src/mcp/tools.rs`, and `crates/microwave-api/src/mcp/auth.rs`.

- [ ] **Step 1: Verify the guide against the code**

Run:

```sh
rg -n "Authorization|set-secret|allowed_hosts|writes = false|list_converge_errors|upload_cookbook_version" docs/mcp.md
rg -n "struct UploadCookbookArgs|effective_allowed_hosts|fn bearer_token" crates
```

Expected: every key, header, and tool name in the guide matches the source. Correct any mismatch in the guide, not the source.

- [ ] **Step 2: Add client examples after the generic `mcpServers` block**

Insert these two subsections immediately after the generic JSON block in `## Client configuration` and before “The bearer header must be sent…”:

````markdown
### Claude Desktop

Add the same entry to `claude_desktop_config.json` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`) and restart
Claude Desktop:

```json
{
  "mcpServers": {
    "microwave-acme": {
      "type": "http",
      "url": "https://chef.example.com/organizations/acme/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

### Cursor

Add the entry to the project `.cursor/mcp.json` or the user-level
`~/.cursor/mcp.json`, then reload Cursor:

```json
{
  "mcpServers": {
    "microwave-acme": {
      "url": "https://chef.example.com/organizations/acme/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```
````

- [ ] **Step 3: Add a verification checklist before `## Limitations`**

```markdown
## Verify it works

1. Run `microwave-ctl --config /etc/microwave/microwave.toml check-config`.
2. Restart the server, connect the client, and call `tools/list`. With
   `writes = false`, only the 11 read tools appear; with `writes = true`, all 19
   tools appear.
3. Call `list_nodes` as the configured principal. A scoped non-superuser sees
   only nodes its ACLs allow.
4. If the run tools return nothing, check that `data_collector.store` is
   `"local"` or `"both"` and that reports have been received since enablement.
5. Tail the server log and confirm one structured line per tool call, including
   the organization, principal, tool, duration, and result count.
```

- [ ] **Step 4: Verify the headings and code fences**

Run:

```sh
awk '/^```/{n++} END{print ((n%2)==0) ? "fences ok" : "unbalanced fences"}' docs/mcp.md
rg -n "^## " docs/mcp.md
```

Expected: `fences ok` and the section order ends with `## Verify it works` then `## Limitations`.

- [ ] **Step 5: Commit**

```bash
git add docs/mcp.md
git commit -m "docs(mcp): add client examples and verification checklist"
```

---

### Task 2: Build the agent section

**Files:**
- Create: `website/homepage/src/components/AgentSection.astro`
- Modify: `website/homepage/src/data/site.ts`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: `site.github` from `src/data/site.ts` to build the guide URL.
- Produces: `site.mcp` as `https://github.com/rmoriz/microwave-development/blob/main/docs/mcp.md`; section id `agents`; classes `.agent-section`, `.agent-head`, `.agent-badge`, `.agent-lead`, `.agent-grid`, `.agent-card`, `.agent-note`; workflow list items with `h3` titles.

- [ ] **Step 1: Add the failing content test**

Append this test before the final `test('final CTA, footer, and legal pages'` block in `website/homepage/tests/content/site.test.mjs`:

```js
test('agent MCP sales section', () => {
  const text = $('body').text();
  const guide = 'https://github.com/rmoriz/microwave-development/blob/main/docs/mcp.md';

  assert.equal($('#agents').length, 1);
  assert.equal($('#product').nextAll('#agents').length, 1, '#agents follows #product');
  assert.equal($('#agents').nextAll('#compatibility').length, 1, '#agents precedes #compatibility');
  assert.match(text, /AI-enhanced cooking/);
  assert.match(text, /Your sous-chef speaks MCP\./);
  assert.match(text, /Microwave does not ship a model\. It ships the kitchen:/);
  assert.match(text, /Bring your own agent/);
  assert.match(text, /Experimental/);
  assert.match(text, /POST \/organizations\/<org>\/mcp/);
  assert.match(text, /Why did web-01 fail\?/);
  assert.match(text, /What is drifting\?/);
  assert.match(text, /Prepare the change\./);
  assert.match(text, /Use a scoped user: the superuser default skips ACLs/);
  assert.match(text, /Writes are off until you turn them on/);
  assert.match(text, /warns if a write-enabled organization still uses the superuser default/);
  assert.equal($(`#agents a[href="${guide}"]`).length, 1);
  assert.equal($('#agents h2').length, 1);
  assert.equal($('#agents h3').length, 3);
  assert.equal($('#agents ol.agent-grid > li').length, 3);
  assert.doesNotMatch(text, /AI-powered|built-in AI|autonomous|self-healing/);
  assert.equal(
    fs.existsSync(path.join(root, '../../docs/mcp.md')),
    true,
    'docs/mcp.md exists in the repository',
  );
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd website/homepage && npm run test:content`
Expected: FAIL with `#agents` count 0.

- [ ] **Step 3: Add the guide URL to `src/data/site.ts`**

After `docs:` add:

```ts
  mcp: 'https://github.com/rmoriz/microwave-development/blob/main/docs/mcp.md',
```

- [ ] **Step 4: Create `AgentSection.astro` with this exact copy**

```astro
---
import { site } from '../data/site';

const workflows = [
  {
    title: 'Why did web-01 fail?',
    body: 'The agent reads the latest run and the converge errors behind it, down to the failing resource and error class, without an SSH session.',
  },
  {
    title: 'What is drifting?',
    body: 'The agent filters failed runs, then compares environments and cookbook versions before you commit to a rollout.',
  },
  {
    title: 'Prepare the change.',
    body: 'With writes enabled, the agent creates or updates the node, environment, or cookbook version through the same validation and ACL path as the HTTP API.',
  },
];
---
<section id="agents" class="section agent-section reveal">
  <div class="container">
    <div class="agent-head">
      <div>
        <p class="eyebrow">AI-enhanced cooking</p>
        <h2>Your sous-chef speaks MCP.</h2>
      </div>
      <p class="agent-badge">Experimental</p>
    </div>

    <p class="agent-lead">
      Microwave does not ship a model. It ships the kitchen: an experimental
      MCP endpoint lets an MCP-capable agent read nodes, environments,
      cookbooks, and run reports, and change nodes, environments, and cookbook
      versions when you allow it.
    </p>

    <ol class="agent-grid">
      {workflows.map((workflow, index) => (
        <li class="agent-card">
          <span class="agent-index" aria-hidden="true">0{index + 1}</span>
          <h3>{workflow.title}</h3>
          <p>{workflow.body}</p>
        </li>
      ))}
    </ol>

    <div class="agent-foot">
      <p class="agent-trust">
        Per-organization bearer tokens, mapped to a Chef principal. Use a
        scoped user: the superuser default skips ACLs.
      </p>
      <p class="agent-optin">
        Read first. Writes are off until you turn them on. The server warns if
        a write-enabled organization still uses the superuser default.
      </p>
    </div>

    <div class="agent-call">
      <code>POST /organizations/&lt;org&gt;/mcp</code>
      <a class="text-link" href={site.mcp}>
        MCP setup and tool reference <span aria-hidden="true">↗</span>
      </a>
    </div>

    <p class="agent-note">
      Bring your own agent. Microwave provides the endpoint and the
      permissions; the model stays on your side.
    </p>
  </div>
</section>
```

- [ ] **Step 5: Import and mount it in `src/pages/index.astro`**

Add `import AgentSection from '../components/AgentSection.astro';` after the `CincSection` import, and place `<AgentSection />` after `<CincSection />`.

- [ ] **Step 6: Add styles to `src/styles/components.css`**

Append before `.section-head`:

```css
.agent-section {
  position: relative;
  border-top: 1px solid transparent;
  border-image: var(--gradient) 1;
}

.agent-head {
  display: flex;
  flex-wrap: wrap;
  gap: 20px;
  align-items: start;
  justify-content: space-between;
}

.agent-head h2 {
  margin-top: 16px;
  font-size: clamp(2.2rem, 4vw, 3.6rem);
}

.agent-badge {
  padding: 6px 11px;
  border: 1px solid rgba(255, 138, 61, 0.45);
  border-radius: 999px;
  background: rgba(255, 138, 61, 0.08);
  color: var(--orange);
  font-family: var(--font-mono);
  font-size: 0.72rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.agent-lead {
  max-width: 70ch;
  margin-top: 22px;
  color: #c9c6bd;
  font-size: clamp(1.05rem, 1.5vw, 1.2rem);
}

.agent-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  padding: 0;
  margin: 48px 0 0;
  border-block: 1px solid var(--line);
  list-style: none;
}

.agent-card {
  padding: 28px 24px;
  border-left: 1px solid var(--line);
}

.agent-card:first-child {
  border-left: 0;
  padding-left: 0;
}

.agent-index {
  color: var(--green);
  font-family: var(--font-mono);
  font-size: 0.72rem;
  letter-spacing: 0.08em;
}

.agent-card h3 {
  margin-top: 16px;
  font-size: 1.3rem;
}

.agent-card p {
  margin-top: 12px;
  color: var(--muted);
  font-size: 0.92rem;
}

.agent-foot {
  display: grid;
  grid-template-columns: minmax(0, 1.2fr) minmax(0, 0.8fr);
  gap: 24px;
  margin-top: 32px;
}

.agent-trust,
.agent-optin {
  padding-left: 16px;
  border-left: 2px solid var(--green);
  color: var(--text);
  font-size: 0.96rem;
}

.agent-optin {
  border-left-color: var(--orange);
}

.agent-call {
  display: flex;
  flex-wrap: wrap;
  gap: 20px;
  align-items: center;
  margin-top: 28px;
}

.agent-call code {
  padding: 6px 9px;
  border: 1px solid var(--line);
  border-radius: 4px;
  background: rgba(244, 241, 232, 0.04);
  color: var(--text);
  font-size: 0.82rem;
  overflow-wrap: anywhere;
}

.agent-call .text-link {
  margin-top: 0;
}

.agent-note {
  max-width: 62ch;
  margin-top: 22px;
  color: var(--muted);
  font-family: var(--font-mono);
  font-size: 0.7rem;
  line-height: 1.7;
}
```

In the existing `@media (max-width: 900px)` block, add `.agent-grid` to the `grid-template-columns: 1fr` selector list and add:

```css
  .agent-card {
    border-left: 0;
    border-top: 1px solid var(--line);
    padding-left: 0;
  }

  .agent-card:first-child {
    border-top: 0;
  }

  .agent-foot {
    grid-template-columns: 1fr;
  }
```

- [ ] **Step 7: Run the content test**

Run: `cd website/homepage && npm run test:content`
Expected: PASS, including the new `agent MCP sales section` test.

- [ ] **Step 8: Commit**

```bash
git add website/homepage/src/components/AgentSection.astro website/homepage/src/data/site.ts website/homepage/src/pages/index.astro website/homepage/src/styles/components.css website/homepage/tests/content/site.test.mjs
git commit -m "feat(website): add agent MCP section"
```

---

### Task 3: Add the discovery surfaces

**Files:**
- Modify: `website/homepage/tests/content/site.test.mjs`
- Modify: `website/homepage/tests/e2e/home.spec.ts`
- Modify: `website/homepage/src/components/Hero.astro`
- Modify: `website/homepage/src/components/Nav.astro`
- Modify: `website/homepage/src/components/Compatibility.astro`
- Modify: `website/homepage/src/components/UnderTheHood.astro`
- Modify: `website/homepage/src/components/KnownGaps.astro`
- Modify: `website/homepage/src/components/FAQ.astro`
- Modify: `website/homepage/src/components/FinalCTA.astro`

**Interfaces:**
- Consumes: `#agents`, `docs/mcp.md`, and the copy from Task 2.
- Produces: nav links to `/#agents`, under-the-hood panel 07, FAQ item 9, known-gap item 7, compatibility rows, hero badge, final CTA sentence.

- [ ] **Step 1: Update the failing tests**

In the `hero and navigation` test, add:

```js
  assert.match($('body').text(), /Agent-ready/);
  assert.match($('body').text(), /MCP for AI agents/);
  assert.equal($('.nav-desktop a[href="/#agents"]').length, 1);
  assert.equal($('.nav-mobile a[href="/#agents"]').length, 1);
```

In `compatibility, under the hood, and known gaps`, change `6` to `7`, then add:

```js
  assert.match(text, /Per-organization MCP endpoint/);
  assert.match(text, /MCP-capable AI agents/);
  assert.match(text, /Agents in the kitchen\./);
  assert.match(text, /MCP is experimental: no durable audit table/);
  assert.match(text, /Tool calls are only structured log lines/);
```

In `migration, support, and FAQ`, change `8` to `9`, then add:

```js
  assert.match(text, /Can an AI agent operate the server\?/);
  assert.match(text, /not a built-in assistant/);
  assert.match(text, /superuser default skips them/);
  assert.match(text, /only server log lines, not a durable audit table/);
```

In `final CTA, footer, and legal pages`, add:

```js
  assert.match(text, /Then bring your own agent\./);
```

In `tests/e2e/home.spec.ts`, extend the mobile menu test with:

```ts
  await expect(page.locator('.nav-mobile a[href="/#agents"]')).toBeVisible();
```

and extend the anchor test after the first click:

```ts
  await page.locator('.nav-desktop a[href="/#agents"]').click();
  await expect(page).toHaveURL(/#agents$/);
  const agentTop = await page.locator('#agents').evaluate((element) => {
    return element.getBoundingClientRect().top;
  });
  expect(agentTop).toBeGreaterThanOrEqual(72);
```

In the JavaScript-disabled test, add:

```ts
  await expect(page.locator('#agents')).toBeVisible();
```

- [ ] **Step 2: Run to verify failure**

Run: `cd website/homepage && npm run test:content`
Expected: FAIL on `Agent-ready`.

- [ ] **Step 3: Hero and navigation**

In `Hero.astro`:

- Change the eyebrow to `Independent · Open source · Rust · Agent-ready`.
- Add `<li class="badge">MCP for AI agents</li>` before the AGPL badge.

In `Nav.astro`, add `{ href: '/#agents', label: 'Agents' },` after the Product entry.

- [ ] **Step 4: Compatibility, gaps, FAQ, final CTA**

`Compatibility.astro`: add `'Per-organization MCP endpoint',` to the API items and `'MCP-capable AI agents',` to the Tooling items. Remove `'Validation endpoints',` from API and `'Debian package',` from Tooling so both lists stay at six items.

`KnownGaps.astro`: append:

```js
  'MCP is experimental: no durable audit table, no per-tool scopes or token expiry, and no UI. Tool calls are only structured log lines. Remote agents need allowed_hosts and a reverse-proxy review.',
```

`FAQ.astro`: append before the support question:

```js
  {
    q: 'Can an AI agent operate the server?',
    a: 'Yes, through the experimental MCP endpoint. Bring an MCP-capable client, give it a per-organization token and a scoped principal, and choose whether writes are enabled. ACLs apply to scoped users; the superuser default skips them. It is not a built-in assistant, and tool calls are only server log lines, not a durable audit table.',
  },
```

`FinalCTA.astro`: change the paragraph to:

```astro
    <p>
      Keep the recipes. Choose a smaller server. Then bring your own agent. Add
      commercial support only when you want it.
    </p>
```

- [ ] **Step 5: Under-the-hood panel 07**

Insert after panel 06 and before `</div>` of `.under-list`:

```astro
      <details class="under-item">
        <summary>
          <span class="under-index" aria-hidden="true">07</span>
          <span>Agents in the kitchen.</span>
          <span class="under-plus" aria-hidden="true"></span>
        </summary>
        <div class="under-body">
          <p>
            MCP is a Streamable HTTP endpoint per organization, authenticated
            with a per-organization bearer token that maps to a Chef principal.
            Read tools cover nodes, environments, cookbooks, runs, and converge
            errors; write tools cover nodes, environments, and cookbook
            versions.
          </p>
          <p>
            Writes are opt-in per organization and reuse the HTTP API's ACL,
            normalization, and validation paths. The default principal is the
            superuser and bypasses ACLs, so a scoped user is the production
            setting.
          </p>
          <p>
            Tool calls emit one structured log line with organization,
            principal, tool, duration, and result count; writes also log the
            target. There is no durable audit table. The surface is otherwise
            experimental: no token expiry, per-tool scope, or UI.
          </p>
          <p>
            <a class="text-link" href={site.mcp}>MCP guide <span aria-hidden="true">↗</span></a>
          </p>
        </div>
      </details>
```

Add `mcp: `${site.github}/blob/main/docs/mcp.md`,` to the local `docs` object in `UnderTheHood.astro` with the other doc links.

- [ ] **Step 6: Run the content and browser tests**

Run: `cd website/homepage && npm run test:content && npm run test:e2e`
Expected: PASS, including the new nav, anchor, axe, and JavaScript-disabled assertions.

- [ ] **Step 7: Commit**

```bash
git add website/homepage/src/components/Hero.astro website/homepage/src/components/Nav.astro website/homepage/src/components/Compatibility.astro website/homepage/src/components/UnderTheHood.astro website/homepage/src/components/KnownGaps.astro website/homepage/src/components/FAQ.astro website/homepage/src/components/FinalCTA.astro website/homepage/tests/content/site.test.mjs website/homepage/tests/e2e/home.spec.ts
git commit -m "feat(website): surface agent MCP across the page"
```

---

### Task 4: Content rules and final verification

**Files:**
- Modify: `website/homepage/README.md`

**Interfaces:**
- Consumes: everything from Tasks 1–3.
- Produces: the documented rule that MCP claims stay experimental and trace to `docs/mcp.md`.

- [ ] **Step 1: Add the content rule**

After the line “Keep factual claims traceable to the repository.” add:

```markdown
- MCP is always described as experimental. Reads come before writes; writes are
  opt-in. Keep the wording traceable to `docs/mcp.md` and do not use
  “AI-powered” or “built-in AI”. The content test enforces this.
```

- [ ] **Step 2: Run the complete suite**

Run: `cd website/homepage && npm test`
Expected: `astro check` clean, all content tests pass, all Playwright tests pass, zero serious axe violations.

- [ ] **Step 3: Check the narrow layout and heading order**

Run: `cd website/homepage && npx playwright test tests/e2e/home.spec.ts --grep "no horizontal overflow"`
Expected: PASS at 320, 375, 768, and 1440px.

Then confirm with `rg -n "<h2|<h3" src/components/AgentSection.astro` that the section has one `h2` and three `h3`s, in that order.

- [ ] **Step 4: Confirm the guide link is committed on `main`**

Run: `git log --oneline -1 -- docs/mcp.md && git show --stat --oneline HEAD`
Expected: `docs/mcp.md` is present in history. If the guide commit exists only on a side branch, merge or commit it before launch.

- [ ] **Step 5: Commit**

```bash
git add website/homepage/README.md
git commit -m "docs(website): add MCP content rules"
```
