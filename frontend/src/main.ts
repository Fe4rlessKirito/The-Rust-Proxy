type Health = {
  status: string;
  fresh_accounts: number;
  provider_pools: ProviderPool[];
  reasons: string[];
  send_success_rate: number;
};

type ProviderPool = {
  provider: string;
  ready: number;
  target: number | null;
  generated: number | null;
  failed: number | null;
  dead: number | null;
  cooling: number | null;
};

type Bank = {
  mode: string;
  warm_accounts: number;
  pool_target: number;
  status: string;
};

type Proxies = {
  proxies: string[];
  proxy_count: number;
  provider_assignments: Record<string, string[]>;
  load: {
    window_requests: number;
    requests_per_minute: number;
  };
};

type UsageOverview = {
  sessions: number;
  messages: number;
  total_tokens: number;
  active_days: number;
  current_streak: number;
  longest_streak: number;
  peak_hour: string | null;
  favorite_model: string | null;
  daily: Record<string, number>;
  models: Record<string, number>;
  model_input_tokens: Record<string, number>;
  model_output_tokens: Record<string, number>;
};

type ModelList = {
  object: string;
  data: Array<{ id: string; label: string }>;
};

type ModelPricing = { input: number; output: number };

const MODEL_PRICING_PER_MILLION: Record<string, ModelPricing> = {
  "gpt-5-5": { input: 4, output: 12 },
  "gpt-5-4": { input: 3, output: 10 },
  "gpt-5-3": { input: 2.5, output: 8 },
  "gpt-5-1": { input: 2, output: 7 },
  "gpt-5": { input: 2, output: 6 },
  "gpt-5-mini": { input: 0.3, output: 1.2 },
  "gpt-4o": { input: 2.5, output: 10 },
  "gpt-4o-mini": { input: 0.15, output: 0.6 },
  "claude-sonnet-4-6": { input: 3, output: 15 },
  "claude-sonnet-4-5": { input: 3, output: 15 },
  "claude-sonnet-5": { input: 3, output: 15 },
  "glm-5-2": { input: 0.5, output: 2 },
  "claude-haiku-4-5": { input: 1, output: 5 },
  "claude-haiku-4": { input: 0.8, output: 4 },
  "sakana-namazu": { input: 0, output: 0 },
  "sakana-fugu": { input: 0, output: 0 },
  "sakana-fugu-ultra": { input: 0, output: 0 },
  "gemini-3-1-pro": { input: 2, output: 12 },
  "gemini-3-pro": { input: 2, output: 10 },
  "gemini-3-flash": { input: 0.3, output: 2.5 },
  "gemini-2.5-flash": { input: 0.3, output: 2.5 },
  "deepseek-v4-pro": { input: 0.6, output: 3 },
  "deepseek-v4-flash": { input: 0.2, output: 1 },
  "deepseek-r1": { input: 0.55, output: 2.2 },
  "grok-4": { input: 3, output: 15 },
  "qwen-3-max": { input: 0.8, output: 2.4 },
  "qwen-3-5-397b": { input: 0.8, output: 2.4 },
  "kimi-k2-6": { input: 0.6, output: 2.5 },
  "deepinfra-kimi-k2": { input: 0.5, output: 1.5 },
  "llama-3-3-70b-versatile": { input: 0.3, output: 0.9 },
  "faceb-openai/gpt-5": { input: 2, output: 6 },
  "faceb-openai/gpt-5.1": { input: 2, output: 7 },
  "faceb-openai/gpt-5.2": { input: 2.5, output: 8 },
  "faceb-openai/gpt-5.3-chat": { input: 3, output: 10 },
  "faceb-openai/gpt-5.4": { input: 3, output: 10 },
  "faceb-openai/gpt-5.5": { input: 4, output: 12 },
  "faceb-anthropic/claude-opus-4": { input: 15, output: 75 },
  "faceb-anthropic/claude-opus-4.1": { input: 15, output: 75 },
  "faceb-anthropic/claude-opus-4.5": { input: 15, output: 75 },
  "faceb-anthropic/claude-opus-4.6": { input: 15, output: 75 },
  "faceb-anthropic/claude-opus-4.7": { input: 15, output: 75 },
  "faceb-anthropic/claude-opus-4.8": { input: 15, output: 75 },
  "faceb-anthropic/claude-fable-5": { input: 3, output: 15 },
  "faceb-google/gemini-3.1-pro-preview": { input: 2, output: 12 },
  "faceb-google/gemini-3.5-flash": { input: 0.3, output: 2.5 },
  "faceb-google/gemini-2.5-pro": { input: 2, output: 10 },
  "faceb-google/gemini-2.5-flash": { input: 0.3, output: 2.5 },
  "faceb-qwen/qwen3-max": { input: 0.8, output: 2.4 },
  "faceb-qwen/qwen3-coder": { input: 0.8, output: 2.4 },
  "faceb-qwen/qwen3-coder-plus": { input: 0.8, output: 2.4 },
  "faceb-qwen/qwen3.5-397b-a17b": { input: 0.8, output: 2.4 },
  "faceb-mistralai/ministral-14b-2512": { input: 0.1, output: 0.4 },
  "faceb-mistralai/ministral-8b-2512": { input: 0.06, output: 0.24 },
  "faceb-mistralai/ministral-3b-2512": { input: 0.04, output: 0.16 }
};

