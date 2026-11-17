//! maps_geocode: Nominatim geocoding and reverse lookup, plus local great-circle distance.
use super::{coords, get, text};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};

fn five() -> usize {
    5
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Maps {
    action: String,
    location: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    lat2: Option<f64>,
    lon2: Option<f64>,
    #[serde(default = "five")]
    limit: usize,
}
fn count(n: usize) -> Result<()> {
    if !(1..=10).contains(&n) {
        bail!("Result limit must be 1..10");
    }
    Ok(())
}
fn distance(a: (f64, f64), b: (f64, f64)) -> Value {
    let x = ((b.0 - a.0).to_radians() / 2.0).sin().powi(2)
        + a.0.to_radians().cos()
            * b.0.to_radians().cos()
            * ((b.1 - a.1).to_radians() / 2.0).sin().powi(2);
    let km = 2.0 * 6371.0088 * x.clamp(0.0, 1.0).sqrt().asin();
    json!({"km":(km*1000.0).round()/1000.0,"miles":(km*0.621371*1000.0).round()/1000.0,"method":"great_circle","provider":"local"})
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
pub(super) fn map_row(row: &Value) -> Result<Value> {
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
pub(super) async fn maps(args: Maps) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn antipodal_distance_is_finite() {
        assert!(distance((0.0, 0.0), (0.0, 180.0))["km"]
            .as_f64()
            .unwrap()
            .is_finite());
    }
}
