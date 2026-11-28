use super::super::*;

use aurelia::core::error::{ErrorKind, classify};
use aurelia::core::models::DownloadProgress;

async fn drive(messages: Vec<DownloadProgress>) -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    for m in messages {
        tx.send(m).await.unwrap();
    }
    drop(tx);
    drive_progress(rx, true).await
}

#[tokio::test]
async fn typed_failure_keeps_its_kind() {
    let io = std::io::Error::from(std::io::ErrorKind::StorageFull);
    let failed = DownloadProgress::failed_with("depot 7", &anyhow::Error::new(io));
    let err = drive(vec![failed]).await.unwrap_err();
    assert_eq!(classify(&err).kind, ErrorKind::DiskFull);
    assert_eq!(classify(&err).kind.exit_code(), 85);
    assert!(err.to_string().contains("depot 7"));
}

#[tokio::test]
async fn untyped_failure_stays_unknown() {
    let err = drive(vec![DownloadProgress::failed("No matching depots")])
        .await
        .unwrap_err();
    assert_eq!(classify(&err).kind, ErrorKind::Unknown);
}

#[tokio::test]
async fn completed_is_ok() {
    assert!(drive(vec![DownloadProgress::completed("")]).await.is_ok());
}

#[test]
fn cloud_write_prompt_calls_out_deletions() {
    let plain = cloud_write_prompt("upload", 3, 0);
    assert!(!plain.contains("DELETES"), "{plain}");
    let warn = cloud_write_prompt("upload", 3, 2);
    assert!(warn.contains("DELETES 2 collection(s)"), "{warn}");
}

mod licence {
    use super::super::super::{Licence, decide_licence};
    use anyhow::{Result, anyhow};

    async fn decide(
        owned: bool,
        offline: bool,
        family: Result<Vec<u32>>,
        ticket: bool,
    ) -> Option<Licence> {
        decide_licence(owned, offline, 10, async { family }, async move {
            if ticket {
                Ok(())
            } else {
                Err(anyhow!("denied"))
            }
        })
        .await
    }

    #[tokio::test]
    async fn owned_list_wins_without_lookups() {
        let family = async { panic!("no family lookup for owned games") };
        let ticket = async { panic!("no ticket for owned games") };
        assert_eq!(
            decide_licence(true, false, 10, family, ticket).await,
            Some(Licence::Owned)
        );
    }

    #[tokio::test]
    async fn family_then_ticket_then_denied() {
        assert_eq!(
            decide(false, false, Ok(vec![10]), false).await,
            Some(Licence::FamilyShared)
        );
        assert_eq!(
            decide(false, false, Ok(vec![20]), true).await,
            Some(Licence::Owned)
        );
        assert_eq!(
            decide(false, false, Err(anyhow!("down")), true).await,
            Some(Licence::Licensed)
        );
        assert_eq!(
            decide(false, false, Err(anyhow!("down")), false).await,
            None
        );
        assert_eq!(decide(false, false, Ok(vec![]), false).await, None);
    }

    #[tokio::test]
    async fn offline_refuses_games_not_owned() {
        assert_eq!(decide(false, true, Ok(vec![10]), true).await, None);
    }
}
