//! Prints which of the sixteen modelled endpoints Tesla bills, without making a request.
//!
//! Every request type carries its own `Pricing`, so the table below is read from the types
//! themselves — no network, no token, no car. Run it to see what a call would cost before
//! you write the code that makes it:
//!
//! ```text
//! cargo run --example pricing_table
//! ```
//!
//! The VIN used to build the vehicle requests is a placeholder; nothing is sent anywhere.

use std::process::ExitCode;

use fleetwire::api::users::{GetFeatureConfig, GetMe, GetRegion, ListOrders};
use fleetwire::api::vehicles::{
    DataEndpoint, GetFleetTelemetryConfig, GetFleetTelemetryErrors, GetMobileEnabled,
    GetNearbyChargingSites, GetRecentAlerts, GetReleaseNotes, GetServiceData, GetVehicle,
    GetVehicleData, ListDrivers, ListInvitations, ListVehicles,
};
use fleetwire::{FleetGet, Pricing, Scope, ScopeExpr};

/// A made-up VIN. The requests are only built, never sent.
const PLACEHOLDER_VIN: &str = "00000000000000000";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let vin = PLACEHOLDER_VIN;
    let mut rows = vec![
        row(&ListVehicles {
            page: None,
            per_page: None,
        }),
        row(&GetVehicle::new(vin)?),
        row(&GetVehicleData::new(vin, vec![DataEndpoint::ChargeState])?),
        row(&GetNearbyChargingSites::new(vin, None, None, None)?),
        row(&ListInvitations::new(vin, None, None)?),
        row(&GetMobileEnabled::new(vin)?),
        row(&GetRecentAlerts::new(vin)?),
        row(&GetReleaseNotes::new(vin)?),
        row(&GetServiceData::new(vin)?),
        row(&ListDrivers::new(vin)?),
        row(&GetFleetTelemetryConfig::new(vin)?),
        row(&GetFleetTelemetryErrors::new(vin)?),
        row(&GetRegion),
        row(&GetMe),
        row(&ListOrders),
        row(&GetFeatureConfig),
    ];
    // Billed calls first, so they are the first thing a reader sees.
    rows.sort_by_key(|r| !matches!(r.cost, Cost::Billed(_)));

    let width = rows.iter().map(|r| r.path.len()).max().unwrap_or(0);
    println!("{:<width$}  {:<12}  scopes", "GET", "cost");
    for r in &rows {
        let cost = match r.cost {
            Cost::Billed(usd) => format!("${usd}/call"),
            Cost::Free => "free".to_owned(),
            Cost::Unknown => "unknown".to_owned(),
        };
        println!("{:<width$}  {cost:<12}  {}", r.path, r.scopes);
    }

    let billed = rows
        .iter()
        .filter(|r| matches!(r.cost, Cost::Billed(_)))
        .count();
    let free = rows.iter().filter(|r| matches!(r.cost, Cost::Free)).count();
    println!();
    println!(
        "{billed} of {} endpoints are billed; {free} are free.",
        rows.len()
    );
    println!(
        "Use Client::send_guarded for the billed ones: it checks for free that the car is online first."
    );
    println!(
        "These are the prices the request types carry, read without any network call; they are not a live tariff."
    );
    Ok(())
}

struct Row {
    path: String,
    cost: Cost,
    scopes: String,
}

enum Cost {
    Billed(f64),
    Free,
    Unknown,
}

fn row<R: FleetGet>(req: &R) -> Row {
    let cost = match req.pricing() {
        Pricing::Data { usd_per_request } => Cost::Billed(usd_per_request),
        Pricing::NotCharged => Cost::Free,
        // `Pricing` is non-exhaustive; a category this example does not know is not shown as free.
        _ => Cost::Unknown,
    };
    Row {
        // Show the placeholder segment as `{vin}`, the way Tesla's docs write it.
        path: req.path().replace(PLACEHOLDER_VIN, "{vin}"),
        cost,
        scopes: scopes(&req.required_scopes()),
    }
}

/// Renders a scope expression with Tesla's scope names, e.g. `vehicle_device_data`.
fn scopes(expr: &ScopeExpr) -> String {
    let join = |v: &[ScopeExpr], sep: &str| -> String {
        v.iter().map(scopes).collect::<Vec<_>>().join(sep)
    };
    match expr {
        ScopeExpr::One(s) => scope_name(*s).to_owned(),
        ScopeExpr::All(v) => join(v, " + "),
        ScopeExpr::Any(v) => format!("any of ({})", join(v, ", ")),
        _ => "?".to_owned(),
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::VehicleDeviceData => "vehicle_device_data",
        Scope::VehicleLocation => "vehicle_location",
        Scope::UserData => "user_data",
        Scope::Openid => "openid",
        _ => "?",
    }
}
