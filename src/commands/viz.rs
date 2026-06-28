use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize)]
struct VizNode {
    id: String,
    label: String,
    category: String,
    risk_score: f64,
    community: Option<i64>,
}

#[derive(Serialize)]
struct VizEdge {
    from: String,
    to: String,
    label: String,
}

pub fn execute_viz(
    output_path: Option<PathBuf>,
    limit: usize,
    depth: usize,
    entity: Option<String>,
    view: String,
) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    if view.eq_ignore_ascii_case("services") {
        return execute_viz_services(output_path, layout);
    }

    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path())?;

    let cozo = storage
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB not initialized. Run 'index' first."))?;

    // 1. Fetch scoped nodes and edges
    let (nodes_script, edges_script) = if let Some(ref root_entity) = entity {
        // Scoped traversal from root entity
        let root = crate::commands::ask::escape_cozo_string(root_entity);
        (
            format!(
                r#"
                reachable[id, d] := *node{{id}}, id == '{root}', d = 0
                reachable[id, d] := reachable[prev, d_prev], *edge{{source: prev, target: id}}, d = d_prev + 1, d <= {depth}
                reachable[id, d] := reachable[prev, d_prev], *edge{{source: id, target: prev}}, d = d_prev + 1, d <= {depth}
                
                ?[id, label, category, risk_score] := distinct reachable[id, _], *node{{id, label, category, risk_score}}
                :limit {limit}
                "#
            ),
            format!(
                r#"
                reachable[id, d] := *node{{id}}, id == '{root}', d = 0
                reachable[id, d] := reachable[prev, d_prev], *edge{{source: prev, target: id}}, d = d_prev + 1, d <= {depth}
                reachable[id, d] := reachable[prev, d_prev], *edge{{source: id, target: prev}}, d = d_prev + 1, d <= {depth}

                ?[source, target, relation] := distinct reachable[source, _], distinct reachable[target, _], *edge{{source, target, relation}}
                "#
            ),
        )
    } else {
        // Global view with limit
        (
            format!(
                "?[id, label, category, risk_score] := *node{{id, label, category, risk_score}} :limit {limit}"
            ),
            format!(
                "?[source, target, relation] := *edge{{source, target, relation}} :limit {}",
                limit * 2
            ),
        )
    };

    // 2. Run Louvain community detection (scoped to the retrieved nodes if possible)
    let louvain_script = if entity.is_some() {
        format!(
            r#"
            {nodes_script}
            scoped_edges[src, dst] := ?[src, dst, _], *edge{{source: src, target: dst}}
            ?[community_id, node] <~ CommunityDetectionLouvain(scoped_edges[src, dst], undirected: true)
            "#
        )
    } else {
        "
        edges[src, dst] := *edge{source: src, target: dst}
        ?[community_id, node] <~ CommunityDetectionLouvain(edges[src, dst], undirected: true)
        "
        .to_string()
    };

    let mut communities = std::collections::HashMap::new();
    if let Ok(res) = cozo.run_script(&louvain_script) {
        for row in res.rows {
            if let (Some(cozo::DataValue::List(list)), Some(cozo::DataValue::Str(node))) =
                (row.first(), row.get(1))
            {
                let comm = list
                    .first()
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);
                communities.insert(node.to_string(), comm);
            }
        }
    }

    let nodes_res = cozo.run_script(&nodes_script)?;
    let mut nodes = Vec::new();
    for row in nodes_res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(category)),
            Some(cozo::DataValue::Num(cozo::Num::Float(risk))),
        ) = (
            row.first().cloned(),
            row.get(1).cloned(),
            row.get(2).cloned(),
            row.get(3).cloned(),
        ) {
            let community = communities.get(id.as_str()).copied();
            nodes.push(VizNode {
                id: id.to_string(),
                label: label.to_string(),
                category: category.to_string(),
                risk_score: risk,
                community,
            });
        }
    }

    let edges_res = cozo.run_script(&edges_script)?;
    let mut edges = Vec::new();
    for row in edges_res.rows {
        if let (
            Some(cozo::DataValue::Str(source)),
            Some(cozo::DataValue::Str(target)),
            Some(cozo::DataValue::Str(relation)),
        ) = (
            row.first().cloned(),
            row.get(1).cloned(),
            row.get(2).cloned(),
        ) {
            edges.push(VizEdge {
                from: source.to_string(),
                to: target.to_string(),
                label: relation.to_string(),
            });
        }
    }

    let html = generate_html(&nodes, &edges);

    let out = output_path.unwrap_or_else(|| layout.reports_dir().join("graph.html").into());
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    fs::write(&out, html).into_diagnostic()?;

    println!("Visualization generated at {}", out.display());
    Ok(())
}

