mod config;
mod graph;
mod history;
mod lease;
mod model;
mod persist;
mod schedule;
// Phase 2:
mod breaker;
mod policy;
mod reduce;
mod state;

fn main() {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/craton/config.toml".to_string());

    match run(&config_path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    }
}

fn run(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(config_path)
        .map_err(|e| format!("reading config {config_path}: {e}"))?;

    let cfg = config::CratonConfig::from_toml(&raw)?;

    eprintln!(
        "cratond: config loaded — {} services, backup repo: {}",
        cfg.services.len(),
        cfg.backup.restic_repo,
    );

    let dep_graph = graph::DepGraph::build(&cfg.services)?;

    let order = dep_graph.topological_order();
    eprintln!("cratond: dependency order: {order:?}");

    // Phase 2: initialize state and verify reducer compiles.
    let st = state::State::new(&cfg, 0);
    eprintln!("cratond: state initialized — {} services tracked", st.services.len());

    eprintln!("cratond: stage 2 — all checks passed");
    Ok(())
}