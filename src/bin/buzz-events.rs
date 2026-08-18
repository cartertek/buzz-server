use buzz_core::Keys;
use buzz_server::{events, relay_adapter::BuzzWsFactory};
use std::{
    env,
    io::{self, Write},
};
use tokio::sync::watch;

fn usage() -> ! {
    eprintln!("usage: buzz-events subscribe [--filter '<Nostr filter JSON>']");
    std::process::exit(64);
}

fn help() -> ! {
    println!("usage: buzz-server events subscribe [--filter '<Nostr filter JSON>']");
    println!();
    println!("Subscribe to authenticated live relay events as JSONL.");
    println!(
        "--filter accepts one Nostr filter object; its `since` is managed by the live cursor."
    );
    println!(
        r##"example: buzz-server events subscribe --filter '{{"kinds":[1],"#p":["<pubkey>"]}}'"##
    );
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("subscribe") {
        usage();
    }
    let mut filter_json = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => help(),
            "--filter" => {
                let Some(value) = args.next() else { usage() };
                if filter_json.is_some() {
                    usage();
                }
                filter_json = Some(value);
            }
            _ => usage(),
        }
    }
    let filter = match events::parse_filter(filter_json.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(64);
        }
    };
    let relay = env::var("BUZZ_RELAY_URL").unwrap_or_else(|_| usage());
    let private_key = env::var("BUZZ_PRIVATE_KEY").unwrap_or_else(|_| usage());
    let keys: Keys = match events::parse_keys(&private_key) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(3);
        }
    };
    let auth_tag = match events::parse_auth_tag(env::var("BUZZ_AUTH_TAG").ok().as_deref()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(3);
        }
    };
    let factory = BuzzWsFactory {
        keys,
        authorization_tag: auth_tag,
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = signal_tx.send(true);
    });
    #[cfg(unix)]
    {
        let signal_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
                let _ = signal_tx.send(true);
            }
        });
    }
    let result = events::run_with_filter(
        &factory,
        &relay,
        shutdown_rx,
        |value| {
            let mut stdout = io::stdout().lock();
            let _ = serde_json::to_writer(&mut stdout, &value);
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
        },
        filter,
    )
    .await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
