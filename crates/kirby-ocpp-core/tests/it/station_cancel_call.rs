//! `Command::CancelCall`: stop waiting for an unanswered CALL.

use kirby_ocpp_core::StationError;
use kirby_ocpp_core::station::Command;
use kirby_ocpp_testkit::Fault;

use crate::station_support::{Rig, STEP};

fn data_transfer() -> Command {
    Command::DataTransfer {
        vendor_id: "acme".to_owned(),
        message_id: None,
        data: None,
    }
}

#[tokio::test]
async fn cancelling_an_unanswered_call_completes_its_command_with_cancelled() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms.inject(Fault::silence("DataTransfer").times(1));

    let station = rig.station.clone();
    let waiting = tokio::spawn(async move { station.send(data_transfer()).await });
    // The CSMS saw the CALL and stays silent: the command is genuinely waiting.
    rig.csms.wait_for_call("DataTransfer", STEP).await.unwrap();

    let cancelled = rig
        .station
        .send(Command::CancelCall {
            action: "DataTransfer".to_owned(),
        })
        .await;
    assert!(cancelled.is_ok(), "{cancelled:?}");

    let result = tokio::time::timeout(STEP, waiting)
        .await
        .expect("the waiting command completes after the cancel")
        .expect("task");
    let error = result.expect_err("a cancelled call is an error");
    assert!(error.to_string().contains("cancelled"), "{error}");
    rig.observed.wait_log("DataTransfer cancelled").await;

    // Nothing is left to cancel.
    let again = rig
        .station
        .send(Command::CancelCall {
            action: "DataTransfer".to_owned(),
        })
        .await;
    assert!(
        matches!(again, Err(StationError::InvalidState { .. })),
        "{again:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn transaction_messages_cannot_be_cancelled() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    for action in ["StartTransaction", "StopTransaction", "MeterValues"] {
        let result = rig
            .station
            .send(Command::CancelCall {
                action: action.to_owned(),
            })
            .await;
        match result {
            Err(StationError::InvalidState { reason, .. }) => {
                assert!(reason.contains("cannot be cancelled"), "{reason}");
            }
            other => panic!("{action}: {other:?}"),
        }
    }
    rig.shutdown().await;
}
