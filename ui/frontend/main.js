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

// The tail of a path — the last few segments — so a long absolute path stays
// legible on one progress line. Short paths are returned whole.
function shortPath(p) {
  if (!p) return "";
  const segs = p.split("/").filter(Boolean);
  return segs.length <= 3 ? p : "…/" + segs.slice(-3).join("/");
}

// Shorten the kept-copy path inside a dedup reason ("exact duplicate — kept …")
// so its filename tail stays visible where a line would otherwise ellipsis it.
function fmtReason(reason) {
  if (!reason) return "";
  const i = reason.indexOf("kept ");
  return i < 0 ? reason : reason.slice(0, i + 5) + shortPath(reason.slice(i + 5));
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
    pending: { to_write: 0, bytes: 0 }, // reorganise work from the saved plan
    dom: null, // { row, cells, badge, childrenEl, expanded, rendered } once drawn
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
    const badge = el("span", { class: "pending-badge hidden" });
    badge.addEventListener("click", (ev) => {
      ev.stopPropagation(); // don't toggle the row
      showPendingDetail(child);
    });
    const name = el(
      "div",
      { class: "name", style: `padding-left:${depth * 18}px`, title: child.name },
      toggle,
      el("span", { class: hasChildren ? "node-name group" : "node-name" }, child.name),
      count,
      badge
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
    child.dom = { row, cells, badge, childrenEl, expanded: false, rendered: false };
    drawNodeStats(child);
    drawBadge(child);
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

// Draw (or clear) a node's pending-work badge — the reorganise work the saved
// plan would do within it. Clicking the badge opens the per-node detail.
function drawBadge(node) {
  if (!node.dom) return;
  const b = node.dom.badge;
  const p = node.pending;
  if (p.to_write > 0) {
    b.textContent = `⊕ ${p.to_write.toLocaleString()} to file · ${fmtBytes(p.bytes)}`;
    b.classList.remove("hidden");
    b.title = "Pending reorganise work — click for detail";
  } else {
    b.classList.add("hidden");
  }
}

// Add one collection's pending work to its leaf and every ancestor, redrawing
// any badges currently on screen.
function applyPending(leafNode, toWrite, bytes) {
  for (let n = leafNode; n; n = n.parent) {
    n.pending.to_write += toWrite;
    n.pending.bytes += bytes;
    if (n.dom) drawBadge(n);
  }
}

// Collect the leaf collections beneath `node` that have pending work.
function pendingLeaves(node, out = []) {
  if (node.collection && node.pending.to_write > 0 && node.children.size === 0) {
    out.push(node);
  }
  for (const ch of node.children.values()) pendingLeaves(ch, out);
  return out;
}

// Show a node's pending reorganise work, broken down by collection.
function showPendingDetail(node) {
  const leaves = pendingLeaves(node).sort((a, b) => b.pending.to_write - a.pending.to_write);
  const rows = leaves.map((l) =>
    el(
      "div",
      { class: "detail-row" },
      el("div", { class: "detail-name", title: l.collection.node_path }, l.collection.name),
      el("div", { class: "num" }, `${l.pending.to_write.toLocaleString()} to file`),
      el("div", { class: "num muted" }, fmtBytes(l.pending.bytes))
    )
  );
  const panel = el(
    "div",
    { class: "detail-panel" },
    el(
      "div",
      { class: "detail-head" },
      el("strong", {}, node.name || "Library"),
      el(
        "span",
        { class: "muted" },
        ` · ${node.pending.to_write.toLocaleString()} to file · ${fmtBytes(node.pending.bytes)}`
      ),
      el("button", { class: "detail-close", onclick: () => overlay.remove() }, "✕")
    ),
    el("div", { class: "detail-list" }, ...(rows.length ? rows : [el("div", { class: "muted" }, "No pending work.")]))
  );
  const overlay = el("div", { class: "detail-overlay" }, panel);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  document.body.append(overlay);
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

  const banner = el("div", { class: "stale-banner hidden" });
  body.className = "";
  body.replaceChildren(el("div", { class: "tree" }, banner, header, treeBody));

  // Overlay the saved plan's pending reorganise work as per-node badges. This is
  // a cheap read of the saved plan — independent of the completeness fill below.
  invoke("pending_work")
    .then((pw) => {
      if (!pw) return;
      if (pw.stale) {
        banner.textContent =
          "⚠ The pending counts are from an earlier plan and may be out of date — run `cat198x plan` to refresh.";
        banner.classList.remove("hidden");
      }
      for (const item of pw.items) {
        const leaf = leafByName.get(item.collection);
        if (leaf) applyPending(leaf, item.to_write, item.bytes);
      }
    })
    .catch(() => {
      /* pending overlay is best-effort; the tree still works without it */
    });

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
      return el(
        "div",
        { class: "paths" },
        kind.path,
        kind.reason ? el("span", { class: "to" }, "  —  " + fmtReason(kind.reason)) : null
      );
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

// ---- Apply preview + confirm-gated apply ----
let applyUnlisten = null;

// Run an apply command — the dry-run preview or the real execute — with a live
// progress bar fed by the engine's `apply-progress` events, and return the final
// report. The two commands never run at once, so they share the one channel; the
// caller decides how to render success, and an error propagates to its catch.
async function streamApply(body, command, startLabel) {
  // A counts line, then one row per worker slot (filled lazily once the first
  // event reveals how many), then the bar. Each slot shows the file that worker
  // is currently on; serial ops (delete/quarantine) show on a shared line.
  const counts = el("div", { class: "muted" }, startLabel);
  const slotsEl = el("div", { class: "slots" });
  const serial = el("div", { class: "prog-detail" }, "");
  const fill = el("span", {});
  const bar = el("div", { class: "progress" }, fill);
  // A scrolling log of each completed/failed/refused operation, newest at the
  // bottom, capped so a long run can't grow it without bound.
  const logEl = el("div", { class: "apply-log" });
  body.replaceChildren(
    el("div", { class: "prog-wrap" }, counts, slotsEl, serial, bar, logEl)
  );

  let slotRows = null; // one element per worker, built on the first event
  const MAX_LOG = 200;

  const ensureSlots = (jobs) => {
    if (slotRows || !jobs) return;
    slotRows = Array.from({ length: jobs }, () => el("div", { class: "slot idle" }, "idle"));
    slotsEl.replaceChildren(...slotRows);
  };
  const opText = (verb, from, to, bytes) => {
    const size = bytes ? `  ·  ${fmtBytes(bytes)}` : "";
    return to ? `${verb} ${shortPath(from)} → ${shortPath(to)}${size}` : `${verb} ${shortPath(from)}${size}`;
  };
  const ICON = { ok: "✓", failed: "✗", refused: "⚠" };
  // A failure/refusal shows its detail; an otherwise-safe op (a dedup delete)
  // shows its reason — what survives it — so a mass delete reads as reassuring.
  const appendLog = (outcome, verb, from, to, detail, reason) => {
    const atBottom = logEl.scrollTop + logEl.clientHeight >= logEl.scrollHeight - 4;
    const note = detail || (reason ? fmtReason(reason) : "");
    const text = `${ICON[outcome] || "·"} ${opText(verb, from, to, 0)}` + (note ? `  —  ${note}` : "");
    logEl.append(el("div", { class: `log-line ${outcome}` }, text));
    while (logEl.childElementCount > MAX_LOG) logEl.firstElementChild.remove();
    if (atBottom) logEl.scrollTop = logEl.scrollHeight; // follow the tail
  };

  const stop = () => {
    if (applyUnlisten) {
      applyUnlisten();
      applyUnlisten = null;
    }
  };

  stop(); // drop any listener from a previous run
  applyUnlisten = await window.__TAURI__.event.listen("apply-progress", (e) => {
    const { done, total, jobs, slot, finished, verb, from, to, bytes, bytes_done, bytes_total, outcome, detail, reason } =
      e.payload;
    ensureSlots(jobs);

    const pct = total ? Math.round((done / total) * 100) : 0;
    fill.style.width = pct + "%";
    counts.textContent =
      `${done.toLocaleString()} / ${total.toLocaleString()} (${pct}%)` +
      ` · ${fmtBytes(bytes_done)} of ${fmtBytes(bytes_total)} processed`;

    if (slot != null && slotRows && slotRows[slot]) {
      const row = slotRows[slot];
      if (finished) {
        row.className = "slot idle";
        row.textContent = "idle";
      } else {
        row.className = "slot busy";
        row.textContent = opText(verb, from, to, bytes);
      }
    } else if (slot == null && verb && !outcome) {
      // A serial op (delete/quarantine/repack) — show it on the shared line.
      serial.textContent = opText(verb, from, to, bytes);
    }

    // A terminal result (ok/failed/refused) also goes to the log.
    if (outcome) appendLog(outcome, verb, from, to, detail, reason);
  });
  try {
    return await invoke(command);
  } finally {
    stop();
  }
}

async function loadPreview() {
  const body = document.getElementById("preview-body");
  body.className = "";
  try {
    const report = await streamApply(body, "apply_stream", "Starting dry run…");
    renderPreview(body, report);
  } catch (e) {
    showError(body, e);
  }
}

function renderPreview(body, r) {
  body.className = "";
  if (!r) {
    body.replaceChildren(
      el("div", { class: "empty" }, "No plan saved yet. Generate one with: cat198x plan")
    );
    return;
  }

  // A plan with some operations already done is mid-flight: re-applying resumes
  // it (the library allows a started plan through the staleness gate), so its
  // own catalogue drift is expected, not a reason to regenerate.
  const started = r.pending < r.total_ops;

  const parts = [];
  // Only warn "regenerate" when staleness would actually block — a fresh plan.
  // A started+stale plan resumes instead, so it gets a resume note, not a block.
  if (r.stale && !started) {
    parts.push(
      el(
        "div",
        { class: "stale-banner" },
        "⚠ The plan predates the current catalogue — regenerate with `cat198x plan` before applying."
      )
    );
  } else if (started) {
    parts.push(
      el(
        "div",
        { class: "stale-banner" },
        `↻ A previous apply was interrupted — ${r.pending.toLocaleString()} operation(s) remain. Apply to resume.`
      )
    );
  }
  parts.push(
    r.disk_ok
      ? el("div", { class: "disk-ok" }, "✓ Fits on the destination volume.")
      : el(
          "div",
          { class: "error" },
          "Insufficient disk space — " + (r.disk_detail || "the destination volume is too full.")
        )
  );

  // Op tally, from the apply engine's own progress events.
  const order = ["COPY", "MOVE", "RELOCATE", "REPACK", "DELETE", "QUARANTINE"];
  const summary = el("div", { class: "summary" });
  for (const k of order) {
    const n = r.by_kind[k];
    if (n) summary.append(chip(n, k.toLowerCase()));
  }
  if (!summary.children.length) {
    summary.append(el("div", { class: "muted" }, "Nothing to do — the library is already in place."));
  }
  parts.push(summary);

  parts.push(
    el(
      "div",
      { class: "muted", style: "margin-top:6px" },
      `${r.total_ops.toLocaleString()} operation(s), ${r.pending.toLocaleString()} pending · dry run — nothing was changed`
    )
  );

  // Offer the real apply only when there is pending work that would actually run:
  // it must fit on disk, and either be a fresh non-stale plan or a started one to
  // resume. A fresh+stale plan is blocked at the library gate, so don't present a
  // button that could only be refused.
  if (r.pending > 0 && r.disk_ok && (!r.stale || started)) {
    parts.push(applyGate(body, r, started));
  }

  body.replaceChildren(...parts);
}

// The confirm gate: an "Apply…" button whose click reveals an explicit
// confirmation. Nothing mutates until that confirmation is clicked — the click
// is the authorisation. Cancel returns to the preview, having done nothing.
function applyGate(body, r, started) {
  const wrap = el("div", { class: "apply-gate" });

  const verb = started ? "Resume — apply" : "Apply";
  const ask = el(
    "button",
    { class: "apply-btn", type: "button" },
    `${verb} ${r.pending.toLocaleString()} operation(s)…`
  );

  ask.addEventListener("click", () => {
    const msg = el(
      "div",
      { class: "confirm-msg" },
      `Apply ${r.pending.toLocaleString()} operation(s), moving ~${fmtBytes(r.total_bytes)}? ` +
        "Files are moved and staging freed — reversible only with `cat198x apply --rollback`."
    );
    const go = el("button", { class: "apply-btn danger", type: "button" }, "Confirm apply");
    const cancel = el("button", { class: "ghost-btn", type: "button" }, "Cancel");
    cancel.addEventListener("click", () => loadPreview());
    go.addEventListener("click", () => runExecute(body));
    wrap.replaceChildren(msg, el("div", { class: "confirm-row" }, go, cancel));
  });

  wrap.replaceChildren(ask);
  return wrap;
}

// Drive the real apply and render its outcome. Reused by the confirm gate and by
// the "Apply again to resume" button, which re-runs the same mutating command —
// a started plan resumes its still-pending operations.
async function runExecute(body) {
  try {
    const report = await streamApply(body, "apply_execute", "Applying…");
    renderExecuteResult(body, report);
  } catch (e) {
    showError(body, e);
  }
}

// The outcome of a real apply. A flaky network mount can drop mid-run, so some
// operations may have failed; apply is resumable (per-op status, journaled), so
// surface "N done, M remaining — Apply again to resume" rather than treat a
// partial run as a dead end.
function renderExecuteResult(body, r) {
  body.className = "";
  if (!r) {
    body.replaceChildren(el("div", { class: "empty" }, "No plan to apply."));
    return;
  }
  // The library refused at a gate (stale / won't fit): nothing was touched.
  if (r.refused) {
    body.replaceChildren(el("div", { class: "stale-banner" }, "⚠ Apply refused: " + r.refused));
    return;
  }

  const parts = [];
  const refused = r.refused_ops || 0;
  const clean = r.failed === 0 && refused === 0;
  parts.push(
    el(
      "div",
      { class: clean ? "disk-ok" : "error" },
      `${r.succeeded.toLocaleString()} operation(s) applied` +
        (r.failed ? `, ${r.failed.toLocaleString()} failed` : "") +
        (refused ? `, ${refused.toLocaleString()} refused (safety)` : "") +
        "."
    )
  );

  // Retryable failures (e.g. a dropped mount) resume by re-running; refusals are
  // sticky (the safety net declined them) and need a fresh plan, not a retry.
  if (r.failed > 0) {
    parts.push(
      el(
        "div",
        { class: "muted", style: "margin-top:6px" },
        `${r.succeeded.toLocaleString()} done, ${r.failed.toLocaleString()} remaining — the mount may have dropped. Apply again to resume the rest.`
      )
    );
    const resume = el("button", { class: "apply-btn", type: "button" }, "Apply again to resume");
    resume.addEventListener("click", () => runExecute(body));
    parts.push(resume);
  } else if (refused > 0) {
    parts.push(
      el(
        "div",
        { class: "muted", style: "margin-top:6px" },
        `${refused.toLocaleString()} operation(s) were refused by the safety net and won't be retried. Regenerate the plan if the catalogue has changed.`
      )
    );
  } else {
    parts.push(
      el("div", { class: "muted", style: "margin-top:6px" }, "The library is now in place.")
    );
  }
  body.replaceChildren(...parts);
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
  document.getElementById("preview-view").classList.toggle("active", view === "preview");

  // Status completeness can be expensive over a large catalogue, so it loads
  // lazily — only the first time its tab is opened. The plan loads up front
  // because it is the central view and only reads the saved plan file. The
  // preview runs a dry-run apply, so it too loads on first open.
  if (view === "status" && !statusLoaded) {
    statusLoaded = true;
    loadStatus();
  } else if (view === "preview") {
    loadPreview();
  }
}

function refresh() {
  if (currentView === "status") {
    statusLoaded = true;
    loadStatus();
  } else if (currentView === "preview") {
    loadPreview();
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
