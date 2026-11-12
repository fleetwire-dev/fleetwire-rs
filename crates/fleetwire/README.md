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

**Before you start** you need two things from Tesla, and this crate does not obtain them for you:

- a Tesla developer app — its **client id**;
- a **refresh token** for your account, issued to that app with the scopes `openid`, `offline_access`
  and `vehicle_device_data`.

Tesla's [Fleet API docs](https://developer.tesla.com/docs/fleet-api) cover app registration and the
authorization-code flow that yields the refresh token. fleetwire takes over from there: it refreshes
access tokens, but it does not run the authorization-code exchange. Expect this part to take longer than
everything below.

The crate builds on Linux and macOS (CI runs on Linux).

1. Get the code. The example lives in the repository, so clone it; the commands below work from
   anywhere inside the clone.

   ```sh
   git clone https://github.com/fleetwire-dev/fleetwire-rs
   cd fleetwire-rs
   ```

2. Put the refresh token where only you can read it:

   ```sh
   mkdir -p ~/.fleetwire && chmod 700 ~/.fleetwire
   (umask 077 && cat > ~/.fleetwire/tokens.json <<'EOF'
   {"access_token": "", "refresh_token": "PASTE_YOUR_REFRESH_TOKEN", "expires_at": 0}
   EOF
   )
   ```

   - `expires_at` is a Unix time in seconds; `0` means "already expired", so the first call refreshes
     the access token and fills in `access_token`.
   - ⚠️ Do this **once**. fleetwire rewrites this file every time Tesla rotates the refresh token, and
     the newest refresh token exists only there. Running the command again over a working directory
     overwrites it with the old token you pasted.
   - The directory must not be accessible to other users, or the client refuses to read it.
   - A `tokens.lock` file appears next to it. It is the cross-process lock; leave it.

3. List your vehicles. This call is free and does not wake the car:

   ```sh
   TESLA_CLIENT_ID=your-client-id FLEETWIRE_TOKEN_DIR=$HOME/.fleetwire \
     cargo run --example list_vehicles
   ```

   ```text
   1 vehicle(s)
   - vin LRW**********1234 · state asleep
   ```

   The example masks the VIN; add `-- --show-vin` to print it in full. `TESLA_CLIENT_ID` and
   `FLEETWIRE_TOKEN_DIR` are read by the example only — the library takes both as arguments.

### If it fails

| You see | Usual cause |
|---|---|
| `set TESLA_CLIENT_ID …` / `set FLEETWIRE_TOKEN_DIR …` | The environment variable is missing. |
| `token refresh failed: HTTP 400 … client_not_found` | The client id is wrong, or belongs to another app. |
| Any other `token refresh failed: HTTP 4xx …` | The refresh token is mistyped, expired, already rotated, or was issued to a different client id. Tesla's own message follows the status. |
| `token directory permissions are too permissive` | The directory or `tokens.json` is readable by others: `chmod 700 ~/.fleetwire && chmod 600 ~/.fleetwire/tokens.json`. |
| `token file not found` | `FLEETWIRE_TOKEN_DIR` points at a directory without `tokens.json` (step 2). |
| A scope error before any request is sent | The token lacks `vehicle_device_data`; re-authorize the app with that scope. |

4. Use it from your code. Add the dependency (`cargo add fleetwire`, once published — until then, a
   `path` or `git` dependency on this repository), then:

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
