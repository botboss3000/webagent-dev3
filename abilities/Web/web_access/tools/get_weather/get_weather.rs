//! get_weather: current conditions from Open-Meteo for a place or coordinates.
use super::{coords, get, text};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

fn metric() -> String {
    "metric".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Weather {
    location: String,
    #[serde(default = "metric")]
    units: String,
}
pub(super) async fn weather(args: Weather) -> Result<Value> {
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
pub(super) fn weather_output(v: &Value, location: &str) -> Result<Value> {
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