fn generate_html(nodes: &[VizNode], edges: &[VizEdge]) -> String {
    let nodes_json = serde_json::to_string(nodes).unwrap_or_else(|_| "[]".to_string());
    let edges_json = serde_json::to_string(edges).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>Ledgerful Knowledge Graph</title>
    <script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
    <style>
        body {{
            background: #0f0f1a;
            color: #e0e0e0;
            font-family: sans-serif;
            margin: 0;
            display: flex;
            height: 100vh;
        }}
        #mynetwork {{
            flex: 1;
            height: 100%;
        }}
        #sidebar {{
            width: 300px;
            background: #1a1a2e;
            border-left: 1px solid #2a2a4e;
            padding: 15px;
            overflow-y: auto;
            display: flex;
            flex-direction: column;
        }}
        h2 {{ font-size: 1.2em; margin-top: 0; color: #4E79A7; }}
        .node-info {{ margin-bottom: 10px; padding: 10px; background: #0f0f1a; border-radius: 4px; }}
        .risk-high {{ color: #ff5555; font-weight: bold; }}
        .filter-section {{ margin-top: 20px; }}
        select, button {{ width: 100%; padding: 8px; margin-top: 5px; background: #2a2a4e; color: white; border: none; border-radius: 4px; }}
        #loading {{ position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%); font-size: 24px; color: #e0e0e0; z-index: 10; }}
    </style>
</head>
<body>
    <div id="loading">Stabilizing Graph Physics (Please Wait)...</div>
    <div id="mynetwork"></div>
    <div id="sidebar">
        <h2>Graph Info</h2>
        <div id="selection">Click a node to see details</div>
        <hr/>
        <p>Nodes: {node_count}</p>
        <p>Edges: {edge_count}</p>
        
        <div class="filter-section">
            <label for="communityFilter">Filter by Community:</label>
            <select id="communityFilter" onchange="applyFilters()">
                <option value="all">All Communities</option>
            </select>
        </div>
        
        <div class="filter-section">
            <label for="riskFilter">Filter by Risk:</label>
            <select id="riskFilter" onchange="applyFilters()">
                <option value="all">All Risks</option>
                <option value="high">High Risk (>0.7)</option>
                <option value="medium">Medium Risk (>0.3)</option>
            </select>
        </div>
        
        <div class="filter-section">
            <label for="searchInput">Search Node:</label>
            <input type="text" id="searchInput" placeholder="Type node label..." style="width: 100%; padding: 8px; margin-top: 5px; background: #2a2a4e; color: white; border: none; border-radius: 4px; box-sizing: border-box;" onkeypress="if(event.key === 'Enter') searchNode()">
            <button onclick="searchNode()">Search</button>
        </div>
        
        <button onclick="resetGraph()">Reset Graph</button>
    </div>

    <script type="text/javascript">
        const raw_nodes = {nodes_json};
        const raw_edges = {edges_json};

        const colorPalette = [
            '#4E79A7', '#F28E2B', '#E15759', '#76B7B2', '#59A14F', 
            '#EDC948', '#B07AA1', '#FF9DA7', '#9C755F', '#BAB0AC'
        ];

        // Populate community filter
        const communities = new Set();
        raw_nodes.forEach(n => {{ if (n.community !== null) communities.add(n.community); }});
        const commSelect = document.getElementById('communityFilter');
        Array.from(communities).sort((a,b) => a-b).forEach(c => {{
            const opt = document.createElement('option');
            opt.value = c;
            opt.innerHTML = `Community ${{c}}`;
            commSelect.appendChild(opt);
        }});

        const nodes = new vis.DataSet(raw_nodes.map(n => {{
            let bgColor = '#4E79A7';
            if (n.risk_score > 0.7) {{
                bgColor = '#ff5555';
            }} else if (n.community !== null && n.community !== undefined) {{
                bgColor = colorPalette[Math.abs(n.community) % colorPalette.length];
            }}
            return {{
                id: n.id,
                label: n.label,
                group: n.community,
                color: {{
                    background: bgColor,
                    border: '#2a2a4e'
                }},
                value: 1 + (n.risk_score * 5),
                title: n.label + ' (' + n.category + ')' + (n.community !== null ? ` [Comm: ${{n.community}}]` : '')
            }};
        }}));

        const edges = new vis.DataSet(raw_edges.map(e => ({{
            from: e.from,
            to: e.to,
            label: e.label,
            arrows: 'to',
            font: {{ size: 10, align: 'middle', color: '#888' }},
            color: {{ color: '#2a2a4e' }}
        }})));

        const container = document.getElementById('mynetwork');
        let data = {{ nodes: nodes, edges: edges }};
        const options = {{
            nodes: {{
                shape: 'dot',
                font: {{ color: '#e0e0e0', size: 12 }},
                borderWidth: 2
            }},
            edges: {{
                width: 1,
                smooth: {{ type: 'continuous' }}
            }},
            physics: {{
                stabilization: {{
                    enabled: true,
                    iterations: 150,
                    updateInterval: 25
                }},
                barnesHut: {{
                    gravitationalConstant: -2000,
                    centralGravity: 0.3,
                    springLength: 95
                }}
            }}
        }};
        let network = new vis.Network(container, data, options);
        
        network.on("stabilizationIterationsDone", function () {{
            network.setOptions( {{ physics: false }} );
            document.getElementById('loading').style.display = 'none';
        }});

        network.on("click", function (params) {{
            if (params.nodes.length > 0) {{
                const nodeId = params.nodes[0];
                const node = raw_nodes.find(n => n.id === nodeId);
                if (node) {{
                    document.getElementById('selection').innerHTML = `
                        <div class="node-info">
                            <b>ID:</b> ${{node.id}}<br/>
                            <b>Label:</b> ${{node.label}}<br/>
                            <b>Category:</b> ${{node.category}}<br/>
                            <b>Community:</b> ${{node.community !== null ? node.community : 'N/A'}}<br/>
                            <b>Risk Score:</b> <span class="${{node.risk_score > 0.7 ? 'risk-high' : ''}}">${{node.risk_score.toFixed(2)}}</span>
                        </div>
                    `;
                }}
            }}
        }});
        
        function applyFilters() {{
            const commVal = document.getElementById('communityFilter').value;
            const riskVal = document.getElementById('riskFilter').value;
            
            const filteredNodes = raw_nodes.filter(n => {{
                let commMatch = (commVal === 'all') || (n.community == commVal);
                let riskMatch = true;
                if (riskVal === 'high') riskMatch = n.risk_score > 0.7;
                if (riskVal === 'medium') riskMatch = n.risk_score > 0.3;
                return commMatch && riskMatch;
            }});
            
            const filteredIds = new Set(filteredNodes.map(n => n.id));
            const filteredEdges = raw_edges.filter(e => filteredIds.has(e.from) && filteredIds.has(e.to));
            
            nodes.clear();
            edges.clear();
            
            nodes.add(filteredNodes.map(n => {{
                let bgColor = '#4E79A7';
                if (n.risk_score > 0.7) {{
                    bgColor = '#ff5555';
                }} else if (n.community !== null && n.community !== undefined) {{
                    bgColor = colorPalette[Math.abs(n.community) % colorPalette.length];
                }}
                return {{
                    id: n.id,
                    label: n.label,
                    group: n.community,
                    color: {{ background: bgColor, border: '#2a2a4e' }},
                    value: 1 + (n.risk_score * 5),
                    title: n.label + ' (' + n.category + ')' + (n.community !== null ? ` [Comm: ${{n.community}}]` : '')
                }};
            }}));
            edges.add(filteredEdges);
        }}
        
        function resetGraph() {{
            document.getElementById('communityFilter').value = 'all';
            document.getElementById('riskFilter').value = 'all';
            document.getElementById('searchInput').value = '';
            applyFilters();
            network.fit();
        }}
        
        function searchNode() {{
            const query = document.getElementById('searchInput').value.toLowerCase();
            if (!query) return;
            
            const found = raw_nodes.find(n => n.label.toLowerCase().includes(query) || n.id.toLowerCase().includes(query));
            if (found) {{
                network.selectNodes([found.id]);
                network.focus(found.id, {{
                    scale: 1.5,
                    animation: {{
                        duration: 1000,
                        easingFunction: 'easeInOutQuad'
                    }}
                }});
                document.getElementById('selection').innerHTML = `
                    <div class="node-info">
                        <b>ID:</b> ${{found.id}}<br/>
                        <b>Label:</b> ${{found.label}}<br/>
                        <b>Category:</b> ${{found.category}}<br/>
                        <b>Community:</b> ${{found.community !== null ? found.community : 'N/A'}}<br/>
                        <b>Risk Score:</b> <span class="${{found.risk_score > 0.7 ? 'risk-high' : ''}}">${{found.risk_score.toFixed(2)}}</span>
                    </div>
                `;
            }} else {{
                alert("Node not found!");
            }}
        }}
    </script>
</body>
</html>"#,
        node_count = nodes.len(),
        edge_count = edges.len(),
        nodes_json = nodes_json,
        edges_json = edges_json
    )
}

// ---------------------------------------------------------------------------
// K4: Service Connectivity Visualization
// ---------------------------------------------------------------------------

/// Renders the service boundary and communication graph as an HTML file.
fn execute_viz_services(output_path: Option<PathBuf>, layout: Layout) -> Result<()> {
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path())?;

    let cozo = storage
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB not initialized. Run 'ledgerful index' first."))?;

    // Query service roots
    #[derive(serde::Serialize)]
    struct SvcNode {
        name: String,
        dir_path: String,
        marker_kind: String,
        confidence: f64,
    }

    #[derive(serde::Serialize)]
    struct SvcEdge {
        caller: String,
        callee: String,
        call_kind: String,
        pattern: String,
    }

    let mut svc_nodes: Vec<SvcNode> = Vec::new();
    let roots_res = cozo.run_script(
        "?[name, dir_path, marker_kind, confidence] := *service_roots{name, dir_path, marker_kind, confidence} :order name"
    );

    match roots_res {
        Ok(res) => {
            for row in res.rows {
                if let (
                    Some(cozo::DataValue::Str(name)),
                    Some(cozo::DataValue::Str(dir)),
                    Some(cozo::DataValue::Str(marker)),
                    Some(cozo::DataValue::Num(conf)),
                ) = (row.first(), row.get(1), row.get(2), row.get(3))
                {
                    let confidence = match conf {
                        cozo::Num::Float(f) => *f,
                        cozo::Num::Int(i) => *i as f64,
                    };
                    svc_nodes.push(SvcNode {
                        name: name.to_string(),
                        dir_path: dir.to_string(),
                        marker_kind: marker.to_string(),
                        confidence,
                    });
                }
            }
        }
        Err(e) => {
            println!("Warning: Could not query service_roots (run 'ledgerful index' first): {e}");
        }
    }

    let mut svc_edges: Vec<SvcEdge> = Vec::new();
    let deps_res = cozo.run_script(
        "?[caller_service, callee_service, call_kind, pattern] := *service_dependencies{caller_service, callee_service, call_kind, pattern} :order caller_service"
    );

    match deps_res {
        Ok(res) => {
            for row in res.rows {
                if let (
                    Some(cozo::DataValue::Str(caller)),
                    Some(cozo::DataValue::Str(callee)),
                    Some(cozo::DataValue::Str(call_kind)),
                    Some(cozo::DataValue::Str(pattern)),
                ) = (row.first(), row.get(1), row.get(2), row.get(3))
                {
                    svc_edges.push(SvcEdge {
                        caller: caller.to_string(),
                        callee: callee.to_string(),
                        call_kind: call_kind.to_string(),
                        pattern: pattern.to_string(),
                    });
                }
            }
        }
        Err(e) => {
            println!("Warning: Could not query service_dependencies: {e}");
        }
    }

    let html = generate_services_html(&svc_nodes, &svc_edges);

    let out = output_path.unwrap_or_else(|| layout.reports_dir().join("services.html").into());
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    fs::write(&out, html).into_diagnostic()?;

    println!("Service Connectivity Graph generated at {}", out.display());
    println!("  Services detected: {}", svc_nodes.len());
    println!("  Communication edges: {}", svc_edges.len());
    if svc_nodes.is_empty() {
        println!("  (Run 'ledgerful index' to populate service data)");
    }
    Ok(())
}

fn generate_services_html(
    nodes: &[impl serde::Serialize],
    edges: &[impl serde::Serialize],
) -> String {
    let nodes_json = serde_json::to_string(nodes).unwrap_or_else(|_| "[]".to_string());
    let edges_json = serde_json::to_string(edges).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Ledgerful — Service Connectivity</title>
    <script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{
            background: #0a0a14;
            color: #e2e8f0;
            font-family: 'Segoe UI', system-ui, sans-serif;
            display: flex;
            flex-direction: column;
            height: 100vh;
        }}
        header {{
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            border-bottom: 1px solid #2d3748;
            padding: 14px 24px;
            display: flex;
            align-items: center;
            gap: 16px;
        }}
        header h1 {{
            font-size: 1.15rem;
            font-weight: 600;
            background: linear-gradient(90deg, #63b3ed, #9f7aea);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
        }}
        .badge {{
            background: #2d3748;
            color: #a0aec0;
            border-radius: 20px;
            padding: 3px 10px;
            font-size: 0.78rem;
        }}
        .main {{
            display: flex;
            flex: 1;
            overflow: hidden;
        }}
        #graph-container {{
            flex: 1;
            position: relative;
        }}
        #network {{
            width: 100%;
            height: 100%;
        }}
        aside {{
            width: 310px;
            background: #111827;
            border-left: 1px solid #1f2937;
            overflow-y: auto;
            padding: 16px;
        }}
        .section-title {{
            font-size: 0.72rem;
            text-transform: uppercase;
            letter-spacing: 0.08em;
            color: #718096;
            margin-bottom: 10px;
            margin-top: 16px;
        }}
        .service-card {{
            background: #1a2035;
            border: 1px solid #2d3748;
            border-radius: 8px;
            padding: 10px 12px;
            margin-bottom: 8px;
            cursor: pointer;
            transition: border-color 0.2s, background 0.2s;
        }}
        .service-card:hover {{ border-color: #63b3ed; background: #1e2a4a; }}
        .service-name {{ font-weight: 600; font-size: 0.9rem; color: #e2e8f0; }}
        .service-meta {{ font-size: 0.76rem; color: #718096; margin-top: 3px; }}
        .marker-badge {{
            display: inline-block;
            background: #2a3a5a;
            color: #90cdf4;
            border-radius: 4px;
            padding: 1px 7px;
            font-size: 0.7rem;
            margin-top: 4px;
        }}
        .edge-item {{
            background: #161e30;
            border: 1px solid #2d3748;
            border-radius: 6px;
            padding: 8px 10px;
            margin-bottom: 6px;
            font-size: 0.78rem;
        }}
        .edge-caller {{ color: #63b3ed; font-weight: 600; }}
        .edge-arrow {{ color: #718096; margin: 0 6px; }}
        .edge-callee {{ color: #9f7aea; font-weight: 600; }}
        .edge-kind {{ color: #68d391; font-size: 0.7rem; margin-top: 3px; }}
        .edge-pattern {{ color: #a0aec0; font-size: 0.7rem; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
        .empty-state {{
            color: #4a5568;
            font-size: 0.85rem;
            text-align: center;
            padding: 24px 0;
            line-height: 1.5;
        }}
        #info-panel {{
            background: #1a2035;
            border: 1px solid #2d3748;
            border-radius: 8px;
            padding: 12px;
            margin-top: 16px;
            font-size: 0.82rem;
            min-height: 60px;
            color: #a0aec0;
        }}
    </style>
</head>
<body>
<header>
    <h1>⬡ Ledgerful — Service Connectivity Graph</h1>
    <span class="badge">{node_count} services</span>
    <span class="badge">{edge_count} connections</span>
</header>
<div class="main">
    <div id="graph-container">
        <div id="network"></div>
    </div>
    <aside>
        <div class="section-title">Services</div>
        <div id="service-list"></div>
        <div class="section-title">Communication Edges</div>
        <div id="edge-list"></div>
        <div class="section-title">Selected Node</div>
        <div id="info-panel">Click a node to inspect it.</div>
    </aside>
</div>
<script>
const SVC_NODES = {nodes_json};
const SVC_EDGES = {edges_json};

const markerColors = {{
    CARGO_WORKSPACE: '#f6ad55',
    NPM_PACKAGE: '#68d391',
    GO_MODULE: '#63b3ed',
    MAVEN_POM: '#fc8181',
    DOCKERFILE: '#9f7aea',
}};

function markerColor(kind) {{
    return markerColors[kind] || '#a0aec0';
}}

const visNodes = SVC_NODES.map((s, i) => ({{
    id: s.name,
    label: s.name,
    title: `<b>${{s.name}}</b><br/>Path: ${{s.dir_path}}<br/>Marker: ${{s.marker_kind}}<br/>Confidence: ${{(s.confidence * 100).toFixed(0)}}%`,
    shape: 'ellipse',
    color: {{
        background: markerColor(s.marker_kind),
        border: '#1a1a2e',
        highlight: {{ background: '#fff', border: '#63b3ed' }},
    }},
    font: {{ color: '#0a0a14', size: 14, bold: {{ size: 14 }} }},
    size: 28,
}}));

const edgeMap = {{}};
SVC_EDGES.forEach(e => {{
    const key = `${{e.caller}}_${{e.callee}}`;
    edgeMap[key] = (edgeMap[key] || 0) + 1;
}});

const visEdges = Object.entries(edgeMap).map(([key, count]) => {{
    const [from, to] = key.split('_');
    return {{
        from, to,
        label: String(count),
        arrows: 'to',
        color: {{ color: '#4a5568', highlight: '#63b3ed' }},
        font: {{ color: '#a0aec0', size: 11 }},
        width: Math.min(1 + count * 0.5, 5),
    }};
}});

const container = document.getElementById('network');
const network = new vis.Network(container, {{
    nodes: new vis.DataSet(visNodes),
    edges: new vis.DataSet(visEdges),
}}, {{
    physics: {{
        stabilization: {{ iterations: 120 }},
        barnesHut: {{ gravitationalConstant: -6000, centralGravity: 0.3, springLength: 160 }},
    }},
    interaction: {{ hover: true, tooltipDelay: 150 }},
    layout: {{ improvedLayout: true }},
}});

// Sidebar: services list
const serviceList = document.getElementById('service-list');
if (SVC_NODES.length === 0) {{
    serviceList.innerHTML = '<div class="empty-state">No services detected.<br/>Run <code>ledgerful index</code> first.</div>';
}} else {{
    SVC_NODES.forEach(s => {{
        const card = document.createElement('div');
        card.className = 'service-card';
        card.innerHTML = `
            <div class="service-name">${{s.name}}</div>
            <div class="service-meta">${{s.dir_path || '.'}}</div>
            <span class="marker-badge">${{s.marker_kind}}</span>
        `;
        card.onclick = () => {{
            network.selectNodes([s.name]);
            network.focus(s.name, {{ scale: 1.4, animation: {{ duration: 600 }} }});
        }};
        serviceList.appendChild(card);
    }});
}}

// Sidebar: edge list
const edgeList = document.getElementById('edge-list');
if (SVC_EDGES.length === 0) {{
    edgeList.innerHTML = '<div class="empty-state">No outbound HTTP calls detected.</div>';
}} else {{
    SVC_EDGES.slice(0, 50).forEach(e => {{
        const div = document.createElement('div');
        div.className = 'edge-item';
        div.innerHTML = `
            <div><span class="edge-caller">${{e.caller}}</span><span class="edge-arrow">→</span><span class="edge-callee">${{e.callee}}</span></div>
            <div class="edge-kind">${{e.call_kind}}</div>
            <div class="edge-pattern" title="${{e.pattern}}">${{e.pattern || '(dynamic URL)'}}</div>
        `;
        edgeList.appendChild(div);
    }});
    if (SVC_EDGES.length > 50) {{
        const more = document.createElement('div');
        more.className = 'empty-state';
        more.textContent = `... and ${{SVC_EDGES.length - 50}} more`;
        edgeList.appendChild(more);
    }}
}}

// Info panel on node click
network.on('click', params => {{
    if (params.nodes.length > 0) {{
        const name = params.nodes[0];
        const svc = SVC_NODES.find(s => s.name === name);
        const panel = document.getElementById('info-panel');
        if (svc) {{
            const outgoing = SVC_EDGES.filter(e => e.caller === name).length;
            panel.innerHTML = `
                <b>${{svc.name}}</b><br/>
                Path: ${{svc.dir_path || '.'}}<br/>
                Marker: <span style="color:#f6ad55">${{svc.marker_kind}}</span><br/>
                Confidence: ${{(svc.confidence * 100).toFixed(0)}}%<br/>
                Outgoing HTTP calls: ${{outgoing}}
            `;
        }}
    }}
}});
</script>
</body>
</html>"#,
        node_count = nodes.len(),
        edge_count = edges.len(),
        nodes_json = nodes_json,
        edges_json = edges_json
    )
}
