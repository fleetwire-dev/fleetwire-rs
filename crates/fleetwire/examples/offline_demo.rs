//! Shows what the client refuses to do, against a mock Fleet API on a loopback port.
//!
//! No Tesla account, no token and no outside connection are needed:
//!
//! ```text
//! cargo run --example offline_demo
//! ```
//!
//! Each scene starts its own mock server, drives the real `Client` against it, and then asks
//! the server what it actually received. The claims are about request counts, so counting on
//! the server side is the proof. The process exits non-zero if any claim does not hold, so it
//! can be run as a check (the public repository's CI does).
//!
//! The tokens are unsigned, made-up JWTs and the VIN is a placeholder. The mock server is
//! `fleetwire-testkit`, the same one the crate's tests use.

use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fleetwire::api::vehicles::{DataEndpoint, GetVehicleData, ListVehicles};
use fleetwire::{
    Billing, Client, Error, FleetGet, Guarded, Pricing, Refreshed, TokenManager, TokenRefresher,
};
use fleetwire_testkit::matchers::{header, method, path};
use fleetwire_testkit::{Mock, MockServer, Request, ResponseTemplate};

/// A made-up VIN.
const VIN: &str = "5YJ3E1EA1JF000001";
const READ_SCOPES: &[&str] = &["openid", "offline_access", "vehicle_device_data"];
/// What the mock's region endpoint answers by default: a host that is not on the allow-list.
const UNKNOWN_REGION: &str = "https://fleet-api.example.invalid";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    match run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(failed) => {
            eprintln!("{failed} claim(s) did not hold");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Res<u32> {
    let mut report = Report::default();
    println!("Each scene drives the real client against its own mock server on a loopback port.");
    println!(
        "\"server saw\" lists what that mock received; the claims under it are checked against it."
    );
    println!(
        "(A refused region answer is not cached, so the region lookup repeats on every call here."
    );
    println!(" The mock's answer is refused on purpose; scene 7 looks at that directly.)");
    sleeping_car_is_not_billed(&mut report)?;
    online_car_is_billed_once(&mut report)?;
    a_408_is_billed_and_not_retried(&mut report)?;
    a_billed_401_is_not_sent_again(&mut report)?;
    a_free_401_is_retried_once(&mut report)?;
    a_command_scope_is_refused(&mut report)?;
    an_untrusted_region_is_refused(&mut report)?;
    secrets_are_redacted(&mut report)?;
    println!(
        "\n{} of {} claims held",
        report.held,
        report.held + report.failed
    );
    Ok(report.failed)
}

// --- Scenes ----------------------------------------------------------------------------------

/// `send_guarded` asks a free endpoint for the car's state and skips the billed call unless
/// the car is online.
fn sleeping_car_is_not_billed(report: &mut Report) -> Res<()> {
    report.scene("A sleeping car: the guarded call spends nothing");
    let demo = Demo::start(READ_SCOPES)?;
    demo.vehicle_state("asleep");
    demo.vehicle_data_408();

    let req = GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState])?;
    let got = demo.client.send_guarded(&req)?;
    report.seen(&demo)?;

    let skipped = matches!(&got.value, Guarded::Skipped { state, .. } if state == "asleep");
    report.claim(
        "the guard reports the car as asleep and skips the call",
        skipped,
    );
    report.claim(
        "billed requests the server received: 0",
        demo.billed_requests()? == 0,
    );
    Ok(())
}

fn online_car_is_billed_once(report: &mut Report) -> Res<()> {
    report.scene("An online car: one billed request, and the quota comes back with it");
    let demo = Demo::start(READ_SCOPES)?;
    demo.vehicle_state("online");
    demo.vehicle_data_ok();

    let req = GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState])?;
    report.note(&format!("this request is tagged {}", price_of(&req)));
    let got = demo.client.send_guarded(&req)?;
    report.seen(&demo)?;

    let battery = match &got.value {
        Guarded::Executed(data) => data
            .charge_state
            .as_ref()
            .and_then(|c| c.get("battery_level"))
            .and_then(serde_json::Value::as_u64),
        _ => None,
    };
    report.claim("the billed call was made: battery 71%", battery == Some(71));
    report.claim(
        "billed requests the server received: 1",
        demo.billed_requests()? == 1,
    );
    let quota = got.rate_limit.map(|rl| (rl.remaining, rl.limit));
    report.claim(
        "the per-vehicle quota is reported: 59 of 60 left",
        quota == Some((59, 60)),
    );
    Ok(())
}

