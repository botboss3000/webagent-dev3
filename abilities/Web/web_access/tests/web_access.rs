//! Web access tests through the dispatcher and across both tools.
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
    assert!(maps_geocode::map_row(&json!({"lat":"oops","lon":"0"})).is_err());
    assert!(get_weather::weather_output(&json!({"current":{}}), "London").is_err());
}