const OPENCODE_MODELS = [
  "gpt-5-5",
  "gpt-5-4",
  "gpt-5-3",
  "gpt-5-1",
  "gpt-5",
  "gpt-5-mini",
  "gpt-4o",
  "gpt-4o-mini",
  "claude-sonnet-5",
  "claude-sonnet-4-6",
  "claude-sonnet-4-5",
  "claude-haiku-4-5",
  "claude-haiku-4",
  "sakana-namazu",
  "sakana-fugu",
  "sakana-fugu-ultra",
  "faceb-openai/gpt-5",
  "faceb-openai/gpt-5.1",
  "faceb-openai/gpt-5.2",
  "faceb-openai/gpt-5.3-chat",
  "faceb-openai/gpt-5.4",
  "faceb-openai/gpt-5.5",
  "faceb-anthropic/claude-opus-4",
  "faceb-anthropic/claude-opus-4.1",
  "faceb-anthropic/claude-opus-4.5",
  "faceb-anthropic/claude-opus-4.6",
  "faceb-anthropic/claude-opus-4.7",
  "faceb-anthropic/claude-opus-4.8",
  "faceb-anthropic/claude-fable-5",
  "faceb-google/gemini-3.1-pro-preview",
  "faceb-google/gemini-3.5-flash",
  "faceb-google/gemini-2.5-pro",
  "faceb-google/gemini-2.5-flash",
  "faceb-qwen/qwen3-max",
  "faceb-qwen/qwen3-coder",
  "faceb-qwen/qwen3-coder-plus",
  "faceb-qwen/qwen3.5-397b-a17b",
  "faceb-mistralai/ministral-14b-2512",
  "faceb-mistralai/ministral-8b-2512",
  "faceb-mistralai/ministral-3b-2512",
  "gemini-3-1-pro",
  "gemini-3-pro",
  "gemini-3-flash",
  "gemini-2.5-flash",
  "deepseek-v4-pro",
  "deepseek-v4-flash",
  "deepseek-r1",
  "grok-4",
  "glm-5-2",
  "qwen-3-max",
  "qwen-3-5-397b",
  "kimi-k2-6",
  "deepinfra-kimi-k2",
  "llama-3-3-70b-versatile"
];

const OPENCODE_CONFIG = `{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "leech-rs": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Leech-RS",
      "options": {
        "baseURL": "http://127.0.0.1:8000/v1"
      },
      "models": {
${OPENCODE_MODELS.map((model) => `        "${model}": {}`).join(",\n")}
      }
    }
  },
  "model": "leech-rs/gpt-5-4"
}`;

