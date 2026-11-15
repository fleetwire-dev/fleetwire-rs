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
use fleetwire::{Client, Guarded, HttpRefresher, TokenManager};

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
    let vin = std::env::args()
        .nth(1)
        .ok_or("pass the vehicle's VIN as the first argument")?;

    let tokens = TokenManager::new(dir, Arc::new(HttpRefresher::new(client_id)));
    let client = Client::builder(tokens).build()?;

    // Ask only for charge_state: no location, so the `vehicle_location` scope is not needed.
    let req = GetVehicleData::new(vin, vec![DataEndpoint::ChargeState])?;
    let metered = client.send_guarded(&req)?;

    match metered.value {
        Guarded::Skipped { state, .. } => {
            let state = if state.is_empty() { "unknown" } else { &state };
            println!("car is {state}; skipped the billed call, nothing spent");
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
        }
        _ => println!("unrecognised gate outcome"),
    }

    // The billed endpoints report their per-vehicle quota; free ones report none.
    if let Some(rl) = metered.rate_limit {
        println!("quota: {} of {} left", rl.remaining, rl.limit);
    }
    Ok(())
}
