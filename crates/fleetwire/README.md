# fleetwire

An **unofficial, read-only** Rust client for the Tesla Fleet API that knows which calls cost money.

> **Not affiliated with, endorsed by, or sponsored by Tesla, Inc.**
> Tesla names and trademarks belong to their respective owners.

- **Read-only.** GET requests only. There is no vehicle-command or signing code, and the endpoint trait is
  sealed, so a caller cannot add a write.
- **Cost-aware.** Every request type says whether Tesla bills it (`Pricing`). Billed calls are never
  retried or re-sent by the recovery paths, and `Client::send_guarded` checks for free whether the car is
  online before spending anything.
- **Synchronous.** Built on `ureq` with rustls. No async runtime needed.
- **Careful with tokens.** `TokenManager` keeps tokens in an owner-only directory at `0600`, persists a
  rotated refresh token before returning, and collapses concurrent refreshes within and across processes.

## Quick start

You need a Tesla developer app (its **client id**) and a **refresh token** for your account. Tesla's
[Fleet API docs](https://developer.tesla.com/docs/fleet-api) cover app registration and the
authorization-code flow that yields the refresh token.

1. Put the refresh token where only you can read it:

   ```sh
   mkdir -m 700 ~/.fleetwire
   (umask 077 && cat > ~/.fleetwire/tokens.json <<'EOF'
   {"access_token": "", "refresh_token": "PASTE_YOUR_REFRESH_TOKEN", "expires_at": 0}
   EOF
   )
   ```

   `expires_at: 0` makes the first call refresh the access token. fleetwire rewrites this file each time
   Tesla rotates the refresh token, so after this step you never edit it by hand. The directory must not
   be accessible to other users, or the client refuses to read it.

2. List your vehicles. This call is free and does not wake the car:

   ```sh
   TESLA_CLIENT_ID=your-client-id FLEETWIRE_TOKEN_DIR=$HOME/.fleetwire \
     cargo run --example list_vehicles
   ```

   ```text
   1 vehicle(s)
   - vin LRW**********1234 · state asleep
   ```

3. Use it from your code:

   ```rust,no_run
   use std::sync::Arc;
   use fleetwire::api::vehicles::ListVehicles;
   use fleetwire::{Client, HttpRefresher, TokenManager};

   # fn main() -> Result<(), Box<dyn std::error::Error>> {
   let tokens = TokenManager::new(
       "/home/me/.fleetwire".into(),
       Arc::new(HttpRefresher::new("your-client-id".to_owned())),
   );
   let client = Client::builder(tokens).build()?;
   let page = client.send(&ListVehicles { page: None, per_page: None })?;
   println!("{} vehicle(s)", page.items.len());
   # Ok(())
   # }
   ```

## Calls that cost money

Of the sixteen endpoints this crate models, two are billed: `GetVehicleData` and
`GetNearbyChargingSites` (measured: $0.002 per request, and a 408 from a sleeping vehicle is billed too).
The other fourteen are not.

| Entry point | What it does |
|---|---|
| `Client::send` | The typed response. |
| `Client::send_guarded` | Checks the car's state with a free call first and only makes the billed call when it is online; otherwise reports `Guarded::Skipped` and spends nothing. |
| `Client::send_metered` | The typed response plus the `ratelimit-*` quota the billed endpoints report. |
| `Client::send_raw` | ⚠️ The response body **verbatim and unscrubbed** (VIN, coordinates, odometer) as one `serde_json::Value`, for observing schema drift. Treat it as personal data. |

`max_attempts` defaults to `1`, and the 401/412 recovery paths never re-send a request to a billed
endpoint. `send_guarded` narrows the window but cannot close it: the car can fall asleep between the free
check and the billed call.

## Status

Early release. The API may change before 1.0.

The sixteen modelled GET endpoints (12 vehicle, 4 user) were verified against live responses. Not
implemented: vehicle commands and anything that writes — this crate has no POST or signing code and is
not a route to one — and the charging, energy and partner endpoints, which are left out until a live
response can verify their shape.

A custom `TokenRefresher` must bound its own request time: `TokenManager` holds a cross-process file lock
while a refresh runs.

## Minimum Supported Rust Version (MSRV)

Rust `1.89.0` (edition 2024 floor). The MSRV may rise only in a documented minor release.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you shall be dual licensed as above,
without any additional terms or conditions.