const TOOL_GUIDES = [
  {
    name: "OpenCode",
    protocol: "OpenAI-compatible",
    endpoint: "http://127.0.0.1:8000/v1",
    config: "%USERPROFILE%\\.config\\opencode\\opencode.json",
    note: "Use the config below. Model names are referenced as leech-rs/model-id."
  },
  {
    name: "Aider",
    protocol: "OpenAI-compatible",
    endpoint: "http://127.0.0.1:8000/v1",
    config: "CLI flags or environment variables",
    note: "Use an OpenAI-compatible base URL and any placeholder API key if your client requires one."
  },
  {
    name: "Continue",
    protocol: "OpenAI-compatible",
    endpoint: "http://127.0.0.1:8000/v1",
    config: "Continue config.json or assistant config",
    note: "Add Leech-RS as an OpenAI-compatible provider and select any /v1/models ID."
  },
  {
    name: "Cline / Roo Code",
    protocol: "OpenAI-compatible or Anthropic-compatible",
    endpoint: "http://127.0.0.1:8000/v1",
    config: "Extension provider settings",
    note: "Use OpenAI-compatible mode for /v1/chat/completions or Anthropic-compatible mode for /v1/messages."
  },
  {
    name: "Claude Code",
    protocol: "Anthropic-compatible",
    endpoint: "http://127.0.0.1:8000/v1/messages",
    config: "Claude Code custom Anthropic base URL setting, if supported by your installed version",
    note: "Leech-RS exposes /v1/messages. Point Claude Code at the local base URL when its version allows overriding Anthropic's endpoint."
  },
  {
    name: "Codex CLI",
    protocol: "OpenAI-compatible",
    endpoint: "http://127.0.0.1:8000/v1",
    config: "Codex CLI provider/base-url setting, if supported by your installed version",
    note: "Use the OpenAI-compatible chat completions endpoint and one of the listed model IDs."
  }
];

const currency = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  maximumFractionDigits: 4
});

function pricingFor(model: string): ModelPricing {
  return MODEL_PRICING_PER_MILLION[model] ?? { input: 0.5, output: 2 };
}

function estimateSpend(inputTokens: number, outputTokens: number, model: string): number {
  const pricing = pricingFor(model);
  return ((inputTokens / 1_000_000) * pricing.input) + ((outputTokens / 1_000_000) * pricing.output);
}

