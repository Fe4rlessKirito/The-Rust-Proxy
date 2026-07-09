"use strict";
const MODEL_PRICING_PER_MILLION = {
    "gpt-5-5": { input: 4, output: 12 },
    "gpt-5-4": { input: 3, output: 10 },
    "gpt-5-3": { input: 2.5, output: 8 },
    "gpt-5-1": { input: 2, output: 7 },
    "gpt-5": { input: 2, output: 6 },
    "gpt-5-mini": { input: 0.3, output: 1.2 },
    "gpt-4o": { input: 2.5, output: 10 },
    "gpt-4o-mini": { input: 0.15, output: 0.6 },
    "claude-sonnet-5": { input: 3, output: 15 },
    "claude-sonnet-4-6": { input: 3, output: 15 },
    "claude-sonnet-4-5": { input: 3, output: 15 },
    "claude-haiku-4-5": { input: 1, output: 5 },
    "claude-haiku-4": { input: 0.8, output: 4 },
    "sakana-namazu": { input: 0, output: 0 },
    "sakana-fugu": { input: 0, output: 0 },
    "sakana-fugu-ultra": { input: 0, output: 0 },
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
    "faceb-mistralai/ministral-3b-2512": { input: 0.04, output: 0.16 },
    "gemini-3-1-pro": { input: 2, output: 12 },
    "gemini-3-pro": { input: 2, output: 10 },
    "gemini-3-flash": { input: 0.3, output: 2.5 },
    "gemini-2.5-flash": { input: 0.3, output: 2.5 },
    "deepseek-v4-pro": { input: 0.6, output: 3 },
    "deepseek-v4-flash": { input: 0.2, output: 1 },
    "deepseek-r1": { input: 0.55, output: 2.2 },
    "grok-4": { input: 3, output: 15 },
    "glm-5-2": { input: 0.5, output: 2 },
    "qwen-3-max": { input: 0.8, output: 2.4 },
    "qwen-3-5-397b": { input: 0.8, output: 2.4 },
    "kimi-k2-6": { input: 0.6, output: 2.5 },
    "deepinfra-kimi-k2": { input: 0.5, output: 1.5 },
    "llama-3-3-70b-versatile": { input: 0.3, output: 0.9 }
};
const currency = new Intl.NumberFormat("en-US", {
    currency: "USD",
    maximumFractionDigits: 4,
    style: "currency"
});
const DB_NAME = "leech-browser-workspace";
const DB_VERSION = 1;
const FILE_STORE = "files";
const state = {
    files: [],
    isSending: false,
    messages: [],
    model: localStorage.getItem("leech-model") ?? "gpt-5-4",
    models: [],
    selectedFileId: localStorage.getItem("leech-selected-file-id"),
    sessionId: localStorage.getItem("leech-session-id") ?? crypto.randomUUID(),
    view: (localStorage.getItem("leech-view") === "dashboard" ? "dashboard" : "chat")
};
localStorage.setItem("leech-session-id", state.sessionId);
function id(prefix) {
    return `${prefix}-${crypto.randomUUID()}`;
}
function escapeHtml(value) {
    return value
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;");
}
function formatNumber(value) {
    return new Intl.NumberFormat("en-US").format(value);
}
function pricingFor(model) {
    return MODEL_PRICING_PER_MILLION[model] ?? { input: 0.5, output: 2 };
}
function estimateSpend(overview) {
    return Object.entries(overview.models).reduce((sum, [model, outputTokens]) => {
        const pricing = pricingFor(model);
        const inputTokens = overview.model_input_tokens?.[model] ?? 0;
        const billedOutput = overview.model_output_tokens?.[model] ?? outputTokens;
        return sum + (inputTokens / 1000000) * pricing.input + (billedOutput / 1000000) * pricing.output;
    }, 0);
}
function estimateModelSpend(inputTokens, outputTokens, model) {
    const pricing = pricingFor(model);
    return (inputTokens / 1000000) * pricing.input + (outputTokens / 1000000) * pricing.output;
}
async function fetchJson(url) {
    const response = await fetch(url);
    if (!response.ok) {
        throw new Error(`${url} returned ${response.status}`);
    }
    return response.json();
}
function setText(selector, value) {
    const element = document.querySelector(selector);
    if (element) {
        element.textContent = value;
    }
}
function clearElement(element) {
    while (element.firstChild) {
        element.removeChild(element.firstChild);
    }
}
function appendCell(row, value) {
    const cell = document.createElement("td");
    cell.textContent = value;
    row.appendChild(cell);
}
function openWorkspaceDb() {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(DB_NAME, DB_VERSION);
        request.onupgradeneeded = () => {
            const db = request.result;
            if (!db.objectStoreNames.contains(FILE_STORE)) {
                db.createObjectStore(FILE_STORE, { keyPath: "id" });
            }
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error ?? new Error("Failed to open browser workspace"));
    });
}
async function loadWorkspaceFiles() {
    const db = await openWorkspaceDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(FILE_STORE, "readonly");
        const store = tx.objectStore(FILE_STORE);
        const request = store.getAll();
        request.onsuccess = () => {
            resolve(request.result.sort((a, b) => b.updatedAt - a.updatedAt));
            db.close();
        };
        request.onerror = () => {
            reject(request.error ?? new Error("Failed to load files"));
            db.close();
        };
    });
}
async function saveWorkspaceFile(file) {
    const db = await openWorkspaceDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(FILE_STORE, "readwrite");
        tx.objectStore(FILE_STORE).put(file);
        tx.oncomplete = () => {
            db.close();
            resolve();
        };
        tx.onerror = () => {
            const error = tx.error ?? new Error("Failed to save file");
            db.close();
            reject(error);
        };
    });
}
async function deleteWorkspaceFile(id) {
    const db = await openWorkspaceDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(FILE_STORE, "readwrite");
        tx.objectStore(FILE_STORE).delete(id);
        tx.oncomplete = () => {
            db.close();
            resolve();
        };
        tx.onerror = () => {
            const error = tx.error ?? new Error("Failed to delete file");
            db.close();
            reject(error);
        };
    });
}
function selectedFile() {
    return state.files.find((file) => file.id === state.selectedFileId);
}
function providerSummary(pools) {
    return pools
        .map((pool) => {
        const target = pool.target === null ? "" : `/${pool.target}`;
        return `${pool.provider} ${pool.ready}${target}`;
    })
        .join("  ");
}
function renderModelOptions(models) {
    const label = document.querySelector("#model-picker-label");
    const menu = document.querySelector("#model-menu");
    if (!label || !menu)
        return;
    const modelList = models.length ? models : [{ id: state.model, label: state.model }];
    if (!modelList.some((model) => model.id === state.model)) {
        state.model = modelList[0]?.id ?? "gpt-5-4";
        localStorage.setItem("leech-model", state.model);
    }
    const selectedModel = modelList.find((model) => model.id === state.model);
    label.textContent = selectedModel?.label ?? state.model;
    menu.innerHTML = modelList
        .map((model) => {
        const selected = model.id === state.model ? " active" : "";
        return `
        <button class="model-option${selected}" type="button" data-model-id="${escapeHtml(model.id)}">
          ${escapeHtml(model.label ?? model.id)}
        </button>
      `;
    })
        .join("");
}
function renderMessages() {
    const conversation = document.querySelector("#chat-view");
    if (!conversation)
        return;
    if (state.messages.length === 0) {
        conversation.innerHTML = `
      <section class="empty-state">
        <div class="mark">L</div>
        <h1>Leech Chat</h1>
        <div class="suggestions">
          <button class="suggestion" type="button" data-prompt="Give me a quick provider health summary.">Provider health</button>
          <button class="suggestion" type="button" data-prompt="Which model should I use for fast coding tasks?">Pick a model</button>
          <button class="suggestion" type="button" data-prompt="Summarize recent gateway usage.">Usage summary</button>
        </div>
      </section>
    `;
        return;
    }
    conversation.innerHTML = state.messages
        .map((message) => `
      <article class="message ${message.role}" data-message-id="${message.id}">
        <div class="avatar">${message.role === "user" ? "You" : "L"}</div>
        <div class="bubble">${formatMessage(message.content)}</div>
      </article>
    `)
        .join("");
    conversation.scrollTop = conversation.scrollHeight;
}
function renderWorkspace() {
    const list = document.querySelector("#file-list");
    const meta = document.querySelector("#file-meta");
    const file = selectedFile();
    if (list) {
        list.innerHTML = state.files.length
            ? state.files
                .map((item) => `
            <button class="file-item ${item.id === state.selectedFileId ? "active" : ""}" type="button" data-file-id="${escapeHtml(item.id)}">
              <span>${escapeHtml(item.name)}</span>
              <small>${formatNumber(item.content.length)} chars</small>
            </button>
          `)
                .join("")
            : `<p class="empty-files">No browser files yet.</p>`;
    }
    if (meta) {
        meta.textContent = file
            ? `${file.name} stored locally, ${formatNumber(file.content.length)} chars`
            : "Stored only in this browser";
    }
    localStorage.setItem("leech-selected-file-id", state.selectedFileId ?? "");
}
async function refreshWorkspace() {
    state.files = await loadWorkspaceFiles();
    if (state.selectedFileId && !state.files.some((file) => file.id === state.selectedFileId)) {
        state.selectedFileId = null;
    }
    if (!state.selectedFileId && state.files.length > 0) {
        state.selectedFileId = state.files[0].id;
    }
    renderWorkspace();
}
async function importFiles(fileList) {
    if (!fileList?.length)
        return;
    for (const file of Array.from(fileList)) {
        if (!file.type.startsWith("text/") && !/\.(txt|md|json|toml|rs|ts|tsx|js|jsx|css|html|yaml|yml|xml|csv|py|go|java|c|cpp|h|hpp)$/i.test(file.name)) {
            continue;
        }
        const content = await file.text();
        const workspaceFile = {
            content,
            id: id("file"),
            name: file.name,
            updatedAt: Date.now()
        };
        await saveWorkspaceFile(workspaceFile);
        state.selectedFileId = workspaceFile.id;
    }
    await refreshWorkspace();
}
async function createNewFile() {
    const file = {
        content: "",
        id: id("file"),
        name: `untitled-${state.files.length + 1}.txt`,
        updatedAt: Date.now()
    };
    await saveWorkspaceFile(file);
    state.selectedFileId = file.id;
    await refreshWorkspace();
}
async function deleteSelectedFileFromWorkspace() {
    const file = selectedFile();
    if (!file)
        return;
    await deleteWorkspaceFile(file.id);
    state.selectedFileId = null;
    await refreshWorkspace();
}
function latestAssistantReply() {
    return [...state.messages].reverse().find((message) => message.role === "assistant")?.content;
}
function extractEditableContent(reply) {
    const fenced = reply.match(/```(?:[a-zA-Z0-9_+\-.#]+)?\n([\s\S]*?)```/);
    return (fenced?.[1] ?? reply).trim();
}
async function applyLatestReplyToSelectedFile() {
    const file = selectedFile();
    const reply = latestAssistantReply();
    if (!file || !reply)
        return;
    const updated = {
        ...file,
        content: extractEditableContent(reply),
        updatedAt: Date.now()
    };
    await saveWorkspaceFile(updated);
    await refreshWorkspace();
}
function formatMessage(content) {
    const escaped = escapeHtml(content.trim() || "...");
    return escaped
        .split(/\n{2,}/)
        .map((paragraph) => `<p>${paragraph.replace(/\n/g, "<br>")}</p>`)
        .join("");
}
function setSending(isSending) {
    state.isSending = isSending;
    const send = document.querySelector("#send-button");
    const prompt = document.querySelector("#prompt");
    if (send) {
        send.disabled = isSending;
        send.textContent = isSending ? "…" : "↑";
    }
    if (prompt) {
        prompt.disabled = isSending;
    }
}
function requestMessages() {
    const messages = state.messages.map((message) => ({
        content: message.content,
        role: message.role
    }));
    const file = selectedFile();
    if (!file) {
        return messages;
    }
    const fileContext = [
        "Browser-local workspace file context.",
        `File: ${file.name}`,
        "When asked to edit this file, return the full replacement content in one fenced code block.",
        "",
        "Current file contents:",
        "```",
        file.content.slice(0, 80000),
        "```"
    ].join("\n");
    return [
        {
            role: "user",
            content: fileContext
        },
        ...messages
    ];
}
async function sendMessage(content) {
    if (state.isSending)
        return;
    const text = content.trim();
    if (!text)
        return;
    state.messages.push({ content: text, id: id("user"), role: "user" });
    const assistantId = id("assistant");
    renderMessages();
    setSending(true);
    try {
        const response = await fetch("/v1/chat/completions", {
            body: JSON.stringify({
                messages: requestMessages(),
                model: state.model,
                stream: false,
                user: state.sessionId
            }),
            headers: { "content-type": "application/json" },
            method: "POST"
        });
        const payload = await response.json();
        if (!response.ok || payload.error) {
            throw new Error(payload.error ?? `Request failed with ${response.status}`);
        }
        const reply = payload.choices?.[0]?.message?.content;
        const toolCalls = payload.choices?.[0]?.message?.tool_calls;
        state.messages.push({
            content: reply ?? (toolCalls ? JSON.stringify(toolCalls, null, 2) : "No response content."),
            id: assistantId,
            role: "assistant"
        });
    }
    catch (error) {
        state.messages.push({
            content: error instanceof Error ? error.message : String(error),
            id: assistantId,
            role: "assistant"
        });
    }
    finally {
        setSending(false);
        renderMessages();
        void refreshStatus();
    }
}
function resizeComposer() {
    const prompt = document.querySelector("#prompt");
    if (!prompt)
        return;
    prompt.style.height = "auto";
    prompt.style.height = `${Math.min(prompt.scrollHeight, 220)}px`;
}
async function refreshStatus() {
    try {
        const [health, bank, proxies, usage, models] = await Promise.all([
            fetchJson("/health"),
            fetchJson("/bank"),
            fetchJson("/proxies"),
            fetchJson("/usage/overview"),
            fetchJson("/v1/models")
        ]);
        state.models = models.data;
        renderModelOptions(models.data);
        setText("#status-pill", health.status.toUpperCase());
        setText("#health-line", providerSummary(health.provider_pools ?? []) || "No provider stats yet");
        setText("#favorite-model", usage.favorite_model ?? "n/a");
        renderProviderList(health.provider_pools ?? [], proxies.provider_assignments ?? {}, proxies.provider_configured_routes ?? {});
        renderDashboard(health, bank, proxies, usage, models);
    }
    catch (error) {
        setText("#status-pill", "OFFLINE");
        setText("#health-line", error instanceof Error ? error.message : String(error));
    }
}
function renderProviderList(pools, assignments, configuredRoutes) {
    const list = document.querySelector("#provider-list");
    if (!list)
        return;
    if (!pools.length) {
        list.innerHTML = `<li><span>No providers</span><strong>0</strong></li>`;
        return;
    }
    list.innerHTML = pools
        .map((pool) => {
        const target = pool.target === null ? "" : ` / ${pool.target}`;
        const activeRoutes = assignments[pool.provider]?.length ?? 0;
        const configured = configuredRoutes[pool.provider] ?? activeRoutes;
        const routeText = configured > activeRoutes
            ? `${configured} configured routes, ${activeRoutes} active`
            : `${activeRoutes} active proxy routes`;
        return `
        <li>
          <span>${escapeHtml(pool.provider)}</span>
          <strong>${pool.ready}${target}</strong>
          <small>${routeText}</small>
        </li>
      `;
    })
        .join("");
}
function renderModelTable(overview, models) {
    const table = document.querySelector("#model-table");
    if (!table)
        return;
    const labels = new Map(models.data.map((model) => [model.id, model.label]));
    const rows = Object.entries(overview.models)
        .sort((a, b) => b[1] - a[1])
        .map(([model, tokens]) => {
        const inputTokens = overview.model_input_tokens?.[model] ?? 0;
        const outputTokens = overview.model_output_tokens?.[model] ?? tokens;
        const spend = estimateModelSpend(inputTokens, outputTokens, model);
        return `
        <tr>
          <td>${escapeHtml(labels.get(model) ?? model)}</td>
          <td>${escapeHtml(model)}</td>
          <td>${formatNumber(inputTokens)}</td>
          <td>${formatNumber(outputTokens)}</td>
          <td>${currency.format(spend)}</td>
        </tr>
      `;
    })
        .join("");
    table.innerHTML = rows || `<tr><td colspan="5">No model usage yet.</td></tr>`;
}
function renderProxies(proxies) {
    const list = document.querySelector("#proxy-list");
    if (!list)
        return;
    clearElement(list);
    const items = proxies.proxies.length ? proxies.proxies : ["No proxies currently active."];
    for (const proxy of items) {
        const item = document.createElement("li");
        item.textContent = proxy;
        list.appendChild(item);
    }
}
function renderProxyAssignments(proxies) {
    const table = document.querySelector("#provider-proxy-table");
    if (!table)
        return;
    clearElement(table);
    const entries = Object.entries(proxies.provider_assignments ?? {});
    if (!entries.length) {
        const row = document.createElement("tr");
        const cell = document.createElement("td");
        cell.colSpan = 4;
        cell.textContent = "No provider proxy assignments yet.";
        row.appendChild(cell);
        table.appendChild(row);
        return;
    }
    for (const [provider, providerProxies] of entries) {
        const configured = proxies.provider_configured_routes?.[provider] ?? providerProxies.length;
        const row = document.createElement("tr");
        appendCell(row, provider);
        appendCell(row, String(configured));
        appendCell(row, String(providerProxies.length));
        appendCell(row, providerProxies.join(", ") || "direct");
        table.appendChild(row);
    }
}
function renderDailyUsage(overview) {
    const list = document.querySelector("#daily-usage");
    if (!list)
        return;
    const entries = Object.entries(overview.daily)
        .sort(([a], [b]) => a.localeCompare(b))
        .slice(-7);
    list.innerHTML = entries.length
        ? entries
            .map(([day, tokens]) => `<li><strong>${escapeHtml(day)}</strong><span>${formatNumber(tokens)} tokens</span></li>`)
            .join("")
        : "<li>No daily usage yet.</li>";
}
function renderProviderPools(pools) {
    const table = document.querySelector("#provider-pool-table");
    if (!table)
        return;
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
function providerPool(pools, provider) {
    return pools.find((pool) => pool.provider === provider);
}
function providerProxyCount(proxies, provider) {
    return (proxies.provider_assignments?.[provider] ?? []).length;
}
function setProviderCardValues(health, proxies) {
    const useAi = providerPool(health.provider_pools ?? [], "use_ai");
    const sakana = providerPool(health.provider_pools ?? [], "sakana");
    const faceb = providerPool(health.provider_pools ?? [], "faceb");
    setText("#use-ai-ready", `${useAi?.ready ?? 0}${useAi?.target === null || useAi?.target === undefined ? "" : ` / ${useAi.target}`}`);
    setText("#use-ai-proxies", `${providerProxyCount(proxies, "use_ai")} / ${proxies.provider_configured_routes?.use_ai ?? providerProxyCount(proxies, "use_ai")}`);
    setText("#use-ai-mode", "Tor isolated");
    setText("#use-ai-status", useAi && useAi.ready > 0 ? "Ready" : "Warming");
    setText("#sakana-ready", String(sakana?.ready ?? 0));
    setText("#sakana-cooling", String(sakana?.cooling ?? 0));
    setText("#sakana-mode", "Direct egress");
    setText("#sakana-status", (sakana?.ready ?? 0) > 0 ? "Session cached" : "Lazy");
    setText("#faceb-ready", `${faceb?.ready ?? 0}${faceb?.target === null || faceb?.target === undefined ? "" : ` / ${faceb.target}`}`);
    setText("#faceb-generated", String(faceb?.generated ?? 0));
    setText("#faceb-dead", String(faceb?.dead ?? 0));
    setText("#faceb-proxies", `${providerProxyCount(proxies, "faceb")} / ${proxies.provider_configured_routes?.faceb ?? providerProxyCount(proxies, "faceb")}`);
}
function renderDashboard(health, bank, proxies, overview, models) {
    const totalSpend = estimateSpend(overview);
    setText("#server-status", health.status.toUpperCase());
    setText("#pool-count", String(bank.warm_accounts));
    setText("#pool-target", String(bank.pool_target));
    setText("#tor-count", String(proxies.proxy_count));
    setText("#request-rate", `${proxies.load.requests_per_minute.toFixed(2)} req/min`);
    setText("#session-count", String(overview.sessions));
    setText("#message-count", String(overview.messages));
    setText("#token-total", formatNumber(overview.total_tokens));
    setText("#estimated-spend", currency.format(totalSpend));
    setText("#dashboard-favorite-model", overview.favorite_model ?? "n/a");
    setText("#streak", `${overview.current_streak} day(s)`);
    setText("#peak-hour", overview.peak_hour ?? "n/a");
    setText("#reasons", health.reasons.join(", "));
    setProviderCardValues(health, proxies);
    renderModelTable(overview, models);
    renderProxies(proxies);
    renderProxyAssignments(proxies);
    renderDailyUsage(overview);
    renderProviderPools(health.provider_pools ?? []);
}
function applyView() {
    const chatView = document.querySelector("#chat-view");
    const dashboardView = document.querySelector("#dashboard-view");
    const composer = document.querySelector("#chat-form");
    const toggle = document.querySelector("#view-toggle");
    const label = document.querySelector("#workspace-label");
    const dashboardActive = state.view === "dashboard";
    chatView?.toggleAttribute("hidden", dashboardActive);
    dashboardView?.toggleAttribute("hidden", !dashboardActive);
    composer?.toggleAttribute("hidden", dashboardActive);
    if (toggle)
        toggle.textContent = dashboardActive ? "Back to chat" : "Open dashboard";
    if (label)
        label.textContent = dashboardActive ? "Operations dashboard" : "Gateway chat";
    localStorage.setItem("leech-view", state.view);
}
function newChat() {
    state.messages = [];
    state.sessionId = crypto.randomUUID();
    localStorage.setItem("leech-session-id", state.sessionId);
    state.view = "chat";
    applyView();
    renderMessages();
}
function attachEvents() {
    const form = document.querySelector("#chat-form");
    const prompt = document.querySelector("#prompt");
    const modelPicker = document.querySelector("#model-picker");
    const modelMenu = document.querySelector("#model-menu");
    const newChatButton = document.querySelector("#new-chat");
    const refreshButton = document.querySelector("#refresh-status");
    const viewToggle = document.querySelector("#view-toggle");
    const fileInput = document.querySelector("#file-input");
    const attachFileButton = document.querySelector("#attach-file");
    const newFileButton = document.querySelector("#new-file");
    const deleteFileButton = document.querySelector("#delete-file");
    const applyReplyButton = document.querySelector("#apply-reply");
    form?.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!prompt)
            return;
        const value = prompt.value;
        prompt.value = "";
        resizeComposer();
        void sendMessage(value);
    });
    prompt?.addEventListener("input", resizeComposer);
    prompt?.addEventListener("keydown", (event) => {
        if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            form?.requestSubmit();
        }
    });
    modelPicker?.addEventListener("click", (event) => {
        event.stopPropagation();
        if (!modelMenu)
            return;
        const isOpen = modelMenu.hidden;
        modelMenu.hidden = !isOpen;
        modelPicker.setAttribute("aria-expanded", String(isOpen));
    });
    newChatButton?.addEventListener("click", newChat);
    refreshButton?.addEventListener("click", () => { void refreshStatus(); });
    viewToggle?.addEventListener("click", () => {
        state.view = state.view === "dashboard" ? "chat" : "dashboard";
        applyView();
    });
    attachFileButton?.addEventListener("click", () => fileInput?.click());
    fileInput?.addEventListener("change", () => {
        void importFiles(fileInput.files).finally(() => {
            fileInput.value = "";
        });
    });
    newFileButton?.addEventListener("click", () => { void createNewFile(); });
    deleteFileButton?.addEventListener("click", () => { void deleteSelectedFileFromWorkspace(); });
    applyReplyButton?.addEventListener("click", () => { void applyLatestReplyToSelectedFile(); });
    document.body.addEventListener("click", (event) => {
        const modelButton = event.target.closest("[data-model-id]");
        if (modelButton) {
            state.model = modelButton.dataset.modelId ?? state.model;
            localStorage.setItem("leech-model", state.model);
            renderModelOptions(state.models);
            if (modelMenu && modelPicker) {
                modelMenu.hidden = true;
                modelPicker.setAttribute("aria-expanded", "false");
            }
            return;
        }
        if (!event.target.closest(".model-picker-wrap") && modelMenu && modelPicker) {
            modelMenu.hidden = true;
            modelPicker.setAttribute("aria-expanded", "false");
        }
        const fileButton = event.target.closest("[data-file-id]");
        if (fileButton) {
            state.selectedFileId = fileButton.dataset.fileId ?? null;
            renderWorkspace();
            return;
        }
        const button = event.target.closest("[data-prompt]");
        if (!button || !prompt)
            return;
        prompt.value = button.dataset.prompt ?? "";
        resizeComposer();
        prompt.focus();
    });
    document.body.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && modelMenu && modelPicker) {
            modelMenu.hidden = true;
            modelPicker.setAttribute("aria-expanded", "false");
        }
    });
}
function renderShell() {
    document.body.innerHTML = `
    <div class="app">
      <aside class="rail">
        <div class="brand">
          <div>
            <strong>Leech-RS</strong>
            <span id="status-pill">Loading</span>
          </div>
        </div>

        <button id="new-chat" class="primary-action" type="button">New chat</button>
        <button id="view-toggle" class="secondary-action" type="button">Open dashboard</button>

        <section class="rail-section workspace-section">
          <div class="rail-heading">
            <span>Files</span>
            <button id="new-file" type="button">New</button>
          </div>
          <div id="file-list" class="file-list"></div>
          <span id="file-meta" class="file-meta">Stored only in this browser</span>
          <div class="file-actions">
            <button id="apply-reply" type="button">Apply reply</button>
            <button id="delete-file" type="button">Delete</button>
          </div>
        </section>

        <section class="rail-section">
          <div class="rail-heading">
            <span>Providers</span>
            <button id="refresh-status" type="button">Refresh</button>
          </div>
          <ul id="provider-list" class="provider-list"></ul>
        </section>
      </aside>

      <main class="workspace">
        <header class="topbar">
          <div>
            <span id="workspace-label" class="thread-label">Gateway chat</span>
            <strong id="favorite-model">n/a</strong>
          </div>
          <p id="health-line">Loading provider status</p>
        </header>

        <section id="chat-view" class="conversation"></section>
        <section id="dashboard-view" class="dashboard-view" hidden>
          <section class="dashboard-hero">
            <div>
              <span class="thread-label">Leech-RS Dashboard</span>
              <h1>Proxy Operations</h1>
            </div>
            <div class="status-pill">
              <span>Server</span>
              <strong id="server-status">Loading...</strong>
            </div>
          </section>

          <section class="dashboard-grid metric-grid">
            <article class="dashboard-card"><span>Estimated Spend</span><strong id="estimated-spend">$0.0000</strong><small>Total estimated cost</small></article>
            <article class="dashboard-card"><span>Tor Instances</span><strong id="tor-count">0</strong><small>Active proxies</small></article>
            <article class="dashboard-card"><span>Request Rate</span><strong id="request-rate">0.00 req/min</strong><small>Model endpoint load</small></article>
            <article class="dashboard-card"><span>Favorite Model</span><strong id="dashboard-favorite-model">n/a</strong><small>Most-used model</small></article>
          </section>

          <section class="provider-section">
            <div class="section-head"><h2>use.ai</h2><span>Primary account pool</span></div>
            <div class="dashboard-grid provider-cards">
              <article class="dashboard-card"><span>Warm Accounts</span><strong id="use-ai-ready">0</strong><small>Ready accounts vs target</small></article>
              <article class="dashboard-card"><span>Proxy Routes</span><strong id="use-ai-proxies">0</strong><small>Active / configured</small></article>
              <article class="dashboard-card"><span>Mode</span><strong id="use-ai-mode">Tor isolated</strong><small>Provider-specific Tor range</small></article>
              <article class="dashboard-card"><span>Status</span><strong id="use-ai-status">Loading</strong><small>Pool availability</small></article>
            </div>
          </section>

          <section class="provider-section">
            <div class="section-head"><h2>Sakana</h2><span>Direct egress provider</span></div>
            <div class="dashboard-grid provider-cards">
              <article class="dashboard-card"><span>Ready Sessions</span><strong id="sakana-ready">0</strong><small>Cached sessions</small></article>
              <article class="dashboard-card"><span>Cooling</span><strong id="sakana-cooling">0</strong><small>Backed-off sessions</small></article>
              <article class="dashboard-card"><span>Mode</span><strong id="sakana-mode">Direct egress</strong><small>No Tor routing</small></article>
              <article class="dashboard-card"><span>Status</span><strong id="sakana-status">Loading</strong><small>Lazy session pool</small></article>
            </div>
          </section>

          <section class="provider-section">
            <div class="section-head"><h2>Faceb</h2><span>API key pool</span></div>
            <div class="dashboard-grid provider-cards">
              <article class="dashboard-card"><span>Ready Keys</span><strong id="faceb-ready">0</strong><small>Buffered keys vs max</small></article>
              <article class="dashboard-card"><span>Generated</span><strong id="faceb-generated">0</strong><small>Keys created this runtime</small></article>
              <article class="dashboard-card"><span>Dead Keys</span><strong id="faceb-dead">0</strong><small>Rejected keys</small></article>
              <article class="dashboard-card"><span>Proxy Routes</span><strong id="faceb-proxies">0</strong><small>Active / configured</small></article>
            </div>
          </section>

          <section class="provider-section">
            <div class="section-head"><h2>Usage Metrics</h2><span>Spend and activity</span></div>
            <div class="dashboard-grid provider-cards">
              <article class="dashboard-card"><span>Sessions</span><strong id="session-count">0</strong><small>Total tracked sessions</small></article>
              <article class="dashboard-card"><span>Messages</span><strong id="message-count">0</strong><small>Total tracked completions</small></article>
              <article class="dashboard-card"><span>Tokens</span><strong id="token-total">0</strong><small>Total estimated tokens</small></article>
              <article class="dashboard-card"><span>Warm Accounts</span><strong><span id="pool-count">0</span> / <span id="pool-target">0</span></strong><small>Legacy use.ai pool</small></article>
            </div>
          </section>

          <section class="dashboard-grid panels">
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Spend By Model</h2><span id="streak">0 day(s)</span></div>
              <div class="table-wrap">
                <table>
                  <thead><tr><th>Label</th><th>Model</th><th>Input</th><th>Output</th><th>Spend</th></tr></thead>
                  <tbody id="model-table"></tbody>
                </table>
              </div>
            </article>
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Tor Proxies</h2><span id="peak-hour">n/a</span></div>
              <ul id="proxy-list" class="stack"></ul>
            </article>
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Provider Pools</h2><span>Live credentials</span></div>
              <div class="table-wrap">
                <table>
                  <thead><tr><th>Provider</th><th>Ready</th><th>Generated</th><th>Failed</th><th>Dead</th><th>Cooling</th></tr></thead>
                  <tbody id="provider-pool-table"></tbody>
                </table>
              </div>
            </article>
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Provider Proxies</h2><span>Route assignments</span></div>
              <div class="table-wrap">
                <table>
                  <thead><tr><th>Provider</th><th>Configured</th><th>Active</th><th>Proxies</th></tr></thead>
                  <tbody id="provider-proxy-table"></tbody>
                </table>
              </div>
            </article>
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Daily Usage</h2><span>Last 7 active days</span></div>
              <ul id="daily-usage" class="stack"></ul>
            </article>
            <article class="dashboard-panel">
              <div class="panel-head"><h2>Health Notes</h2><span>Live status</span></div>
              <p id="reasons" class="notes">Loading...</p>
            </article>
          </section>
        </section>

        <form id="chat-form" class="composer">
          <textarea id="prompt" rows="1" placeholder="Message Leech-RS"></textarea>
          <input id="file-input" type="file" multiple hidden />
          <div class="composer-actions">
            <div class="composer-left">
              <button id="attach-file" class="icon-button" type="button" title="Attach files">+</button>
              <div class="model-picker-wrap">
                <button id="model-picker" class="model-picker-button" type="button" aria-haspopup="listbox" aria-expanded="false">
                  <span id="model-picker-label">${escapeHtml(state.model)}</span>
                  <span class="model-caret">v</span>
                </button>
                <div id="model-menu" class="model-menu" role="listbox" hidden></div>
              </div>
            </div>
            <button id="send-button" class="send-icon" type="submit" title="Send">↑</button>
          </div>
        </form>
      </main>
    </div>
  `;
}
function injectStyles() {
    const style = document.createElement("style");
    style.textContent = `
    :root {
      color-scheme: dark;
      --bg: #11100e;
      --surface: #1a1815;
      --surface-alt: #24211d;
      --ink: #f3eee6;
      --muted: #a99f92;
      --line: rgba(243, 238, 230, 0.11);
      --line-strong: rgba(243, 238, 230, 0.19);
      --accent: #d86f50;
      --accent-dark: #f08a66;
      --accent-soft: #32201b;
      --accent-line: rgba(216, 111, 80, 0.42);
      --green: #96b9a3;
      --shadow: 0 18px 42px rgba(0, 0, 0, 0.34);
    }

    * {
      box-sizing: border-box;
    }

    body {
      margin: 0;
      background:
        radial-gradient(circle at 70% -20%, rgba(216, 111, 80, 0.12), transparent 34%),
        linear-gradient(180deg, #171511 0%, var(--bg) 42%);
      color: var(--ink);
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      letter-spacing: 0;
    }

    button,
    select,
    textarea {
      font: inherit;
      letter-spacing: 0;
    }

    button {
      cursor: pointer;
    }

    [hidden] {
      display: none !important;
    }

    .app {
      display: grid;
      grid-template-columns: 292px minmax(0, 1fr);
      height: 100vh;
      overflow: hidden;
    }

    .rail {
      display: flex;
      flex-direction: column;
      gap: 16px;
      height: 100vh;
      overflow-y: auto;
      padding: 18px;
      border-right: 1px solid var(--line);
      background: #15130f;
    }

    .brand {
      display: flex;
      align-items: center;
      min-height: 36px;
    }

    .avatar,
    .mark {
      display: grid;
      place-items: center;
      border: 1px solid var(--line-strong);
      background: var(--surface-alt);
      color: var(--green);
      font-weight: 700;
    }

    .brand strong {
      display: block;
      font-size: 1rem;
      line-height: 1.1;
    }

    .brand span {
      color: var(--muted);
      display: block;
      font-size: 0.82rem;
      margin-top: 3px;
    }

    .primary-action,
    .secondary-action {
      border: 0;
      border-radius: 8px;
      background: var(--accent);
      color: #fff;
      font-weight: 700;
      min-height: 42px;
      padding: 0 16px;
    }

    .primary-action:hover {
      background: var(--accent-dark);
    }

    .secondary-action {
      border: 1px solid var(--line-strong);
      background: var(--surface-alt);
      color: var(--ink);
    }

    .secondary-action:hover {
      border-color: var(--accent);
      background: var(--accent-soft);
      color: var(--accent-dark);
    }

    #send-button:disabled {
      cursor: wait;
      opacity: 0.62;
    }

    .rail-section {
      border: 1px solid var(--line);
      border-radius: 8px;
      background: var(--surface);
      padding: 12px;
    }

    .rail-section label,
    .rail-section span,
    .thread-label,
    .provider-list small {
      color: var(--muted);
      font-size: 0.82rem;
    }

    .rail-heading {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px;
      margin-bottom: 10px;
    }

    .rail-heading button,
    .suggestion,
    .file-actions button {
      border: 1px solid var(--line-strong);
      border-radius: 8px;
      background: var(--surface-alt);
      color: var(--ink);
      min-height: 32px;
      padding: 0 10px;
    }

    .rail-heading button:hover,
    .suggestion:hover,
    .file-actions button:hover {
      border-color: var(--accent);
      color: var(--accent-dark);
    }

    .workspace-section {
      display: grid;
      gap: 10px;
    }

    .file-list {
      display: grid;
      gap: 6px;
      max-height: 180px;
      overflow: auto;
    }

    .file-item {
      align-items: center;
      border: 1px solid var(--line);
      border-radius: 8px;
      background: var(--surface-alt);
      color: var(--ink);
      display: grid;
      gap: 3px;
      min-height: 42px;
      padding: 8px 10px;
      text-align: left;
    }

    .file-item.active {
      border-color: var(--accent);
      background: var(--accent-soft);
    }

    .file-item span {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .file-item small,
    .empty-files,
    .file-meta {
      color: var(--muted);
      font-size: 0.78rem;
      margin: 0;
    }

    .file-actions {
      display: grid;
      gap: 6px;
      grid-template-columns: 1fr 1fr;
    }

    .provider-list {
      display: grid;
      gap: 8px;
      list-style: none;
      margin: 0;
      padding: 0;
    }

    .provider-list li {
      display: grid;
      grid-template-columns: 1fr auto;
      gap: 2px 10px;
      padding: 9px;
      border-radius: 8px;
      background: var(--surface-alt);
    }

    .provider-list small {
      grid-column: 1 / -1;
    }

    .workspace {
      display: grid;
      grid-template-rows: auto 1fr auto;
      min-width: 0;
      height: 100vh;
      overflow: hidden;
    }

    .topbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 18px;
      min-height: 70px;
      padding: 14px 28px;
      border-bottom: 1px solid var(--line);
      background: rgba(17, 16, 14, 0.88);
      backdrop-filter: blur(12px);
    }

    .topbar strong {
      display: block;
      margin-top: 3px;
    }

    .topbar p {
      color: var(--muted);
      margin: 0;
      max-width: 54ch;
      overflow: hidden;
      text-align: right;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .conversation {
      overflow: auto;
      padding: 32px 28px 24px;
    }

    .dashboard-view {
      overflow: auto;
      padding: 28px;
    }

    .dashboard-hero {
      align-items: end;
      display: grid;
      gap: 18px;
      grid-template-columns: minmax(0, 1fr) minmax(210px, 320px);
      margin-bottom: 18px;
    }

    .dashboard-hero h1 {
      font-family: Georgia, "Times New Roman", serif;
      font-size: clamp(2rem, 4vw, 3.7rem);
      font-weight: 500;
      line-height: 1;
      margin: 6px 0 0;
    }

    .status-pill,
    .dashboard-card,
    .dashboard-panel {
      background: linear-gradient(180deg, rgba(36, 33, 29, 0.96), rgba(26, 24, 21, 0.96));
      border: 1px solid var(--line);
      border-radius: 8px;
      box-shadow: var(--shadow);
    }

    .status-pill {
      align-items: center;
      display: flex;
      gap: 14px;
      justify-content: space-between;
      min-height: 78px;
      padding: 16px;
    }

    .status-pill span,
    .dashboard-card span,
    .section-head span,
    .panel-head span,
    .notes,
    .stack span,
    small {
      color: var(--muted);
    }

    .status-pill strong,
    .dashboard-card strong {
      font-size: 1.5rem;
      line-height: 1.1;
    }

    .dashboard-grid {
      display: grid;
      gap: 14px;
    }

    .metric-grid,
    .provider-cards {
      grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    }

    .dashboard-card {
      display: flex;
      flex-direction: column;
      gap: 12px;
      justify-content: space-between;
      min-height: 126px;
      padding: 16px;
    }

    .provider-section {
      margin-top: 20px;
    }

    .section-head,
    .panel-head {
      align-items: baseline;
      display: flex;
      gap: 12px;
      justify-content: space-between;
      margin-bottom: 10px;
    }

    .section-head h2,
    .panel-head h2 {
      font-size: 1rem;
      margin: 0;
    }

    .panels {
      align-items: start;
      grid-template-columns: minmax(0, 1.5fr) minmax(280px, 1fr);
      margin-top: 20px;
    }

    .dashboard-panel {
      min-width: 0;
      padding: 16px;
    }

    .table-wrap {
      overflow: auto;
    }

    table {
      border-collapse: collapse;
      font-size: 0.88rem;
      width: 100%;
    }

    th,
    td {
      border-bottom: 1px solid var(--line);
      padding: 10px 8px;
      text-align: left;
      vertical-align: top;
    }

    th {
      color: var(--muted);
      font-size: 0.74rem;
      font-weight: 700;
      text-transform: uppercase;
    }

    .stack {
      display: grid;
      gap: 8px;
      list-style: none;
      margin: 0;
      padding: 0;
    }

    .stack li {
      align-items: center;
      background: var(--surface-alt);
      border: 1px solid var(--line);
      border-radius: 8px;
      display: flex;
      gap: 12px;
      justify-content: space-between;
      padding: 10px 12px;
    }

    .notes {
      line-height: 1.55;
      margin: 0;
    }

    .empty-state {
      align-content: center;
      display: grid;
      gap: 22px;
      justify-items: center;
      min-height: 58vh;
      text-align: center;
    }

    .mark {
      width: 54px;
      height: 54px;
      border-radius: 8px;
      box-shadow: var(--shadow);
      font-size: 1.35rem;
    }

    .empty-state h1 {
      font-family: Georgia, "Times New Roman", serif;
      font-size: 2.65rem;
      font-weight: 500;
      line-height: 1;
      margin: 0;
    }

    .suggestions {
      display: flex;
      flex-wrap: wrap;
      justify-content: center;
      gap: 10px;
      max-width: 760px;
    }

    .message {
      display: grid;
      grid-template-columns: 46px minmax(0, 760px);
      gap: 14px;
      margin: 0 auto 22px;
      max-width: 900px;
    }

    .message.user {
      grid-template-columns: minmax(0, 760px) 46px;
      justify-content: end;
    }

    .message.user .avatar {
      grid-column: 2;
      grid-row: 1;
    }

    .message.user .bubble {
      grid-column: 1;
      grid-row: 1;
      background: var(--accent-soft);
      border-color: var(--accent-line);
    }

    .avatar {
      width: 40px;
      height: 40px;
      border-radius: 8px;
      font-size: 0.78rem;
    }

    .bubble {
      border: 1px solid var(--line);
      border-radius: 8px;
      background: var(--surface);
      box-shadow: var(--shadow);
      line-height: 1.62;
      padding: 16px 18px;
      white-space: normal;
      overflow-wrap: anywhere;
    }

    .bubble p {
      margin: 0 0 12px;
    }

    .bubble p:last-child {
      margin-bottom: 0;
    }

    .composer {
      display: grid;
      gap: 10px;
      width: min(900px, calc(100% - 56px));
      margin: 0 auto 24px;
      border: 1px solid rgba(243, 238, 230, 0.08);
      border-radius: 20px;
      background: #292826;
      box-shadow: 0 18px 44px rgba(0, 0, 0, 0.28);
      padding: 10px 12px;
    }

    textarea {
      width: 100%;
      min-height: 46px;
      max-height: 220px;
      resize: none;
      border: 0;
      outline: 0;
      background: transparent;
      color: var(--ink);
      line-height: 1.5;
      padding: 6px;
    }

    textarea::placeholder {
      color: #7f7568;
    }

    .composer-actions {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      min-height: 42px;
    }

    .composer-left {
      display: flex;
      align-items: center;
      gap: 10px;
      min-width: 0;
    }

    .icon-button,
    .send-icon {
      align-items: center;
      border: 0;
      border-radius: 999px;
      display: inline-flex;
      justify-content: center;
      width: 30px;
      height: 30px;
      padding: 0;
    }

    .icon-button {
      background: transparent;
      color: var(--muted);
      font-size: 1.35rem;
      line-height: 1;
    }

    .icon-button:hover {
      color: var(--ink);
      background: rgba(243, 238, 230, 0.08);
    }

    .send-icon {
      background: #f4f0e9;
      color: #15130f;
      font-size: 1.2rem;
      font-weight: 800;
    }

    .send-icon:hover {
      background: #fffaf2;
    }

    .model-picker-wrap {
      position: relative;
      min-width: 0;
    }

    .model-picker-button {
      align-items: center;
      background: transparent;
      border: 0;
      border-radius: 999px;
      color: var(--ink);
      display: inline-flex;
      gap: 8px;
      min-height: 30px;
      max-width: min(260px, calc(100vw - 120px));
      padding: 0 10px;
    }

    .model-picker-button:hover,
    .model-picker-button[aria-expanded="true"] {
      background: rgba(243, 238, 230, 0.08);
    }

    #model-picker-label {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .model-caret {
      color: var(--muted);
      font-size: 0.75rem;
      line-height: 1;
    }

    .model-menu {
      background: #1d1b18;
      border: 1px solid rgba(243, 238, 230, 0.14);
      border-radius: 10px;
      bottom: 38px;
      box-shadow: 0 18px 48px rgba(0, 0, 0, 0.42);
      display: grid;
      gap: 2px;
      left: 0;
      max-height: 330px;
      overflow-y: auto;
      padding: 6px;
      position: absolute;
      width: min(320px, calc(100vw - 80px));
      z-index: 30;
    }

    .model-menu[hidden] {
      display: none;
    }

    .model-option {
      background: transparent;
      border: 0;
      border-radius: 7px;
      color: #f3eee6;
      min-height: 34px;
      overflow: hidden;
      padding: 7px 10px;
      text-align: left;
      text-overflow: ellipsis;
      white-space: nowrap;
      width: 100%;
    }

    .model-option:hover,
    .model-option:focus {
      background: rgba(243, 238, 230, 0.1);
      outline: none;
    }

    .model-option.active {
      background: #f4f0e9;
      color: #15130f;
      font-weight: 700;
    }

    @media (max-width: 920px) {
      .app {
        grid-template-columns: 1fr;
        height: auto;
        min-height: 100vh;
        overflow: visible;
      }

      .rail {
        min-height: auto;
        height: auto;
        max-height: 42vh;
        overflow-y: auto;
        border-right: 0;
        border-bottom: 1px solid var(--line);
      }

      .workspace {
        min-height: calc(100vh - 360px);
        height: auto;
      }

      .topbar {
        align-items: flex-start;
        flex-direction: column;
      }

      .topbar p {
        max-width: 100%;
        text-align: left;
      }

      .dashboard-view {
        padding: 20px 14px;
      }

      .dashboard-hero,
      .panels {
        grid-template-columns: 1fr;
      }

      .conversation {
        padding: 26px 16px 20px;
      }

      .message,
      .message.user {
        grid-template-columns: 38px minmax(0, 1fr);
      }

      .message.user .avatar {
        grid-column: 1;
      }

      .message.user .bubble {
        grid-column: 2;
      }

      .avatar {
        width: 34px;
        height: 34px;
      }

      .composer {
        width: calc(100% - 28px);
        margin-bottom: 14px;
      }
    }
  `;
    document.head.appendChild(style);
}
injectStyles();
renderShell();
attachEvents();
applyView();
renderMessages();
void refreshWorkspace();
void refreshStatus();
setInterval(() => { void refreshStatus(); }, 15000);