/// Tesla bills the 408 a sleeping car returns. Sending the request again would bill it again.
fn a_408_is_billed_and_not_retried(report: &mut Report) -> Res<()> {
    report
        .scene("An unguarded call to a sleeping car: billed, and not retried even with retries on");
    // With the default of one attempt nothing is ever retried, and this scene could not fail.
    // So the caller opts in to three attempts here: a 408 must still go out once.
    let demo = Demo::start_with(READ_SCOPES, UNKNOWN_REGION, 3)?;
    demo.vehicle_data_408();

    let req = GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState])?;
    let outcome = demo.client.send(&req);
    report.seen(&demo)?;

    let err = outcome.err();
    report.claim(
        "the call fails with VehicleOffline",
        matches!(err, Some(Error::VehicleOffline { .. })),
    );
    report.claim(
        "the error says the call was billed",
        err.as_ref().map(Error::billing) == Some(Billing::Billed),
    );
    report.note("the client was built with max_attempts(3)");
    report.claim(
        "billed requests the server received: 1 (a 408 is not retryable)",
        demo.billed_requests()? == 1,
    );
    Ok(())
}

/// A 401 on a billed endpoint is an answered, billed request. The client repairs the token
/// for the next call but does not send this one again.
fn a_billed_401_is_not_sent_again(report: &mut Report) -> Res<()> {
    report.scene("A 401 on a billed endpoint: the token is repaired, the request is not re-sent");
    let demo = Demo::start(READ_SCOPES)?;
    // Only the first token is rejected. A request that carries any other token succeeds, so the
    // second call can succeed only if the token really was rotated.
    demo.reject_token(&demo.vehicle_data_path(), &demo.first_token);
    demo.vehicle_data_ok();

    let req = GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState])?;
    let first = demo.client.send(&req).err();
    report.seen(&demo)?;
    report.claim(
        "the first call returns the 401 as an Auth error",
        matches!(first, Some(Error::Auth { .. })),
    );
    report.claim(
        "the error says the call was billed",
        first.as_ref().map(Error::billing) == Some(Billing::Billed),
    );
    report.claim(
        "billed requests after the first call: 1 (not sent again)",
        demo.billed_requests()? == 1,
    );

    let second = demo.client.send(&req);
    report.seen(&demo)?;
    report.claim("the next call succeeds", second.is_ok());
    report.claim(
        "billed requests in total: 2 (one per call)",
        demo.billed_requests()? == 2,
    );
    // The server's own record of the two billed requests: the first carried the token that was
    // rejected, the second exactly the token the stand-in token endpoint issued afterwards.
    let bearers = demo.bearers_at(&demo.vehicle_data_path())?;
    let rejected = format!("Bearer {}", demo.first_token);
    let reissued = demo.issued().first().map(|t| format!("Bearer {t}"));
    report.claim(
        "the server saw the rejected token once, then the newly issued one",
        matches!(
            (bearers.as_slice(), &reissued),
            ([Some(a), Some(b)], Some(new)) if *a == rejected && b == new && b != a
        ),
    );
    report.note("the refresh below is counted by this example's stand-in for the token endpoint");
    report.claim("token refreshes: 1", demo.refreshes() == 1);
    Ok(())
}

