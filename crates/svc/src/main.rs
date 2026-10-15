mod acl;
mod app;
mod engine;
mod hub;
mod install;
mod logging;
mod netid;
mod pipe;
mod procs;
mod service;
mod targets;
mod tuner;
mod system;
mod update;

const USAGE: &str = "zapretUI-svc install | uninstall [--purge] | run | portable | probe";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => install::install(),
        Some("uninstall") => install::uninstall(&args),
        Some("probe") => tuner::cli_probe(),
        Some("restart") => install::restart(),
        Some("autostart") => {
            let on = args.get(1).is_some_and(|a| a == "on");
            let user = args.iter().position(|a| a == "--user").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
            system::portable_autostart(on, &user)
        }
        Some("portable") => {
            let after = args.iter().position(|a| a == "--after").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok());
            service::run_portable(after)
        }
        Some("run") => {
            logging::init(true);
            service::run_console()
        }
        None => service::dispatch().map_err(|_| anyhow::anyhow!("{USAGE}")),
        Some(other) => anyhow::bail!("неизвестная команда {other}\n{USAGE}"),
    }
}