async function fetchJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url} failed: ${response.status}`);
  }
  return response.json() as Promise<T>;
}

function setText(id: string, value: string): void {
  const element = document.getElementById(id);
  if (element) element.textContent = value;
}

function clearElement(element: HTMLElement): void {
  while (element.firstChild) element.removeChild(element.firstChild);
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderToolGuideRows(): string {
  return TOOL_GUIDES.map((tool) => `
    <tr>
      <td>${escapeHtml(tool.name)}</td>
      <td>${escapeHtml(tool.protocol)}</td>
      <td><code>${escapeHtml(tool.endpoint)}</code></td>
      <td>${escapeHtml(tool.config)}</td>
      <td>${escapeHtml(tool.note)}</td>
    </tr>
  `).join("");
}

function appendCell(row: HTMLTableRowElement, value: string): void {
  const cell = document.createElement("td");
  cell.textContent = value;
  row.appendChild(cell);
}

function renderModelTable(overview: UsageOverview, models: ModelList): void {
  const table = document.getElementById("model-table");
  if (!table) return;

  const labels = new Map(models.data.map((model) => [model.id, model.label]));
  const rows = Object.entries(overview.models)
    .sort((a, b) => b[1] - a[1])
    .map(([model, tokens]) => {
      const inputTokens = overview.model_input_tokens?.[model] ?? 0;
      const outputTokens = overview.model_output_tokens?.[model] ?? tokens;
      const spend = estimateSpend(inputTokens, outputTokens, model);
      return `
        <tr>
          <td>${labels.get(model) ?? model}</td>
          <td>${model}</td>
          <td>${inputTokens.toLocaleString()}</td>
          <td>${outputTokens.toLocaleString()}</td>
          <td>${currency.format(spend)}</td>
        </tr>
      `;
    })
    .join("");

  table.innerHTML = rows || `<tr><td colspan="5">No model usage yet.</td></tr>`;
}

function renderProxies(proxies: Proxies): void {
  const list = document.getElementById("proxy-list");
  if (!list) return;
  clearElement(list);
  const items = proxies.proxies.length ? proxies.proxies : ["No proxies currently active."];
  for (const proxy of items) {
    const item = document.createElement("li");
    item.textContent = proxy;
    list.appendChild(item);
  }
}

function renderProxyAssignments(proxies: Proxies): void {
  const table = document.getElementById("provider-proxy-table");
  if (!table) return;
  clearElement(table);
  const entries = Object.entries(proxies.provider_assignments ?? {});
  if (!entries.length) {
    const row = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 3;
    cell.textContent = "No provider proxy assignments yet.";
    row.appendChild(cell);
    table.appendChild(row);
    return;
  }
  for (const [provider, providerProxies] of entries) {
    const row = document.createElement("tr");
    appendCell(row, provider);
    appendCell(row, String(providerProxies.length));
    appendCell(row, providerProxies.join(", ") || "direct");
    table.appendChild(row);
  }
}

function renderDailyUsage(overview: UsageOverview): void {
  const list = document.getElementById("daily-usage");
  if (!list) return;
  const entries = Object.entries(overview.daily)
    .sort(([a], [b]) => a.localeCompare(b))
    .slice(-7);
  list.innerHTML = entries.length
    ? entries
        .map(([day, tokens]) => `<li><strong>${day}</strong><span>${tokens.toLocaleString()} tokens</span></li>`)
        .join("")
    : "<li>No daily usage yet.</li>";
}

function renderProviderPools(pools: ProviderPool[]): void {
  const table = document.getElementById("provider-pool-table");
  if (!table) return;
  clearElement(table);
  if (!pools.length) {
    const row = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 6;
    cell.textContent = "No provider pool stats yet.";
    row.appendChild(cell);
    table.appendChild(row);
    return;
  }
  for (const pool of pools) {
    const row = document.createElement("tr");
    appendCell(row, pool.provider);
    appendCell(row, `${pool.ready}${pool.target === null ? "" : ` / ${pool.target}`}`);
    appendCell(row, String(pool.generated ?? "n/a"));
    appendCell(row, String(pool.failed ?? "n/a"));
    appendCell(row, String(pool.dead ?? "n/a"));
    appendCell(row, String(pool.cooling ?? "n/a"));
    table.appendChild(row);
  }
}

function providerPool(pools: ProviderPool[], provider: string): ProviderPool | undefined {
  return pools.find((pool) => pool.provider === provider);
}

function providerProxyCount(proxies: Proxies, provider: string): number {
  return (proxies.provider_assignments?.[provider] ?? []).length;
}

function setProviderCardValues(health: Health, proxies: Proxies): void {
  const useAi = providerPool(health.provider_pools ?? [], "use_ai");
  const sakana = providerPool(health.provider_pools ?? [], "sakana");
  const faceb = providerPool(health.provider_pools ?? [], "faceb");

  setText("use-ai-ready", `${useAi?.ready ?? 0}${useAi?.target === null || useAi?.target === undefined ? "" : ` / ${useAi.target}`}`);
  setText("use-ai-proxies", String(providerProxyCount(proxies, "use_ai")));
  setText("use-ai-mode", "Tor isolated");
  setText("use-ai-status", useAi && useAi.ready > 0 ? "Ready" : "Warming");

  setText("sakana-ready", String(sakana?.ready ?? 0));
  setText("sakana-cooling", String(sakana?.cooling ?? 0));
  setText("sakana-mode", "Direct egress");
  setText("sakana-status", (sakana?.ready ?? 0) > 0 ? "Session cached" : "Lazy");

  setText("faceb-ready", `${faceb?.ready ?? 0}${faceb?.target === null || faceb?.target === undefined ? "" : ` / ${faceb.target}`}`);
  setText("faceb-generated", String(faceb?.generated ?? 0));
  setText("faceb-dead", String(faceb?.dead ?? 0));
  setText("faceb-proxies", String(providerProxyCount(proxies, "faceb")));
}

async function loadDashboard(): Promise<void> {
  try {
    const [health, bank, proxies, overview, models] = await Promise.all([
      fetchJson<Health>("/health"),
      fetchJson<Bank>("/bank"),
      fetchJson<Proxies>("/proxies"),
      fetchJson<UsageOverview>("/usage/overview"),
      fetchJson<ModelList>("/v1/models")
    ]);

    const totalSpend = Object.entries(overview.models).reduce(
      (sum, [model, tokens]) => sum + estimateSpend(
        overview.model_input_tokens?.[model] ?? 0,
        overview.model_output_tokens?.[model] ?? tokens,
        model
      ),
      0
    );

    setText("server-status", health.status.toUpperCase());
    setText("pool-count", String(bank.warm_accounts));
    setText("pool-target", String(bank.pool_target));
    setText("tor-count", String(proxies.proxy_count));
    setText("request-rate", `${proxies.load.requests_per_minute.toFixed(2)} req/min`);
    setText("session-count", String(overview.sessions));
    setText("message-count", String(overview.messages));
    setText("token-total", overview.total_tokens.toLocaleString());
    setText("estimated-spend", currency.format(totalSpend));
    setText("favorite-model", overview.favorite_model ?? "n/a");
    setText("streak", `${overview.current_streak} day(s)`);
    setText("peak-hour", overview.peak_hour ?? "n/a");
    setText("reasons", health.reasons.join(", "));
    setProviderCardValues(health, proxies);

    renderModelTable(overview, models);
    renderProxies(proxies);
    renderProxyAssignments(proxies);
    renderDailyUsage(overview);
    renderProviderPools(health.provider_pools ?? []);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    setText("server-status", "ERROR");
    setText("reasons", message);
  }
}

function renderShell(): void {
  document.body.innerHTML = `
    <main class="shell">
      <section class="hero">
        <div>
          <p class="eyebrow">Leech-RS Dashboard</p>
          <h1>Proxy Operations</h1>
          <p class="sub">A quick view of estimated spend, account pool health, Tor capacity, and request flow.</p>
        </div>
        <div class="status-pill">
          <span>Server</span>
          <strong id="server-status">Loading...</strong>
        </div>
      </section>

      <section class="grid cards">
        <article class="card"><span class="label">Estimated Spend</span><strong id="estimated-spend">$0.0000</strong><small>Total estimated cost across all models</small></article>
        <article class="card"><span class="label">Tor Instances</span><strong id="tor-count">0</strong><small>Active proxies under management</small></article>
        <article class="card"><span class="label">Request Rate</span><strong id="request-rate">0.00 req/min</strong><small>Model endpoint request load</small></article>
        <article class="card"><span class="label">Favorite Model</span><strong id="favorite-model">n/a</strong><small>Most-used model by output tokens</small></article>
      </section>

      <section class="provider-section">
        <div class="section-head"><h2>use.ai</h2><span>Primary account pool</span></div>
        <div class="grid provider-cards">
          <article class="card"><span class="label">Warm Accounts</span><strong id="use-ai-ready">0</strong><small>Ready accounts vs target</small></article>
          <article class="card"><span class="label">Proxy Count</span><strong id="use-ai-proxies">0</strong><small>Active use.ai Tor routes</small></article>
          <article class="card"><span class="label">Mode</span><strong id="use-ai-mode">Tor isolated</strong><small>Uses provider-specific Tor range</small></article>
          <article class="card"><span class="label">Status</span><strong id="use-ai-status">Loading</strong><small>Account pool availability</small></article>
        </div>
      </section>

      <section class="provider-section">
        <div class="section-head"><h2>Sakana</h2><span>Direct egress provider</span></div>
        <div class="grid provider-cards">
          <article class="card"><span class="label">Ready Sessions</span><strong id="sakana-ready">0</strong><small>Cached anonymous sessions</small></article>
          <article class="card"><span class="label">Cooling</span><strong id="sakana-cooling">0</strong><small>Backed-off sessions</small></article>
          <article class="card"><span class="label">Mode</span><strong id="sakana-mode">Direct egress</strong><small>No Tor for Firebase signup</small></article>
          <article class="card"><span class="label">Status</span><strong id="sakana-status">Loading</strong><small>Lazy session pool</small></article>
        </div>
      </section>

      <section class="provider-section">
        <div class="section-head"><h2>Faceb</h2><span>API key pool</span></div>
        <div class="grid provider-cards">
          <article class="card"><span class="label">Ready Keys</span><strong id="faceb-ready">0</strong><small>Buffered keys vs max</small></article>
          <article class="card"><span class="label">Generated</span><strong id="faceb-generated">0</strong><small>Keys created this runtime</small></article>
          <article class="card"><span class="label">Dead Keys</span><strong id="faceb-dead">0</strong><small>Exhausted or rejected keys</small></article>
          <article class="card"><span class="label">Proxy Count</span><strong id="faceb-proxies">0</strong><small>Active Faceb Tor routes</small></article>
        </div>
      </section>

      <section class="provider-section">
        <div class="section-head"><h2>Usage Metrics</h2><span>Spend and activity</span></div>
        <div class="grid provider-cards">
          <article class="card"><span class="label">Sessions</span><strong id="session-count">0</strong><small>Total tracked sessions</small></article>
          <article class="card"><span class="label">Messages</span><strong id="message-count">0</strong><small>Total tracked completions</small></article>
          <article class="card"><span class="label">Tokens</span><strong id="token-total">0</strong><small>Total estimated tokens</small></article>
          <article class="card"><span class="label">Warm Accounts</span><strong><span id="pool-count">0</span> / <span id="pool-target">0</span></strong><small>Legacy use.ai pool view</small></article>
        </div>
      </section>

      <section class="grid panels">
        <article class="panel">
          <div class="panel-head">
            <h2>Spend By Model</h2>
            <span id="streak">0 day(s)</span>
          </div>
          <table>
            <thead>
              <tr><th>Label</th><th>Model</th><th>Input</th><th>Output</th><th>Estimated Spend</th></tr>
            </thead>
            <tbody id="model-table"></tbody>
          </table>
        </article>
        <article class="panel">
          <div class="panel-head">
            <h2>Tor Proxies</h2>
            <span id="peak-hour">n/a</span>
          </div>
          <ul id="proxy-list" class="stack"></ul>
        </article>
        <article class="panel">
          <div class="panel-head">
            <h2>Provider Pools</h2>
            <span>Live credentials</span>
          </div>
          <table>
            <thead>
              <tr><th>Provider</th><th>Ready</th><th>Generated</th><th>Failed</th><th>Dead</th><th>Cooling</th></tr>
            </thead>
            <tbody id="provider-pool-table"></tbody>
          </table>
        </article>
        <article class="panel">
          <div class="panel-head">
            <h2>Provider Proxies</h2>
            <span>Active routing</span>
          </div>
          <table>
            <thead>
              <tr><th>Provider</th><th>Count</th><th>Proxies</th></tr>
            </thead>
            <tbody id="provider-proxy-table"></tbody>
          </table>
        </article>
        <article class="panel">
          <div class="panel-head">
            <h2>Daily Usage</h2>
            <span>Last 7 active days</span>
          </div>
          <ul id="daily-usage" class="stack"></ul>
        </article>
        <article class="panel">
          <div class="panel-head">
            <h2>Health Notes</h2>
            <span>Live status</span>
          </div>
          <p id="reasons" class="notes">Loading...</p>
        </article>
        <article class="panel guide-panel full-width">
          <div class="panel-head">
            <h2>Use Guides</h2>
            <span>Quick starts</span>
          </div>
          <ul class="stack guide-summary-grid">
            <li><strong>Base URL</strong><span>http://host:8000/v1</span></li>
            <li><strong>use.ai</strong><span>Use models like gpt-5-4 or claude-sonnet-4-6.</span></li>
            <li><strong>Sakana</strong><span>Use sakana-namazu, sakana-fugu, or sakana-fugu-ultra. Runs direct egress.</span></li>
            <li><strong>Faceb</strong><span>Use faceb-provider/model IDs like faceb-google/gemini-2.5-flash.</span></li>
            <li><strong>OpenCode config path</strong><span>%USERPROFILE%\\.config\\opencode\\opencode.json</span></li>
            <li><strong>Anthropic endpoint</strong><span>http://127.0.0.1:8000/v1/messages</span></li>
            <li><strong>OpenAI endpoint</strong><span>http://127.0.0.1:8000/v1/chat/completions</span></li>
            <li><strong>Smoke</strong><span>Run .\\smoke.ps1 -SkipProviderSmokes for core-only checks.</span></li>
          </ul>
          <div class="config-guide">
            <div class="panel-head compact">
              <h3>Coding Tools</h3>
              <span>Client setup targets</span>
            </div>
            <table class="tool-guide-table">
              <thead>
                <tr><th>Tool</th><th>Protocol</th><th>Endpoint</th><th>Config</th><th>Note</th></tr>
              </thead>
              <tbody>${renderToolGuideRows()}</tbody>
            </table>
          </div>
          <div class="config-guide">
            <div class="panel-head compact">
              <h3>OpenCode Config</h3>
              <span>All exposed models</span>
            </div>
            <p class="notes">Place this at <code>%USERPROFILE%\\.config\\opencode\\opencode.json</code>. Use <code>http://127.0.0.1:8000/v1</code> when OpenCode runs on the same machine as Leech-RS.</p>
            <pre><code>${escapeHtml(OPENCODE_CONFIG)}</code></pre>
          </div>
        </article>
      </section>
    </main>
  `;
}

function injectStyles(): void {
  const style = document.createElement("style");
  style.textContent = `
    :root {
      color-scheme: dark;
      --bg: #0f1117;
      --paper: #181b24;
      --paper-soft: #202431;
      --ink: #e8edf5;
      --muted: #9aa3b2;
      --accent: #d38b5d;
      --accent-strong: #f0a66d;
      --line: rgba(232, 237, 245, 0.11);
      --shadow: 0 18px 44px rgba(0, 0, 0, 0.34);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: Georgia, "Iowan Old Style", "Palatino Linotype", serif;
      background:
        radial-gradient(circle at 18% -10%, rgba(211,139,93,0.16), transparent 34%),
        radial-gradient(circle at 96% 4%, rgba(82,104,148,0.18), transparent 30%),
        linear-gradient(180deg, #141720 0%, var(--bg) 100%);
      color: var(--ink);
    }
    .shell {
      max-width: 1280px;
      margin: 0 auto;
      padding: 32px 20px 48px;
    }
    .hero, .grid { display: grid; gap: 18px; }
    .hero {
      grid-template-columns: 1.6fr 0.8fr;
      align-items: end;
      margin-bottom: 22px;
    }
    .eyebrow {
      text-transform: uppercase;
      letter-spacing: 0.22em;
      font-size: 0.74rem;
      color: var(--accent);
      margin: 0 0 10px;
    }
    h1 {
      margin: 0;
      font-size: clamp(2.3rem, 5vw, 4.3rem);
      line-height: 0.95;
    }
    .sub {
      margin: 12px 0 0;
      max-width: 58ch;
      color: var(--muted);
      font-size: 1rem;
    }
    .status-pill, .card, .panel {
      background: linear-gradient(180deg, rgba(32,36,49,0.96), rgba(24,27,36,0.96));
      border: 1px solid var(--line);
      border-radius: 20px;
      box-shadow: var(--shadow);
    }
    .status-pill {
      background: linear-gradient(135deg, rgba(45,37,32,0.98), rgba(24,27,36,0.98));
    }
    .status-pill {
      padding: 18px 20px;
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 12px;
    }
    .cards {
      grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
      margin-bottom: 20px;
    }
    .card {
      padding: 18px;
      min-height: 135px;
      display: flex;
      flex-direction: column;
      justify-content: space-between;
    }
    .label {
      color: var(--muted);
      text-transform: uppercase;
      letter-spacing: 0.08em;
      font-size: 0.74rem;
    }
    strong {
      font-size: 1.8rem;
      line-height: 1;
    }
    small {
      color: var(--muted);
      font-size: 0.88rem;
    }
    .provider-section {
      margin-bottom: 20px;
    }
    .section-head {
      display: flex;
      align-items: baseline;
      justify-content: space-between;
      gap: 12px;
      margin: 0 2px 12px;
    }
    .section-head span {
      color: var(--muted);
      font-size: 0.92rem;
    }
    .provider-cards {
      grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    }
    .panels {
      grid-template-columns: 1.5fr 1fr;
      align-items: start;
    }
    .full-width {
      grid-column: 1 / -1;
    }
    .panel {
      padding: 18px;
    }
    .panel-head {
      display: flex;
      justify-content: space-between;
      align-items: baseline;
      gap: 12px;
      margin-bottom: 14px;
    }
    h2 {
      margin: 0;
      font-size: 1.1rem;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 0.94rem;
    }
    code, pre {
      font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace;
    }
    code {
      font-size: 0.86em;
      background: rgba(211,139,93,0.12);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 2px 5px;
      color: #ffd8bd;
    }
    pre {
      max-height: 420px;
      overflow: auto;
      white-space: pre;
      background: #0b0d12;
      color: #e8edf5;
      border: 1px solid rgba(211,139,93,0.2);
      border-radius: 14px;
      padding: 14px;
      font-size: 0.82rem;
      line-height: 1.45;
    }
    pre code {
      background: transparent;
      border: 0;
      color: inherit;
      padding: 0;
      font-size: inherit;
    }
    th, td {
      text-align: left;
      padding: 10px 8px;
      border-bottom: 1px solid var(--line);
      vertical-align: top;
    }
    th {
      color: var(--muted);
      font-weight: 600;
      font-size: 0.8rem;
      text-transform: uppercase;
      letter-spacing: 0.06em;
    }
    .stack {
      list-style: none;
      padding: 0;
      margin: 0;
      display: grid;
      gap: 10px;
    }
    .stack li {
      display: flex;
      justify-content: space-between;
      gap: 12px;
      padding: 10px 12px;
      background: rgba(32,36,49,0.74);
      border: 1px solid var(--line);
      border-radius: 14px;
      font-size: 0.92rem;
    }
    .notes {
      margin: 0;
      color: var(--muted);
      line-height: 1.6;
    }
    .config-guide {
      margin-top: 18px;
      padding-top: 16px;
      border-top: 1px solid var(--line);
    }
    .guide-summary-grid {
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    }
    .guide-summary-grid li {
      align-items: flex-start;
      min-height: 78px;
    }
    .guide-summary-grid li span {
      text-align: right;
      overflow-wrap: anywhere;
    }
    .panel-head.compact {
      margin-bottom: 10px;
    }
    .panel-head h3 {
      margin: 0;
      font-size: 1rem;
    }
    .tool-guide-table {
      min-width: 0;
    }
    .guide-panel {
      overflow: auto;
    }
    @media (max-width: 900px) {
      .hero, .panels { grid-template-columns: 1fr; }
    }
  `;
  document.head.appendChild(style);
}

injectStyles();
renderShell();
loadDashboard();
setInterval(() => { void loadDashboard(); }, 15000);