/// The contrast: a free request costs nothing to repeat, so one call recovers by itself.
fn a_free_401_is_retried_once(report: &mut Report) -> Res<()> {
    report.scene("The same 401 on a free endpoint: one call recovers by itself");
    let demo = Demo::start(READ_SCOPES)?;
    demo.reject_token("/api/1/vehicles", &demo.first_token);
    demo.json("/api/1/vehicles", 200, r#"{"response":[]}"#);

    let req = ListVehicles {
        page: None,
        per_page: None,
    };
    report.note(&format!("this request is tagged {}", price_of(&req)));
    let outcome = demo.client.send(&req);
    report.seen(&demo)?;

    report.claim("the call succeeds", outcome.is_ok());
    report.claim(
        "requests the server received: 2 (the original and one retry after a refresh)",
        demo.requests_to("/api/1/vehicles")? == 2 && demo.refreshes() == 1,
    );
    Ok(())
}

/// A token that can command the car is not used for reading. Nothing is sent.
fn a_command_scope_is_refused(report: &mut Report) -> Res<()> {
    report.scene("A token that carries a command scope: refused before anything is sent");
    let demo = Demo::start(&["openid", "vehicle_device_data", "vehicle_cmds"])?;
    demo.json("/api/1/vehicles", 200, r#"{"response":[]}"#);

    let outcome = demo.client.send(&ListVehicles {
        page: None,
        per_page: None,
    });
    report.seen(&demo)?;

    report.claim(
        "the call fails with CommandScopeOnReadToken",
        matches!(outcome.err(), Some(Error::CommandScopeOnReadToken)),
    );
    report.claim(
        "requests of any kind the server received: 0",
        demo.all_requests()? == 0,
    );
    Ok(())
}

/// The region endpoint's answer decides where the bearer token goes next, so it is adopted
/// only when its host is on the allow-list.
fn an_untrusted_region_is_refused(report: &mut Report) -> Res<()> {
    report.scene("A region answer that names another server: not adopted");
    // A second local server. The region answer points at it; it must never hear from the client.
    let decoy = MockServer::start();
    let demo = Demo::start_with(READ_SCOPES, &decoy.uri(), 1)?;
    demo.json("/api/1/vehicles", 200, r#"{"response":[]}"#);

    let outcome = demo.client.send(&ListVehicles {
        page: None,
        per_page: None,
    });
    report.seen(&demo)?;

    report
        .note("the region answer names a second local server (plain http, not on the allow-list)");
    report.claim(
        "the client asked for the region",
        demo.requests_to("/api/1/users/region")? >= 1,
    );
    report.claim(
        "the second server received nothing",
        matches!(decoy.received_requests().as_deref(), Some([])),
    );
    report.claim(
        "the request went to the configured host, and succeeded",
        outcome.is_ok() && demo.requests_to("/api/1/vehicles")? == 1,
    );
    Ok(())
}

/// Whatever a server puts in an error body, the error the caller sees is scrubbed.
fn secrets_are_redacted(report: &mut Report) -> Res<()> {
    report.scene("An error body with a token, a VIN and coordinates in it: scrubbed");
    let demo = Demo::start(READ_SCOPES)?;
    let leaked_token = fake_jwt(&["openid"]);
    let body = serde_json::json!({
        "error": format!(
            "upstream failed for vin {VIN} at 36.512345,128.512345 with Bearer {leaked_token}"
        ),
        "access_token": leaked_token,
        "latitude": 36.512345,
        "longitude": 128.512345,
    });
    demo.json("/api/1/vehicles", 500, &body.to_string());

    let outcome = demo.client.send(&ListVehicles {
        page: None,
        per_page: None,
    });
    report.seen(&demo)?;
    let Some(err) = outcome.err() else {
        report.claim("the call fails with the server's 500", false);
        return Ok(());
    };
    let shown = format!("{err} / {err:?}");
    report.note(&format!("the caller sees: {err}"));
    // Whole values and telling fragments: the token's payload segment, the integer and first
    // decimals of each coordinate, the serial part of the VIN.
    let payload = leaked_token.split('.').nth(1).unwrap_or_default();
    let traces = [
        VIN,
        "JF000001",
        "36.512",
        "128.512",
        leaked_token.as_str(),
        payload,
    ];
    let leaked: Vec<&str> = traces
        .iter()
        .copied()
        .filter(|t| !t.is_empty() && shown.contains(t))
        .collect();
    report.claim(
        "neither Display nor Debug shows the VIN, the coordinates or the token, whole or in part",
        !payload.is_empty() && leaked.is_empty(),
    );
    Ok(())
}

// --- The stage: one mock server, one token directory, one client ------------------------------

struct Demo {
    server: MockServer,
    client: Client,
    first_token: String,
    refreshes: Arc<AtomicUsize>,
    issued: Arc<Mutex<Vec<String>>>,
    _dir: TokenDir,
}

impl Demo {
    /// Starts a mock server and a client whose token carries `scopes`.
    ///
    /// The region answer names a host that is not on the allow-list, so the client stays on the
    /// mock. `an_untrusted_region_is_refused` is the scene that looks at this directly.
    fn start(scopes: &[&str]) -> Res<Self> {
        Self::start_with(scopes, UNKNOWN_REGION, 1)
    }

    /// The same, with the region endpoint answering that the account lives at `region_url`, and
    /// the client allowed `max_attempts` attempts per call (the crate's default is one).
    fn start_with(scopes: &[&str], region_url: &str, max_attempts: u32) -> Res<Self> {
        let server = MockServer::start();
        let region = serde_json::json!({
            "response": { "region": "na", "fleet_api_base_url": region_url }
        });
        mount(&server, "/api/1/users/region", 200, &region.to_string());

        let first_token = fake_jwt(scopes);
        let dir = TokenDir::create()?;
        dir.seed(&first_token)?;
        let refreshes = Arc::new(AtomicUsize::new(0));
        let issued = Arc::new(Mutex::new(Vec::new()));
        let refresher = Rotating {
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            calls: Arc::clone(&refreshes),
            issued: Arc::clone(&issued),
        };
        let tokens = TokenManager::new(dir.0.clone(), Arc::new(refresher));
        let client = Client::builder(tokens)
            .bootstrap_region(fleetwire::Url::parse(&server.uri())?)
            .max_attempts(max_attempts)
            .build()?;
        Ok(Self {
            server,
            client,
            first_token,
            refreshes,
            issued,
            _dir: dir,
        })
    }

    fn vehicle_data_path(&self) -> String {
        format!("/api/1/vehicles/{VIN}/vehicle_data")
    }

    fn json(&self, at: &str, status: u16, body: &str) {
        mount(&self.server, at, status, body);
    }

    /// The free endpoint the guard asks first.
    fn vehicle_state(&self, state: &str) {
        let body = format!(r#"{{"response":{{"id":1,"vin":"{VIN}","state":"{state}"}}}}"#);
        self.json(&format!("/api/1/vehicles/{VIN}"), 200, &body);
    }

    /// The billed endpoint, answering as it does for an awake car.
    fn vehicle_data_ok(&self) {
        let body =
            format!(r#"{{"response":{{"vin":"{VIN}","charge_state":{{"battery_level":71}}}}}}"#);
        Mock::given(method("GET"))
            .and(path(self.vehicle_data_path()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.into_bytes(), "application/json")
                    .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                    .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                    .insert_header("ratelimit-device-data-per-vehicle-reset", "57"),
            )
            .mount(&self.server);
    }

    /// The billed endpoint, answering as it does for a sleeping car (shape measured live).
    fn vehicle_data_408(&self) {
        let body = r#"{"response":null,"error":"vehicle unavailable: vehicle is offline or asleep","error_description":""}"#;
        self.json(&self.vehicle_data_path(), 408, body);
    }

    /// Answers 401 to requests at `at` that carry exactly `token`. Mount it before the
    /// catch-all answer for the same path: the first mock that matches wins.
    fn reject_token(&self, at: &str, token: &str) {
        Mock::given(method("GET"))
            .and(path(at))
            .and(header("authorization", format!("Bearer {token}")))
            .respond_with(ResponseTemplate::new(401).set_body_raw(
                br#"{"error":"invalid authentication"}"#.to_vec(),
                "application/json",
            ))
            .mount(&self.server);
    }

    /// The mock server's journal. A server that kept none is an error, never "nothing received":
    /// every claim in this example is only as good as this record.
    fn received(&self) -> Res<Vec<Request>> {
        self.server
            .received_requests()
            .ok_or_else(|| "the mock server kept no request journal".into())
    }

    fn requests_to(&self, at: &str) -> Res<usize> {
        Ok(self
            .received()?
            .iter()
            .filter(|r| r.url.path() == at)
            .count())
    }

    /// The `Authorization` header of each request the server received at `at`, in order.
    /// `None` for a request that carried no such header: absent is not "a different token".
    fn bearers_at(&self, at: &str) -> Res<Vec<Option<String>>> {
        Ok(self
            .received()?
            .iter()
            .filter(|r| r.url.path() == at)
            .map(|r| r.header("authorization").map(str::to_owned))
            .collect())
    }

    fn billed_requests(&self) -> Res<usize> {
        self.requests_to(&self.vehicle_data_path())
    }

    /// Everything the server received so far, in order of first arrival, with the VIN masked.
    fn seen(&self) -> Res<String> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for r in self.received()? {
            let label = format!("{} {}", r.method, r.url.path().replace(VIN, "{vin}"));
            match seen.iter_mut().find(|(l, _)| *l == label) {
                Some((_, n)) => *n += 1,
                None => seen.push((label, 1)),
            }
        }
        if seen.is_empty() {
            return Ok("nothing".to_owned());
        }
        let parts: Vec<String> = seen
            .iter()
            .map(|(label, n)| match n {
                1 => label.clone(),
                _ => format!("{label} (x{n})"),
            })
            .collect();
        Ok(parts.join(", "))
    }

    fn all_requests(&self) -> Res<usize> {
        Ok(self.received()?.len())
    }

    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    /// The access tokens the stand-in token endpoint has issued so far.
    fn issued(&self) -> Vec<String> {
        self.issued.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

fn mount(server: &MockServer, at: &str, status: u16, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_raw(body.as_bytes().to_vec(), "application/json"),
        )
        .mount(server);
}

fn price_of(req: &impl FleetGet) -> String {
    match req.pricing() {
        Pricing::Data { usd_per_request } => format!("billed, ${usd_per_request} per request"),
        _ => "free".to_owned(),
    }
}

/// Stands in for the token endpoint. Each refresh returns a token with different bytes, so a
/// retry that carries the rejected token again would be visible on the server.
struct Rotating {
    scopes: Vec<String>,
    calls: Arc<AtomicUsize>,
    /// Every access token this stand-in has handed out, in order.
    issued: Arc<Mutex<Vec<String>>>,
}

impl TokenRefresher for Rotating {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let scopes: Vec<&str> = self.scopes.iter().map(String::as_str).collect();
        let token = fake_jwt_numbered(&scopes, n);
        if let Ok(mut issued) = self.issued.lock() {
            issued.push(token.clone());
        }
        Ok(Refreshed::new(token, format!("demo-refresh-{n}"), 28_800))
    }
}

/// An unsigned JWT with the given `scp` claim. The client reads the claim without verifying
/// the signature; it is a pre-check that saves a request, and the server stays the authority.
fn fake_jwt(scopes: &[&str]) -> String {
    fake_jwt_numbered(scopes, 0)
}

fn fake_jwt_numbered(scopes: &[&str], n: usize) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = serde_json::json!({ "scp": scopes, "exp": 9_999_999_999u64, "demo": n });
    format!(
        "{}.{}.unsigned",
        b64.encode(br#"{"alg":"none","typ":"JWT"}"#),
        b64.encode(payload.to_string()),
    )
}

/// An owner-only token directory under the system temp directory, removed on drop.
///
/// The client refuses a token directory or file that anyone else can read, so the modes are
/// set explicitly here rather than left to the process umask.
struct TokenDir(PathBuf);

impl TokenDir {
    fn create() -> std::io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("fleetwire-offline-demo-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        set_mode(&dir, 0o700)?;
        Ok(Self(dir))
    }

    /// Writes a token file that is valid for a long time, so no scene starts with a refresh.
    fn seed(&self, access_token: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        let file = self.0.join("tokens.json");
        let body = serde_json::json!({
            "access_token": access_token,
            "refresh_token": "demo-refresh-0",
            "expires_at": 4_000_000_000u64,
        });
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)?;
        f.write_all(body.to_string().as_bytes())?;
        set_mode(&file, 0o600)
    }
}

impl Drop for TokenDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn set_mode(at: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(mode))
}

// --- Output ----------------------------------------------------------------------------------

#[derive(Default)]
struct Report {
    scenes: u32,
    held: u32,
    failed: u32,
}

impl Report {
    fn scene(&mut self, title: &str) {
        self.scenes += 1;
        println!("\n{}. {title}", self.scenes);
    }

    /// Prints what the mock server has received in this scene so far: the evidence the
    /// claims below are checked against.
    fn seen(&self, demo: &Demo) -> Res<()> {
        println!("   server saw: {}", demo.seen()?);
        Ok(())
    }

    fn note(&self, text: &str) {
        println!("   .. {text}");
    }

    fn claim(&mut self, what: &str, holds: bool) {
        if holds {
            self.held += 1;
            println!("   ok {what}");
        } else {
            self.failed += 1;
            println!("   FAILED {what}");
        }
    }
}
