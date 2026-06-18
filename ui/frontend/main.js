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

// A completion bar + percentage.
function completeCell(have, total) {
  const pct = total > 0 ? (have / total) * 100 : 0;
  const done = total > 0 && have >= total;
  const bar = el(
    "div",
    { class: done ? "bar complete" : "bar", style: `--pct:${Math.min(100, pct)}%` },
    el("span", {})
  );
  return el("div", { class: "complete-cell" }, bar, `${pct.toFixed(1)}%`);
}

// --- Tree model ---
// The catalogue's collections form a tree by their library path
// (TOSEC/Acorn/Archimedes/Games/[ADF]). Each node rolls up the completeness of
// every collection beneath it; leaves are collections.
function makeNode(name, parent) {
  return {
    name,
    parent: parent || null,
    children: new Map(),
    collection: null, // set on a leaf
    activeLeaves: 0, // active collections in this subtree (for the progress hint)
    agg: { games: 0, have: 0, total: 0, resolved: 0 },
    dom: null, // { row, cells, childrenEl, expanded, rendered } once drawn
  };
}

function buildTree(cols) {
  const root = makeNode("", null);
  for (const c of cols) {
    const segs = (c.node_path || c.name).split("/").filter((s) => s.length);
    let node = root;
    for (const seg of segs) {
      if (!node.children.has(seg)) node.children.set(seg, makeNode(seg, node));
      node = node.children.get(seg);
    }
    node.collection = c; // this path is a collection's leaf
  }
  // Count active leaves per subtree, so a node knows when its rollup is complete.
  const countActive = (node) => {
    let n = node.collection && node.collection.has_active_version ? 1 : 0;
    for (const ch of node.children.values()) n += countActive(ch);
    node.activeLeaves = n;
    return n;
  };
  countActive(root);
  return root;
}

const sortedChildren = (node) =>
  [...node.children.values()].sort((a, b) => a.name.localeCompare(b.name));

// Render a node's children as rows into `container`, recursing lazily: a node's
// own children are only drawn the first time it is expanded.
function renderChildren(node, container, depth) {
  for (const child of sortedChildren(node)) {
    const hasChildren = child.children.size > 0;
    const cells = {
      games: el("div", { class: "num muted" }, ""),
      have: el("div", { class: "num muted" }, ""),
      missing: el("div", { class: "num muted" }, ""),
      complete: el("div", {}, ""),
    };
    const toggle = el("span", { class: "tree-toggle" }, hasChildren ? "▸" : "");
    const count = hasChildren
      ? el("span", { class: "pill" }, `${child.activeLeaves || child.children.size}`)
      : null;
    const name = el(
      "div",
      { class: "name", style: `padding-left:${depth * 18}px`, title: child.name },
      toggle,
      el("span", { class: hasChildren ? "node-name group" : "node-name" }, child.name),
      count
    );
    const row = el(
      "div",
      { class: "tree-row" + (hasChildren ? " clickable" : "") },
      name,
      cells.games,
      cells.have,
      cells.missing,
      cells.complete
    );
    const childrenEl = el("div", { class: "tree-children hidden" });
    child.dom = { row, cells, childrenEl, expanded: false, rendered: false };
    drawNodeStats(child);
    container.append(row, childrenEl);

    if (hasChildren) {
      row.addEventListener("click", () => {
        child.dom.expanded = !child.dom.expanded;
        toggle.textContent = child.dom.expanded ? "▾" : "▸";
        childrenEl.classList.toggle("hidden", !child.dom.expanded);
        if (child.dom.expanded && !child.dom.rendered) {
          child.dom.rendered = true;
          renderChildren(child, childrenEl, depth + 1);
        }
      });
    }
  }
}

// Draw a node's current rolled-up numbers into its (already-rendered) row.
function drawNodeStats(node) {
  if (!node.dom) return;
  const { cells, row } = node.dom;
  const a = node.agg;
  const leaf = node.collection && node.children.size === 0;

  // Numbers: only meaningful once something has resolved.
  if (a.resolved > 0 || leaf) {
    cells.games.className = "num";
    cells.games.textContent = a.games.toLocaleString();
    cells.have.className = "num";
    cells.have.textContent = a.have.toLocaleString();
    cells.missing.className = "num";
    cells.missing.textContent = (a.total - a.have).toLocaleString();
  }

  if (node.activeLeaves === 0) {
    row.classList.add("inactive");
    cells.complete.replaceChildren(el("span", { class: "muted" }, "—"));
  } else if (a.resolved >= node.activeLeaves) {
    cells.complete.replaceChildren(completeCell(a.have, a.total));
  } else {
    cells.complete.replaceChildren(
      el("span", { class: "muted" }, `${a.resolved}/${node.activeLeaves} computed…`)
    );
  }
}

// Add a resolved collection's numbers to its leaf node and every ancestor,
// redrawing whichever of those rows are currently on screen.
function rollUpToRoot(leafNode, s) {
  for (let n = leafNode; n; n = n.parent) {
    n.agg.games += s.total_games;
    n.agg.have += s.have_roms;
    n.agg.total += s.total_roms;
    n.agg.resolved += 1;
    if (n.dom) drawNodeStats(n);
  }
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

  const root = buildTree(cols);
  const leafByName = new Map();
  const collectLeaves = (node) => {
    if (node.collection) leafByName.set(node.collection.name, node);
    for (const ch of node.children.values()) collectLeaves(ch);
  };
  collectLeaves(root);

  const header = el(
    "div",
    { class: "tree-row tree-head" },
    el("div", { class: "name" }, "Set / Manufacturer / System / …"),
    el("div", { class: "num" }, "Games"),
    el("div", { class: "num" }, "Have"),
    el("div", { class: "num" }, "Missing"),
    el("div", {}, "Complete")
  );
  const treeBody = el("div", { class: "tree-body" });
  renderChildren(root, treeBody, 0); // top-level sets, collapsed

  body.className = "";
  body.replaceChildren(el("div", { class: "tree" }, header, treeBody));

  // Fill each active collection concurrently, rolling each result up the tree.
  const active = cols.filter((c) => c.has_active_version);
  await runPool(active, STATUS_POOL, async (c) => {
    try {
      const s = await invoke("status_one", { name: c.name });
      const leaf = leafByName.get(c.name);
      if (s && s.version && leaf) rollUpToRoot(leaf, s);
    } catch (e) {
      // Leave the node showing its pending hint; surface the error on hover.
      const leaf = leafByName.get(c.name);
      if (leaf && leaf.dom) leaf.dom.row.title = String(e);
    }
  });
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
