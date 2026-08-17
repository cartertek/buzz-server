use buzz_core::Keys;
use buzz_server::{events, relay_adapter::BuzzWsFactory};
use std::{
    env,
    io::{self, Write},
};
use tokio::sync::watch;

fn usage() -> ! {
    eprintln!("usage: buzz-events subscribe");
    std::process::exit(64);
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("subscribe") || args.next().is_some() {
        usage();
    }
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
    let result = events::run(&factory, &relay, shutdown_rx, |value| {
        let mut stdout = io::stdout().lock();
        let _ = serde_json::to_writer(&mut stdout, &value);
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    })
    .await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
