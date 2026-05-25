//! HTML renderer for `qp html`. Emits a single self-contained dark-themed
//! dashboard with inlined CSS, JSON data blocks, an SVG DAG, and ~400 LoC
//! of vanilla JS for filters + click interactions. Zero external requests.

use serde_json::{json, Value};
use crate::cmd::html_layout::{Layout, LaidNode};

pub struct TaskData {
    pub id: i64,
    pub display_id: String,
    pub title: String,
    pub state: String,
    pub agent: Option<String>,
    pub tags: Vec<String>,
}

pub struct EventData {
    pub task: Option<String>,
    pub ts: String,
    pub kind: String,
    pub agent: Option<String>,
    pub payload: Value,
}

pub struct RenderInput<'a> {
    pub project: &'a str,
    pub generated_at: &'a str,
    pub state_counts: &'a [(String, i64)],
    pub tasks: &'a [TaskData],
    pub events: &'a [EventData],
    pub layout: &'a Layout,
    pub refresh: u32,
    pub wave: Option<&'a str>,
    pub svg_too_large: bool,
    pub task_count_for_dag: usize,
}

pub fn render(input: &RenderInput) -> String {
    let tasks_json = build_tasks_json(input.tasks);
    let events_json = build_events_json(input.events);
    let svg = if input.svg_too_large { String::new() } else { build_svg(input.layout) };

    let mut o = String::with_capacity(64 * 1024);
    o.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    o.push_str("<meta charset=\"UTF-8\">\n");
    o.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    if input.refresh > 0 {
        o.push_str(&format!("<meta http-equiv=\"refresh\" content=\"{}\">\n", input.refresh));
    }
    o.push_str(&format!("<title>quipu board — {}</title>\n", html_esc(input.project)));
    o.push_str("<style>\n");
    o.push_str(CSS);
    o.push_str("</style>\n</head>\n<body>\n");

    // header
    o.push_str("<header class=\"hdr\">\n");
    o.push_str(&format!("<div class=\"hdr-l\"><span class=\"proj\">{}</span>", html_esc(input.project)));
    if let Some(w) = input.wave {
        o.push_str(&format!(" <span class=\"scope\">wave {}</span>", html_esc(w)));
    }
    o.push_str("</div>\n");
    o.push_str("<div class=\"hdr-c\">");
    for (st, c) in input.state_counts {
        o.push_str(&format!(
            "<span class=\"sc sc-{}\"><span class=\"sc-l\">{}</span> <span class=\"sc-v\">{}</span></span>",
            html_esc(st), html_esc(st), c));
    }
    o.push_str("</div>\n");
    o.push_str(&format!("<div class=\"hdr-r mono\">{}</div>\n", html_esc(input.generated_at)));
    o.push_str("</header>\n");

    // main grid
    o.push_str("<main class=\"grid\">\n");

    // left: filters + task table
    o.push_str("<section class=\"panel pnl-tasks\">\n");
    o.push_str("<div class=\"filters\">\n");
    o.push_str("<div class=\"chip-row\" id=\"state-chips\">\n");
    for st in ["pending","ready","assigned","running","done","cancelled"] {
        o.push_str(&format!(
            "<button class=\"chip chip-state\" data-state=\"{}\">{}</button>",
            st, st));
    }
    o.push_str("</div>\n");
    o.push_str("<div class=\"text-row\">\n");
    o.push_str("<input type=\"text\" id=\"f-tag\" placeholder=\"tag prefix (e.g. kind:)\" />\n");
    o.push_str("<input type=\"text\" id=\"f-agent\" placeholder=\"agent prefix\" />\n");
    o.push_str("<input type=\"text\" id=\"f-title\" placeholder=\"title substring\" />\n");
    o.push_str("<button id=\"f-clear\" type=\"button\">clear</button>\n");
    o.push_str("</div>\n");
    o.push_str("<div class=\"facets\" id=\"tag-facets\"></div>\n");
    o.push_str("</div>\n");
    o.push_str("<div class=\"task-wrap\">\n");
    o.push_str("<table class=\"tasks\" id=\"task-table\">\n");
    o.push_str("<thead><tr><th>id</th><th>state</th><th>agent</th><th>tags</th><th>title</th></tr></thead>\n");
    o.push_str("<tbody id=\"task-body\"></tbody>\n");
    o.push_str("</table>\n");
    o.push_str("</div>\n");
    o.push_str("</section>\n");

    // right: DAG
    o.push_str("<section class=\"panel pnl-dag\">\n");
    o.push_str("<h2>dep DAG</h2>\n");
    if input.svg_too_large {
        o.push_str(&format!(
            "<div class=\"too-big\">graph too large ({} tasks); use <code>--wave QP-N</code> to scope.</div>\n",
            input.task_count_for_dag));
    } else if input.layout.nodes.is_empty() {
        o.push_str("<div class=\"empty\">no tasks</div>\n");
    } else {
        o.push_str(&svg);
    }
    o.push_str("</section>\n");

    // bottom: timeline
    o.push_str("<section class=\"panel pnl-timeline\">\n");
    o.push_str("<h2>timeline (last 200)</h2>\n");
    o.push_str("<div class=\"tl-wrap\"><ul id=\"timeline\" class=\"tl\"></ul></div>\n");
    o.push_str("</section>\n");
    o.push_str("</main>\n");

    // data blocks
    o.push_str("<script id=\"qp-tasks\" type=\"application/json\">");
    o.push_str(&escape_json_in_script(&tasks_json));
    o.push_str("</script>\n");
    o.push_str("<script id=\"qp-events\" type=\"application/json\">");
    o.push_str(&escape_json_in_script(&events_json));
    o.push_str("</script>\n");

    // JS
    o.push_str("<script>\n");
    o.push_str(JS);
    o.push_str("</script>\n");
    o.push_str("</body>\n</html>\n");
    o
}

