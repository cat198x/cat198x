// Thin frontend for the Cat198x UI. It calls the same operations the CLI and MCP
// server use, through Tauri commands (window.__TAURI__ is injected because
// app.withGlobalTauri is enabled). No logic lives here beyond rendering.

const invoke = window.__TAURI__.core.invoke;

// Cap how many operations we draw — a real plan can hold tens of thousands, and
// the diff is for review, not a full dump. The summary always reflects the whole.
const MAX_OPS = 500;

function fmtBytes(n) {
  const u = ["bytes", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = Number(n) || 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${v} ${u[i]}` : `${v.toFixed(2)} ${u[i]}`;
}

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else if (k === "style") node.setAttribute("style", v);
    else if (k.startsWith("data-")) node.setAttribute(k, v);
    else node[k] = v;
  }
  for (const c of children) {
    if (c == null) continue;
    node.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return node;
}

function showError(container, e) {
  container.className = "";
  container.replaceChildren(el("div", { class: "error" }, String(e)));
}

// ---- Status view ----
// Concurrency for the per-collection stat fill — enough to overlap the slow
// collections (e.g. MAME) with the fast ones, without flooding the backend.
const STATUS_POOL = 8;

// Run `worker` over `items` with at most `size` in flight at once.
async function runPool(items, size, worker) {
  let i = 0;
  const next = async () => {
    while (i < items.length) {
      const idx = i++;
      await worker(items[idx], idx);
    }
  };
  await Promise.all(Array.from({ length: Math.min(size, items.length) }, next));
}

async function loadStatus() {
  const body = document.getElementById("status-body");
  body.className = "loading";
  body.textContent = "Loading collections…";
  let cols;
  try {
    cols = await invoke("collections");
  } catch (e) {
    showError(body, e);
    return;
  }
  if (!cols || cols.length === 0) {
    body.className = "";
    body.replaceChildren(el("div", { class: "empty" }, "No collections imported yet."));
    return;
  }

  // Render the list immediately: a row per collection with placeholder cells.
  // The numbers fill in as each per-collection query resolves, so a slow
  // collection holds up only its own row, never the list.
  const head = el(
    "tr",
    {},
    el("th", {}, "Collection"),
    el("th", {}, "Version"),
    el("th", { class: "num" }, "Games"),
    el("th", { class: "num" }, "Have"),
    el("th", { class: "num" }, "Missing"),
    el("th", {}, "Complete")
  );
  const tbody = el("tbody");
  const rowFor = new Map();
  for (const c of cols) {
    const cells = {
      version: el("td", { class: "muted" }, c.has_active_version ? "…" : "no active version"),
      games: el("td", { class: "num muted" }, c.has_active_version ? "…" : ""),
      have: el("td", { class: "num muted" }, ""),
      missing: el("td", { class: "num muted" }, ""),
      complete: el("td", { class: "muted" }, ""),
    };
    rowFor.set(c.name, cells);
    tbody.append(
      el("tr", {}, el("td", {}, c.name), cells.version, cells.games, cells.have, cells.missing, cells.complete)
    );
  }
  body.className = "";
  body.replaceChildren(el("table", {}, el("thead", {}, head), tbody));

  // Fill stats for collections with an active version, concurrently.
  const active = cols.filter((c) => c.has_active_version);
  await runPool(active, STATUS_POOL, async (c) => {
    const cells = rowFor.get(c.name);
    try {
      const s = await invoke("status_one", { name: c.name });
      if (!s || !s.version) {
        cells.version.textContent = "no active version";
        cells.games.textContent = "";
        return;
      }
      fillStatusRow(cells, s);
    } catch (e) {
      cells.version.textContent = "error";
      cells.version.title = String(e);
    }
  });
}

// Fill one collection's row from its computed status.
function fillStatusRow(cells, s) {
  cells.version.className = "muted";
  cells.version.textContent = `v${s.version}`;
  cells.games.className = "num";
  cells.games.textContent = s.total_games.toLocaleString();
  cells.have.className = "num";
  cells.have.textContent = s.have_roms.toLocaleString();
  cells.missing.className = "num";
  cells.missing.textContent = s.missing_roms.toLocaleString();

  const pct = (s.completion_pct || 0).toFixed(1);
  const complete = s.missing_roms === 0 && s.total_roms > 0;
  const bar = el(
    "div",
    { class: complete ? "bar complete" : "bar", style: `--pct:${Math.min(100, s.completion_pct || 0)}%` },
    el("span", {})
  );
  cells.complete.className = "";
  cells.complete.replaceChildren(
    el("div", { style: "display:flex;align-items:center;gap:8px" }, bar, `${pct}%`)
  );
}

// ---- Plan (diff) view ----
async function loadPlan() {
  const body = document.getElementById("plan-body");
  body.className = "loading";
  body.textContent = "Loading plan…";
  try {
    const plan = await invoke("plan_diff");
    renderPlan(body, plan);
  } catch (e) {
    showError(body, e);
  }
}

function chip(n, k) {
  return el("div", { class: "chip" }, el("div", { class: "n" }, n.toLocaleString()), el("div", { class: "k" }, k));
}

function opPaths(kind) {
  switch (kind.type) {
    case "copy":
    case "move":
      return el("div", { class: "paths" }, kind.source.path, el("span", { class: "to" }, "  →  " + kind.dest));
    case "relocate":
      return el("div", { class: "paths" }, kind.source, el("span", { class: "to" }, "  →  " + kind.dest));
    case "repack":
      return el(
        "div",
        { class: "paths" },
        `${kind.sources.length} source(s)`,
        el("span", { class: "to" }, `  →  ${kind.dest} [${kind.format}]`)
      );
    case "delete":
      return el("div", { class: "paths" }, kind.path);
    case "quarantine":
      return el("div", { class: "paths" }, kind.path, el("span", { class: "to" }, "  →  quarantine"));
    default:
      return el("div", { class: "paths" }, JSON.stringify(kind));
  }
}

function renderPlan(body, plan) {
  body.className = "";
  if (!plan) {
    body.replaceChildren(
      el("div", { class: "empty" }, "No plan saved yet. Generate one with: cat198x plan")
    );
    return;
  }
  const s = plan.summary;
  const summary = el(
    "div",
    { class: "summary" },
    chip(s.copy_count, "copy"),
    chip(s.move_count, "move"),
    chip(s.repack_count, "repack"),
    chip(s.delete_count, "delete"),
    chip(s.quarantine_count || 0, "quarantine"),
    chip(s.already_correct, "already correct"),
    el("div", { class: "chip" }, el("div", { class: "n" }, fmtBytes(s.total_bytes)), el("div", { class: "k" }, "to transfer"))
  );

  const total = plan.total_operations;
  const meta = el(
    "div",
    { class: "muted", style: "margin-bottom:12px" },
    `${total.toLocaleString()} operation(s) · generated ${plan.created_at} · state ${plan.state_hash.slice(0, 12)}`
  );

  const list = el("div", {});
  const shown = plan.operations.slice(0, MAX_OPS);
  for (const op of shown) {
    list.append(
      el(
        "div",
        { class: "op" },
        el("div", { class: "kind " + op.kind.type }, op.kind.type),
        opPaths(op.kind)
      )
    );
  }
  if (total > shown.length) {
    list.append(
      el("div", { class: "muted", style: "padding:10px" }, `… and ${(total - shown.length).toLocaleString()} more (showing first ${shown.length.toLocaleString()})`)
    );
  }

  body.replaceChildren(summary, meta, list);
}

// ---- Tabs + refresh ----
let currentView = "plan";
let statusLoaded = false;

function switchView(view) {
  currentView = view;
  for (const t of document.querySelectorAll(".tab")) {
    t.classList.toggle("active", t.dataset.view === view);
  }
  document.getElementById("status-view").classList.toggle("active", view === "status");
  document.getElementById("plan-view").classList.toggle("active", view === "plan");

  // Status completeness can be expensive over a large catalogue, so it loads
  // lazily — only the first time its tab is opened. The plan loads up front
  // because it is the central view and only reads the saved plan file.
  if (view === "status" && !statusLoaded) {
    statusLoaded = true;
    loadStatus();
  }
}

function refresh() {
  if (currentView === "status") {
    statusLoaded = true;
    loadStatus();
  } else {
    loadPlan();
  }
}

window.addEventListener("DOMContentLoaded", () => {
  for (const t of document.querySelectorAll(".tab")) {
    t.addEventListener("click", () => switchView(t.dataset.view));
  }
  document.getElementById("refresh").addEventListener("click", refresh);
  // Default to the plan (diff) view; it is fast and is the central view.
  switchView("plan");
  loadPlan();
});
