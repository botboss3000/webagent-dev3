//! Trusted, compiled Web Access adapter. Manifests never select executable code.
//! Fixed public providers only; no worker, credentials, proxy, redirects or retries.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 512 * 1024;
const USER_AGENT: &str = "WebAgent-v3/1.0 (+https://webagent.live)";

pub fn owns(name: &str) -> bool {
    matches!(name, "get_weather" | "maps_geocode")
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["get_weather", "maps_geocode"];
fn five() -> usize {
    5
}
fn metric() -> String {
    "metric".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Weather {
    location: String,
    #[serde(default = "metric")]
    units: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Maps {
    action: String,
    location: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    lat2: Option<f64>,
    lon2: Option<f64>,
    #[serde(default = "five")]
    limit: usize,
}
fn text(s: &str, max: usize) -> Result<()> {
    if s.trim().is_empty() || s.len() > max || s.chars().any(char::is_control) {
        bail!("Invalid text argument");
    }
    Ok(())
}
fn count(n: usize) -> Result<()> {
    if !(1..=10).contains(&n) {
        bail!("Result limit must be 1..10");
    }
    Ok(())
}
fn coords(lat: Option<f64>, lon: Option<f64>) -> Result<(f64, f64)> {
    let (Some(a), Some(b)) = (lat, lon) else {
        bail!("Coordinates required");
    };
    if !a.is_finite()
        || !b.is_finite()
        || !(-90.0..=90.0).contains(&a)
        || !(-180.0..=180.0).contains(&b)
    {
        bail!("Coordinates out of range");
    }
    Ok((a, b))
}
fn distance(a: (f64, f64), b: (f64, f64)) -> Value {
    let x = ((b.0 - a.0).to_radians() / 2.0).sin().powi(2)
        + a.0.to_radians().cos()
            * b.0.to_radians().cos()
            * ((b.1 - a.1).to_radians() / 2.0).sin().powi(2);
    let km = 2.0 * 6371.0088 * x.clamp(0.0, 1.0).sqrt().asin();
    json!({"km":(km*1000.0).round()/1000.0,"miles":(km*0.621371*1000.0).round()/1000.0,"method":"great_circle","provider":"local"})
}
fn client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .build()?;
    let _ = CLIENT.set(c);
    Ok(CLIENT.get().unwrap())
}
async fn body(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        bail!("Public provider HTTP {}", status.as_u16());
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        bail!("Provider response exceeds limit");
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_BODY {
            bail!("Provider response exceeds limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).context("Invalid provider text")
}
async fn get(url: &'static str, params: &[(&str, String)]) -> Result<Value> {
    let response = client()?
        .get(url)
        .query(params)
        .send()
        .await
        .context("Public provider unavailable")?;
    serde_json::from_str(&body(response).await?).context("Malformed provider response")
}
// Nominatim public service permits at most one request/sec. This process-wide gate
// covers single and parallel tool calls in the single v3 loop deployment.
async fn nominatim_slot() {
    static GATE: OnceLock<tokio::sync::Mutex<Option<tokio::time::Instant>>> = OnceLock::new();
    let mut last = GATE
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if let Some(previous) = *last {
        tokio::time::sleep_until(previous + Duration::from_secs(1)).await;
    }
    *last = Some(tokio::time::Instant::now());
}
async fn weather(args: Weather) -> Result<Value> {
    text(&args.location, 256)?;
    if !matches!(args.units.as_str(), "metric" | "imperial") {
        bail!("Invalid weather units");
    }
    let parts = args.location.split(',').collect::<Vec<_>>();
    let literal = if parts.len() == 2 {
        parts[0]
            .trim()
            .parse::<f64>()
            .ok()
            .zip(parts[1].trim().parse::<f64>().ok())
    } else {
        None
    };
    let (lat, lon) = if let Some((a, b)) = literal {
        coords(Some(a), Some(b))?
    } else {
        let v = get(
            "https://geocoding-api.open-meteo.com/v1/search",
            &[
                ("name", args.location.clone()),
                ("count", "1".into()),
                ("language", "en".into()),
                ("format", "json".into()),
            ],
        )
        .await?;
        let row = v["results"]
            .as_array()
            .and_then(|a| a.first())
            .context("Location not found")?;
        coords(row["latitude"].as_f64(), row["longitude"].as_f64())?
    };
    let imperial = args.units == "imperial";
    let v=get("https://api.open-meteo.com/v1/forecast",&[("latitude",lat.to_string()),("longitude",lon.to_string()),("current","temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m,wind_direction_10m".into()),("temperature_unit",if imperial {"fahrenheit"}else{"celsius"}.into()),("wind_speed_unit",if imperial {"mph"}else{"kmh"}.into()),("timezone","auto".into())]).await?;
    weather_output(&v, &args.location)
}
fn weather_output(v: &Value, location: &str) -> Result<Value> {
    let c = &v["current"];
    for key in [
        "temperature_2m",
        "relative_humidity_2m",
        "apparent_temperature",
        "weather_code",
        "wind_speed_10m",
        "wind_direction_10m",
    ] {
        if !c[key].is_number() {
            bail!("Incomplete weather response");
        }
    }
    if !c["time"].is_string() || !v["current_units"].is_object() {
        bail!("Incomplete weather metadata");
    }
    Ok(
        json!({"provider":"open_meteo","location":location,"current":c,"units":v["current_units"],"timezone":v["timezone"]}),
    )
}
fn map_row(row: &Value) -> Result<Value> {
    let number = |key: &str| {
        row[key]
            .as_f64()
            .or_else(|| row[key].as_str().and_then(|s| s.parse().ok()))
    };
    let (lat, lon) = coords(number("lat"), number("lon"))?;
    let name = row["display_name"]
        .as_str()
        .context("Incomplete geocoding response")?;
    Ok(
        json!({"display_name":name.chars().take(2000).collect::<String>(),"lat":lat,"lon":lon,"type":row["type"],"category":row["category"],"address":row["address"]}),
    )
}
async fn maps(args: Maps) -> Result<Value> {
    count(args.limit)?;
    match args.action.as_str() {
        "distance" => Ok(distance(
            coords(args.lat, args.lon)?,
            coords(args.lat2, args.lon2)?,
        )),
        "geocode" => {
            let location = args.location.context("Location required")?;
            text(&location, 256)?;
            nominatim_slot().await;
            let v = get(
                "https://nominatim.openstreetmap.org/search",
                &[
                    ("q", location.clone()),
                    ("format", "jsonv2".into()),
                    ("limit", args.limit.to_string()),
                ],
            )
            .await?;
            let rows = v
                .as_array()
                .context("Malformed geocoding response")?
                .iter()
                .take(args.limit)
                .map(map_row)
                .collect::<Result<Vec<_>>>()?;
            Ok(
                json!({"provider":"nominatim","query":location,"count":rows.len(),"results":rows,"attribution":"© OpenStreetMap contributors"}),
            )
        }
        "reverse" => {
            let (lat, lon) = coords(args.lat, args.lon)?;
            nominatim_slot().await;
            let v = get(
                "https://nominatim.openstreetmap.org/reverse",
                &[
                    ("lat", lat.to_string()),
                    ("lon", lon.to_string()),
                    ("format", "jsonv2".into()),
                ],
            )
            .await?;
            Ok(
                json!({"provider":"nominatim","result":map_row(&v)?,"attribution":"© OpenStreetMap contributors"}),
            )
        }
        _ => bail!("Unknown geocoding action"),
    }
}
pub async fn execute(name: &str, args: &Value, cancel: CancellationToken) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let work = async {
        match name {
            "get_weather" => {
                weather(serde_json::from_value(args.clone()).context("Invalid weather arguments")?)
                    .await
            }
            "maps_geocode" => {
                maps(serde_json::from_value(args.clone()).context("Invalid geocoding arguments")?)
                    .await
            }
            _ => bail!("Unknown web access tool"),
        }
    };
    let output = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=tokio::time::timeout(Duration::from_secs(35),work)=>match result {Ok(v)=>v,Err(_)=>Err(anyhow::anyhow!("Public provider timed out"))},
    };
    Ok(match output {
        Ok(value) => json!({"status":"success","output":value}),
        Err(error) => json!({"status":"error","output":error.to_string()}),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn distance_and_invalid_inputs() {
        let args = json!({"action":"distance","lat":0,"lon":0,"lat2":0,"lon2":1});
        let v = execute("maps_geocode", &args, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(v["status"], "success");
        assert_eq!(v["output"]["km"], 111.195);
        for args in [
            json!({"action":"distance","lat":91,"lon":0,"lat2":0,"lon2":1}),
            json!({"action":"distance"}),
            json!({"action":"geocode","location":"","limit":0}),
            json!({"action":"reverse","lat":0,"lon":0,"url":"http://localhost"}),
        ] {
            assert_eq!(
                execute("maps_geocode", &args, CancellationToken::new())
                    .await
                    .unwrap()["status"],
                "error"
            );
        }
        assert_eq!(
            execute(
                "get_weather",
                &json!({"location":"91,0"}),
                CancellationToken::new()
            )
            .await
            .unwrap()["status"],
            "error"
        );
        assert_eq!(
            execute(
                "get_weather",
                &json!({"location":"London","units":"bogus"}),
                CancellationToken::new()
            )
            .await
            .unwrap()["status"],
            "error"
        );
    }
    #[tokio::test]
    async fn cancelled_before_network() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            execute("get_weather", &json!({"location":"London"}), cancel)
                .await
                .is_err()
        );
    }
    #[test]
    fn provider_rows_fail_honestly() {
        assert!(map_row(&json!({"lat":"oops","lon":"0"})).is_err());
        assert!(weather_output(&json!({"current":{}}), "London").is_err());
    }
    #[test]
    fn antipodal_distance_is_finite() {
        assert!(distance((0.0, 0.0), (0.0, 180.0))["km"]
            .as_f64()
            .unwrap()
            .is_finite());
    }
}