fn build_tasks_json(tasks: &[TaskData]) -> String {
    let v: Vec<Value> = tasks.iter().map(|t| json!({
        "id": t.id,
        "display_id": t.display_id,
        "state": t.state,
        "agent": t.agent,
        "tags": t.tags,
        "title": t.title,
    })).collect();
    serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
}

fn build_events_json(events: &[EventData]) -> String {
    let v: Vec<Value> = events.iter().map(|e| {
        let summary = summarize_payload(&e.kind, &e.payload);
        json!({
            "task": e.task,
            "ts": e.ts,
            "kind": e.kind,
            "agent": e.agent,
            "summary": summary,
        })
    }).collect();
    serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
}

/// Same per-kind summary as `qp timeline` human mode.
fn summarize_payload(kind: &str, p: &Value) -> String {
    match kind {
        "state_change" => format!("→ {}", p.get("to").and_then(|v| v.as_str()).unwrap_or("")),
        "decision" => {
            let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if p.get("auto").and_then(|v| v.as_bool()).unwrap_or(false) {
                format!("[auto] {text}")
            } else { text.to_string() }
        }
        "dep_added" | "dep_removed" =>
            p.get("on").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "tag_added" | "tag_removed" =>
            p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "blocker" =>
            p.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "edit" => {
            if let Some(obj) = p.get("changes").and_then(|v| v.as_object()) {
                obj.keys().cloned().collect::<Vec<_>>().join(",")
            } else { String::new() }
        }
        _ => {
            let s = serde_json::to_string(p).unwrap_or_default();
            if s.len() > 80 { format!("{}...", &s[..80]) } else { s }
        }
    }
}

// ---------- SVG ----------

const NODE_W: f64 = 110.0;
const NODE_H: f64 = 38.0;
const LAYER_H: f64 = 80.0;
const PAD_X: f64 = 30.0;
const PAD_Y: f64 = 20.0;
const COL_SPACING: f64 = 130.0;

