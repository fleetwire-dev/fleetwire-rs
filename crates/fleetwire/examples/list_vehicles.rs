//! Lists the vehicles on your Tesla account.
//!
//! This is a free call: `GET /api/1/vehicles` is not billed, and it does not wake the car.
//!
//! ```text
//! TESLA_CLIENT_ID=<your app's client id> \
//! FLEETWIRE_TOKEN_DIR=$HOME/.fleetwire \
//! cargo run --example list_vehicles
//! ```
//!
//! `FLEETWIRE_TOKEN_DIR` must be a directory only you can access (`0700`) that holds a
//! `tokens.json` (`0600`). See "Quick start" in the README for how to create it.
//! The VIN is printed masked; pass `--show-vin` to print it in full.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use fleetwire::api::vehicles::ListVehicles;
use fleetwire::{Client, HttpRefresher, TokenManager};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the error's message, not its `Debug` form, and point at the README.
            eprintln!("error: {e}");
            eprintln!("See \"If it fails\" in the fleetwire README for the usual causes.");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let client_id = std::env::var("TESLA_CLIENT_ID")
        .map_err(|_| "set TESLA_CLIENT_ID to your Tesla developer app's client id")?;
    let dir = std::env::var_os("FLEETWIRE_TOKEN_DIR")
        .map(PathBuf::from)
        .ok_or("set FLEETWIRE_TOKEN_DIR to the directory that holds tokens.json")?;
    let show_vin = std::env::args().any(|a| a == "--show-vin");

    // The token manager refreshes the access token when it is stale and writes the rotated
    // refresh token back to `tokens.json` before returning.
    let tokens = TokenManager::new(dir, Arc::new(HttpRefresher::new(client_id)));
    let client = Client::builder(tokens).build()?;

    let page = client.send(&ListVehicles {
        page: None,
        per_page: None,
    })?;

    println!("{} vehicle(s)", page.items.len());
    for v in &page.items {
        let vin = v.vin.as_deref().unwrap_or("?");
        let vin = if show_vin { vin.to_owned() } else { mask(vin) };
        println!("- vin {vin} · state {}", v.state.as_deref().unwrap_or("?"));
    }
    Ok(())
}

/// Keeps the first three and last four characters of a VIN.
fn mask(vin: &str) -> String {
    let chars: Vec<char> = vin.chars().collect();
    if chars.len() < 8 {
        return "*".repeat(chars.len());
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}{}{tail}", "*".repeat(chars.len() - 7))
}
