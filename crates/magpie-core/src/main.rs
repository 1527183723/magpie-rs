//! `magpie` — the gateway as a standalone binary.
//!
//! On Android this binary does not exist: the gateway runs inside the app's own
//! process. It is here so the same code can be exercised on a desktop, which is
//! how the protocol translation gets tested without a phone.

use magpie_core::gateway;

fn usage() {
    eprintln!(
        "magpie {ver}

USAGE
  magpie serve [--addr HOST:PORT] [--web HOST:PORT|off] [--config DIR] [--quiet]
  magpie key                    print a fresh gateway key
  magpie web                    print the page URL for the current config
  magpie providers              list the configured providers
  magpie --version

ENVIRONMENT
  MAGPIE_ADDR      gateway listen address (default 127.0.0.1:3425)
  MAGPIE_WEB       page listen address (default 127.0.0.1:3430, `off` to disable)
  MAGPIE_WEB_KEY   page key; generated and stored when absent
  XDG_CONFIG_HOME  config lives in $XDG_CONFIG_HOME/magpie
",
        ver = env!("CARGO_PKG_VERSION")
    );
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("magpie {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let command = args.remove(0);
    let mut opts = gateway::options_from_env();
    opts.echo_log = true;

    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" if i + 1 < args.len() => {
                opts.gateway_addr = args[i + 1].clone();
                i += 2;
            }
            "--web" if i + 1 < args.len() => {
                opts.web_addr = if args[i + 1] == "off" {
                    None
                } else {
                    Some(args[i + 1].clone())
                };
                i += 2;
            }
            "--config" if i + 1 < args.len() => {
                opts.config_dir = std::path::PathBuf::from(&args[i + 1]);
                opts.log_path = Some(opts.config_dir.join("magpie.log"));
                i += 2;
            }
            "--quiet" => {
                quiet = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    opts.quiet = quiet;

    match command.as_str() {
        "serve" => {}
        "key" => {
            println!("{}", magpie_core::config::new_gateway_key());
            return;
        }
        "web" => {
            let cfg = match magpie_core::config::Config::load(&opts.config_dir) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cannot read the config: {e}");
                    std::process::exit(1);
                }
            };
            let web = opts
                .web_addr
                .clone()
                .unwrap_or_else(|| format!("127.0.0.1:{}", cfg.settings.web_port));
            println!("http://{web}/?k={}", cfg.settings.web_key);
            return;
        }
        "providers" => {
            let cfg = match magpie_core::config::Config::load(&opts.config_dir) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cannot read the config: {e}");
                    std::process::exit(1);
                }
            };
            if cfg.providers.is_empty() {
                println!("(none configured — open the page and add one)");
            }
            for p in &cfg.providers {
                println!(
                    "{:<16} {:<10} {:<44} {} model(s){}",
                    p.id,
                    p.protocol.as_str(),
                    p.base(),
                    p.models.len(),
                    if p.enabled { "" } else { "  [disabled]" }
                );
            }
            return;
        }
        other => {
            eprintln!("unknown command: {other}");
            usage();
            std::process::exit(2);
        }
    }

    let gw = match magpie_core::Gateway::new(opts) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("cannot start: {e}");
            std::process::exit(1);
        }
    };

    let ctrl_c = tokio::signal::ctrl_c();
    let shutdown = async move {
        let _ = ctrl_c.await;
        eprintln!("\nstopping");
    };

    if let Err(e) = gw.run(shutdown).await {
        eprintln!("gateway error: {e}");
        std::process::exit(1);
    }
}