fn build_svg(layout: &Layout) -> String {
    // Determine canvas dimensions.
    let max_layer_width = layout.nodes.iter().map(|n| n.layer_width).max().unwrap_or(1);
    let width = (max_layer_width as f64) * COL_SPACING + 2.0 * PAD_X;
    let height = (layout.layer_count.max(1) as f64) * LAYER_H + 2.0 * PAD_Y;

    let pos = |n: &LaidNode| -> (f64, f64) {
        let layer_w = (n.layer_width as f64) * COL_SPACING;
        let layer_left = (width - layer_w) / 2.0;
        let cx = layer_left + (n.col as f64 + 0.5) * COL_SPACING;
        let cy = PAD_Y + (n.layer as f64 + 0.5) * LAYER_H;
        (cx, cy)
    };

    let mut id_pos = std::collections::HashMap::new();
    for n in &layout.nodes {
        id_pos.insert(n.id, pos(n));
    }

    let mut o = String::new();
    o.push_str(&format!(
        "<svg class=\"dag\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {:.0} {:.0}\" preserveAspectRatio=\"xMidYMin meet\">\n",
        width, height));

    // edges first (under nodes)
    o.push_str("<g class=\"edges\">\n");
    for e in &layout.edges {
        if let (Some((x1, y1)), Some((x2, y2))) = (id_pos.get(&e.from), id_pos.get(&e.to)) {
            // task (above) → dep (below). Line from bottom of task to top of dep.
            let sy = y1 + NODE_H / 2.0;
            let ey = y2 - NODE_H / 2.0;
            o.push_str(&format!(
                "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" />\n",
                x1, sy, x2, ey));
        }
    }
    o.push_str("</g>\n");

    // nodes
    o.push_str("<g class=\"nodes\">\n");
    for n in &layout.nodes {
        let (cx, cy) = pos(n);
        let x = cx - NODE_W / 2.0;
        let y = cy - NODE_H / 2.0;
        o.push_str(&format!(
            "<g class=\"node n-{}\" data-task=\"{}\" data-id=\"{}\">\n",
            html_esc(&n.state), html_esc(&n.display_id), n.id));
        o.push_str(&format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"5\" ry=\"5\" />\n",
            x, y, NODE_W, NODE_H));
        o.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" class=\"nlabel\">{}</text>\n",
            cx, cy + 4.0, html_esc(&n.display_id)));
        o.push_str("</g>\n");
    }
    o.push_str("</g>\n");

    o.push_str("</svg>\n");
    o
}

// ---------- HTML / JSON escaping ----------

pub fn html_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// JSON embedded in a <script> block must not contain the substring `</`
/// (which would terminate the script element). Escape `<` defensively.
fn escape_json_in_script(s: &str) -> String {
    s.replace("</", "<\\/").replace("<!--", "<\\!--")
}

// ---------- styles + JS ----------

const CSS: &str = r#":root{
  --bg:#0e1117;--panel:#161b22;--panel-2:#1c222b;--border:#2a3140;
  --text:#d4d4dc;--dim:#7a8499;--accent:#6cb6ff;
  --s-pending:#fbbf24;--s-ready:#4ade80;--s-assigned:#22d3ee;
  --s-running:#60a5fa;--s-done:#71717a;--s-cancelled:#3f3f46;
}
*{box-sizing:border-box}
html,body{margin:0;padding:0;background:var(--bg);color:var(--text);
  font:13px/1.5 ui-monospace,"SF Mono",Menlo,Consolas,monospace}
.mono,code{font-family:ui-monospace,"SF Mono",Menlo,Consolas,monospace}
code{background:var(--panel-2);padding:1px 5px;border-radius:3px;font-size:12px}
button,input{font:inherit;color:inherit}

.hdr{display:flex;align-items:center;gap:18px;
  padding:10px 16px;border-bottom:1px solid var(--border);
  background:var(--panel);position:sticky;top:0;z-index:10}
.hdr-l{font-weight:600}
.hdr-l .proj{color:var(--accent);font-size:15px}
.hdr-l .scope{color:var(--dim);margin-left:8px;font-size:12px}
.hdr-c{flex:1;display:flex;gap:10px;flex-wrap:wrap}
.hdr-r{color:var(--dim);font-size:11.5px}
.sc{display:inline-flex;gap:6px;align-items:center;padding:2px 8px;
  border-radius:3px;font-size:11px;background:var(--panel-2);border:1px solid var(--border)}
