//! Reads one vehicle's charge state, but only pays for it when the car is awake.
//!
//! `GET /api/1/vehicles/{vin}/vehicle_data` is **billed** ($0.002 per request, and a 408 from a
//! sleeping car is billed too). `Client::send_guarded` first asks the free
//! `GET /api/1/vehicles/{vin}` for the car's state and makes the billed call only when it
//! says `online`. Otherwise it reports `Guarded::Skipped` and nothing is spent. The car can
//! still fall asleep between the two calls, so this narrows the window; it does not close it.
//!
//! ```text
//! TESLA_CLIENT_ID=<your app's client id> \
//! FLEETWIRE_TOKEN_DIR=$HOME/.fleetwire \
//! cargo run --example guarded_vehicle_data -- <VIN>
//! ```
//!
//! Running it against an online car costs one billed request. `FLEETWIRE_TOKEN_DIR` is set up
//! the same way as for `list_vehicles` (see "Quick start" in the README); `list_vehicles
//! -- --show-vin` prints the VIN to pass here.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use fleetwire::api::vehicles::{DataEndpoint, GetVehicleData};
use fleetwire::{Billing, Client, FleetGet, Guarded, HttpRefresher, Pricing, TokenManager};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the error's message, not its `Debug` form, and point at the README.
            eprintln!("error: {e}");
            // Every client error can say whether the failed call cost money.
            if let Some(err) = e.downcast_ref::<fleetwire::Error>() {
                eprintln!("{}", spend_after_failure(err.billing()));
            }
            eprintln!("See \"If it fails\" in crates/fleetwire/README.md for the usual causes.");
            ExitCode::FAILURE
        }
    }
}

/// What a failed call cost, in words.
fn spend_after_failure(billing: Billing) -> &'static str {
    match billing {
        Billing::Billed => "spent: this failed call was billed",
        Billing::NotBilled => "spent: nothing, this failed call was not billed",
        // `Unknown`: the request may have reached Tesla. Do not send it again to find out.
        _ => "spent: unknown, the request may have reached Tesla before it failed",
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Name both variables at once, so a first run does not fail twice.
    let client_id = std::env::var("TESLA_CLIENT_ID").ok();
    let dir = std::env::var_os("FLEETWIRE_TOKEN_DIR").map(PathBuf::from);
    let (Some(client_id), Some(dir)) = (client_id, dir) else {
        return Err(
            "set TESLA_CLIENT_ID to your Tesla developer app's client id \
                    and FLEETWIRE_TOKEN_DIR to the directory that holds tokens.json"
                .into(),
        );
    };
    let vin = std::env::args()
        .nth(1)
        .ok_or("pass the vehicle's VIN as the first argument")?;

    let tokens = TokenManager::new(dir, Arc::new(HttpRefresher::new(client_id)));
    let client = Client::builder(tokens).build()?;

    // Ask only for charge_state: no location, so the `vehicle_location` scope is not needed.
    let req = GetVehicleData::new(vin, vec![DataEndpoint::ChargeState])?;
    // The request type knows its own price before anything is sent.
    let price = match req.pricing() {
        Pricing::Data { usd_per_request } => format!("${usd_per_request}"),
        _ => "nothing".to_owned(),
    };
    let metered = client.send_guarded(&req)?;

    match metered.value {
        Guarded::Skipped { state, .. } => {
            let state = if state.is_empty() { "unknown" } else { &state };
            println!("car is {state}; skipped the billed call");
            println!("spent: nothing (the state check is a free call)");
        }
        Guarded::Executed(data) => {
            let battery = data
                .charge_state
                .as_ref()
                .and_then(|c| c.get("battery_level"))
                .and_then(|v| v.as_u64());
            match battery {
                Some(level) => println!("battery {level}%"),
                None => println!("the response carried no charge_state.battery_level"),
            }
            println!("spent: one billed request ({price})");
        }
        _ => println!("unrecognised gate outcome"),
    }

    // The billed endpoints report their per-vehicle quota; free ones report none.
    if let Some(rl) = metered.rate_limit {
        println!("quota: {} of {} left", rl.remaining, rl.limit);
    }
    Ok(())
}
