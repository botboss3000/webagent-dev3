//! Core utilities dispatch and clock tests covering get_time and get_date.
use super::*;
#[test]
fn clock_handles_offsets_leap_days_and_limits() {
    assert_eq!(
        clock("get_date", &json!({"format":"full"}), 0).unwrap()["text"],
        "Thursday, January 1, 1970"
    );
    assert_eq!(
        clock(
            "get_date",
            &json!({"format":"short","timezone":"UTC-01:00"}),
            0
        )
        .unwrap()["text"],
        "1969-12-31"
    );
    assert_eq!(
        clock(
            "get_date",
            &json!({"format":"iso","timezone":"UTC+14:00"}),
            951782400
        )
        .unwrap()["text"],
        "2000-02-29T14:00:00+14:00"
    );
    assert_eq!(
        clock("get_time", &json!({"timezone":"UTC+05:30"}), 0).unwrap()["text"],
        "05:30:00"
    );
    assert_eq!(
        clock("get_date", &json!({"format":"short"}), 253402300799).unwrap()["text"],
        "9999-12-31"
    );
    for zone in [
        "America/Detroit",
        "UTC+14:01",
        "UTC+00:60",
        "UTC+١٠:00",
        "UTC../../",
    ] {
        assert!(offset(zone).is_err());
    }
    assert!(clock("get_date", &json!({"format":"short"}), i64::MAX).is_err());
    assert!(clock("get_time", &json!({"format":"iso"}), 0).is_err());
}
#[tokio::test]
async fn native_execution_reports_errors_and_obeys_cancellation() {
    let call = execute(
        "calculate",
        &json!({"operation":"add","operands":[2,3]}),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(call["status"], "success");
    assert_eq!(call["output"]["result"], 5.0);
    assert_eq!(
        execute(
            "calculate",
            &json!({"expression":"2+2"}),
            CancellationToken::new()
        )
        .await
        .unwrap()["status"],
        "error"
    );
    assert_eq!(
        execute("get_time", &json!({}), CancellationToken::new())
            .await
            .unwrap()["status"],
        "success"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(execute(
        "calculate",
        &json!({"operation":"add","operands":[1]}),
        cancel
    )
    .await
    .is_err());
}