.sc-l{color:var(--dim);text-transform:uppercase;letter-spacing:0.05em}
.sc-v{font-weight:600}
.sc-pending .sc-v{color:var(--s-pending)}
.sc-ready .sc-v{color:var(--s-ready)}
.sc-assigned .sc-v{color:var(--s-assigned)}
.sc-running .sc-v{color:var(--s-running)}
.sc-done .sc-v{color:var(--s-done)}
.sc-cancelled .sc-v{color:var(--s-cancelled)}

.grid{display:grid;grid-template-columns:minmax(0,1.4fr) minmax(0,1fr);
  grid-template-rows:minmax(0,1fr) minmax(0,260px);grid-gap:12px;
  padding:12px;height:calc(100vh - 47px)}
.pnl-tasks{grid-row:1;grid-column:1;min-height:0;overflow:hidden;display:flex;flex-direction:column}
.pnl-dag{grid-row:1;grid-column:2;min-height:0;overflow:auto}
.pnl-timeline{grid-row:2;grid-column:1 / span 2;min-height:0;overflow:hidden;display:flex;flex-direction:column}
.panel{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:12px}
.panel h2{margin:0 0 8px;font-size:11px;color:var(--accent);
  text-transform:uppercase;letter-spacing:0.06em;font-weight:600}

.filters{display:flex;flex-direction:column;gap:8px;margin-bottom:8px}
.chip-row{display:flex;gap:6px;flex-wrap:wrap}
.chip{background:var(--panel-2);border:1px solid var(--border);
  color:var(--dim);padding:3px 10px;border-radius:12px;cursor:pointer;font-size:11px}
.chip.on{background:var(--accent);color:var(--bg);border-color:var(--accent)}
.chip-state[data-state="pending"].on{background:var(--s-pending);border-color:var(--s-pending)}
.chip-state[data-state="ready"].on{background:var(--s-ready);border-color:var(--s-ready)}
.chip-state[data-state="assigned"].on{background:var(--s-assigned);border-color:var(--s-assigned)}
.chip-state[data-state="running"].on{background:var(--s-running);border-color:var(--s-running)}
.chip-state[data-state="done"].on{background:var(--s-done);border-color:var(--s-done);color:#fff}
.chip-state[data-state="cancelled"].on{background:var(--s-cancelled);border-color:var(--s-cancelled);color:#fff}
.text-row{display:flex;gap:6px;flex-wrap:wrap}
.text-row input{background:var(--panel-2);border:1px solid var(--border);
  border-radius:4px;padding:4px 8px;flex:1;min-width:140px;color:var(--text)}
.text-row input:focus{outline:none;border-color:var(--accent)}
.text-row button{background:var(--panel-2);border:1px solid var(--border);
  border-radius:4px;padding:4px 10px;color:var(--dim);cursor:pointer}
.text-row button:hover{color:var(--text)}
.facets{display:flex;gap:4px;flex-wrap:wrap}
.facet{background:var(--panel-2);border:1px solid var(--border);
  color:var(--dim);padding:1px 7px;border-radius:10px;cursor:pointer;font-size:10.5px}
.facet:hover{color:var(--accent);border-color:var(--accent)}
.facet .cnt{color:var(--text);opacity:.6;margin-left:4px}

.task-wrap{flex:1;overflow:auto;min-height:0}
table.tasks{width:100%;border-collapse:collapse;font-size:12.5px}
table.tasks th{position:sticky;top:0;background:var(--panel);
  text-align:left;padding:5px 8px;font-weight:500;color:var(--dim);
  font-size:10.5px;text-transform:uppercase;letter-spacing:0.05em;
  border-bottom:1px solid var(--border);z-index:2}
table.tasks td{padding:4px 8px;border-bottom:1px solid var(--border);
  vertical-align:top}
table.tasks tr{cursor:pointer}
table.tasks tr:hover td{background:var(--panel-2)}
table.tasks tr.flash td{background:rgba(108,182,255,0.18);transition:background 1.2s}
.tid{color:var(--accent);font-weight:600}
.tagcell{color:var(--dim);font-size:11px}
.pill{display:inline-block;padding:1px 6px;border-radius:3px;font-size:10.5px;font-weight:600}
.pill-pending{background:rgba(251,191,36,0.18);color:var(--s-pending)}
.pill-ready{background:rgba(74,222,128,0.18);color:var(--s-ready)}
.pill-assigned{background:rgba(34,211,238,0.18);color:var(--s-assigned)}
.pill-running{background:rgba(96,165,250,0.18);color:var(--s-running)}
.pill-done{background:rgba(113,113,122,0.22);color:#d4d4dc}
.pill-cancelled{background:rgba(63,63,70,0.4);color:#a1a1aa}

svg.dag{width:100%;height:auto;display:block}
svg.dag .edges line{stroke:#3a4358;stroke-width:1.2}
svg.dag .nodes rect{stroke:var(--border);stroke-width:1;fill:var(--panel-2)}
svg.dag .nodes .nlabel{fill:var(--text);font:600 11px ui-monospace,monospace;pointer-events:none}
svg.dag .nodes g{cursor:pointer}
svg.dag .nodes g:hover rect{stroke:var(--accent);stroke-width:2}
svg.dag .nodes .n-pending rect{fill:rgba(251,191,36,0.2);stroke:var(--s-pending)}
svg.dag .nodes .n-ready rect{fill:rgba(74,222,128,0.2);stroke:var(--s-ready)}
svg.dag .nodes .n-assigned rect{fill:rgba(34,211,238,0.2);stroke:var(--s-assigned)}
svg.dag .nodes .n-running rect{fill:rgba(96,165,250,0.2);stroke:var(--s-running)}
svg.dag .nodes .n-done rect{fill:rgba(113,113,122,0.2);stroke:var(--s-done)}
svg.dag .nodes .n-cancelled rect{fill:rgba(63,63,70,0.3);stroke:var(--s-cancelled)}
.too-big,.empty{color:var(--dim);font-style:italic;padding:20px}

.tl-wrap{flex:1;overflow:auto;min-height:0}
.tl{list-style:none;margin:0;padding:0;font-size:12px}
.tl li{display:grid;grid-template-columns:160px 70px 80px 90px 1fr;gap:8px;
  padding:3px 8px;border-bottom:1px solid var(--border);cursor:pointer}
.tl li:hover{background:var(--panel-2)}
.tl .ts{color:var(--dim);font-size:11px}
.tl .kind{color:var(--accent);font-size:11px}
.tl .who{color:var(--dim);font-size:11px}
.tl .who-task{color:var(--accent);font-weight:600;font-size:11px}
.tl .body{color:var(--text)}
"#;

const JS: &str = r#"(function(){
  const tasks = JSON.parse(document.getElementById('qp-tasks').textContent);
  const events = JSON.parse(document.getElementById('qp-events').textContent);
  const STATES = ['pending','ready','assigned','running','done','cancelled'];

  const filters = { states: new Set(), tag: '', agent: '', title: '' };

  // ---- elements ----
  const tbody = document.getElementById('task-body');
  const tlist = document.getElementById('timeline');
  const fTag = document.getElementById('f-tag');
  const fAgent = document.getElementById('f-agent');
  const fTitle = document.getElementById('f-title');
  const fClear = document.getElementById('f-clear');
  const facets = document.getElementById('tag-facets');

  // ---- state chips ----
  document.querySelectorAll('.chip-state').forEach(btn => {
    btn.addEventListener('click', () => {
      const s = btn.dataset.state;
      if (filters.states.has(s)) { filters.states.delete(s); btn.classList.remove('on'); }
      else { filters.states.add(s); btn.classList.add('on'); }
      renderTasks();
    });
  });

  // ---- text filters ----
  function bindText(el, key){
    el.addEventListener('input', () => { filters[key] = el.value.trim(); renderTasks(); });
  }
  bindText(fTag, 'tag');
  bindText(fAgent, 'agent');
  bindText(fTitle, 'title');
  fClear.addEventListener('click', () => {
    filters.states.clear();
    document.querySelectorAll('.chip-state.on').forEach(c => c.classList.remove('on'));
    fTag.value = ''; fAgent.value = ''; fTitle.value = '';
    filters.tag = ''; filters.agent = ''; filters.title = '';
    renderTasks();
  });

  // ---- tag facets (top 12 by count) ----
  function buildFacets(){
    const counts = new Map();
    tasks.forEach(t => (t.tags || []).forEach(n => counts.set(n, (counts.get(n) || 0) + 1)));
    const top = Array.from(counts.entries()).sort((a,b)=>b[1]-a[1]).slice(0, 12);
    facets.innerHTML = '';
    top.forEach(([name, c]) => {
      const b = document.createElement('button');
      b.className = 'facet';
      b.innerHTML = name + '<span class="cnt">' + c + '</span>';
      b.addEventListener('click', () => { fTag.value = name; filters.tag = name; renderTasks(); });
      facets.appendChild(b);
    });
  }

  // ---- task table render ----
  function passes(t){
    if (filters.states.size > 0 && !filters.states.has(t.state)) return false;
    if (filters.tag && !(t.tags || []).some(n => n.startsWith(filters.tag))) return false;
    if (filters.agent && !(t.agent || '').startsWith(filters.agent)) return false;
    if (filters.title && !(t.title || '').toLowerCase().includes(filters.title.toLowerCase())) return false;
    return true;
  }
  function esc(s){ return (s == null ? '' : String(s))
    .replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'); }

  function renderTasks(){
    const frag = document.createDocumentFragment();
    tasks.forEach(t => {
      if (!passes(t)) return;
      const tr = document.createElement('tr');
      tr.dataset.task = t.display_id;
      tr.innerHTML =
        '<td class="tid">' + esc(t.display_id) + '</td>' +
        '<td><span class="pill pill-' + esc(t.state) + '">' + esc(t.state) + '</span></td>' +
        '<td>' + esc(t.agent || '-') + '</td>' +
        '<td class="tagcell">' + esc((t.tags || []).join(', ')) + '</td>' +
        '<td>' + esc(t.title) + '</td>';
      tr.addEventListener('click', () => highlightTask(t.display_id));
      frag.appendChild(tr);
    });
    tbody.innerHTML = '';
    tbody.appendChild(frag);
  }

  // ---- timeline ----
  function renderTimeline(){
    const frag = document.createDocumentFragment();
    events.forEach(e => {
      const li = document.createElement('li');
      li.dataset.task = e.task || '';
      li.innerHTML =
        '<span class="ts">' + esc(e.ts) + '</span>' +
        '<span class="kind">' + esc(e.kind) + '</span>' +
        '<span class="who-task">' + esc(e.task || '-') + '</span>' +
        '<span class="who">' + esc(e.agent || '-') + '</span>' +
        '<span class="body">' + esc(e.summary) + '</span>';
      li.addEventListener('click', () => { if (e.task) highlightTask(e.task); });
      frag.appendChild(li);
    });
    tlist.innerHTML = '';
    tlist.appendChild(frag);
  }

  // ---- DAG node clicks ----
  document.querySelectorAll('svg.dag .nodes g[data-task]').forEach(g => {
    g.addEventListener('click', () => highlightTask(g.dataset.task));
  });

  function highlightTask(displayId){
    document.querySelectorAll('#task-body tr.flash').forEach(r => r.classList.remove('flash'));
    const row = document.querySelector('#task-body tr[data-task="' + displayId + '"]');
    if (row) {
      row.classList.add('flash');
      row.scrollIntoView({block:'nearest', behavior:'smooth'});
    } else {
      // Hidden by filters — clear filters to reveal it.
      filters.states.clear();
      document.querySelectorAll('.chip-state.on').forEach(c => c.classList.remove('on'));
      fTag.value = ''; fAgent.value = ''; fTitle.value = '';
      filters.tag = ''; filters.agent = ''; filters.title = '';
      renderTasks();
      const r2 = document.querySelector('#task-body tr[data-task="' + displayId + '"]');
      if (r2) { r2.classList.add('flash'); r2.scrollIntoView({block:'nearest', behavior:'smooth'}); }
    }
  }

  buildFacets();
  renderTasks();
  renderTimeline();
})();
"#;
