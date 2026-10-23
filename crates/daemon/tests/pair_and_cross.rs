use multicontrol_core::{IpcEvent, IpcRequest, PairingView};
use multicontrol_daemon::{ipc_path, start, DaemonOptions};
use multicontrol_platform::{Captured, DeviceEvent, FakeRecord};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

async fn send(write: &mut tokio::net::unix::OwnedWriteHalf, request: &IpcRequest) {
    let mut line = serde_json::to_string(request).unwrap();
    line.push('\n');
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
}

async fn wait_state(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    pred: impl Fn(&multicontrol_core::StateSnapshot) -> bool,
) -> multicontrol_core::StateSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout_at(deadline, reader.read_line(&mut line))
            .await
            .expect("timed out waiting for state")
            .expect("ipc read");
        if n == 0 {
            panic!("ipc closed");
        }
        if let Ok(IpcEvent::State { snapshot }) = serde_json::from_str(line.trim_end()) {
            if pred(&snapshot) {
                return snapshot;
            }
        }
    }
}

#[tokio::test]
async fn pair_then_cross_the_shared_edge() {
    let a_home = tempfile::tempdir().unwrap();
    let b_home = tempfile::tempdir().unwrap();
    let a = start(DaemonOptions {
        home: a_home.path().into(),
        fake_screen: Some((800, 600)),
        session_port: 0,
        heartbeat: Duration::from_secs(2),
        discover: false,
    })
    .await
    .unwrap();
    let b = start(DaemonOptions {
        home: b_home.path().into(),
        fake_screen: Some((800, 600)),
        session_port: 0,
        heartbeat: Duration::from_secs(2),
        discover: false,
    })
    .await
    .unwrap();

    let (mut a_write, mut a_read) = split_ipc(a_home.path()).await;
    let (mut b_write, mut b_read) = split_ipc(b_home.path()).await;
    let _ = wait_state(&mut a_read, |_| true).await;

    send(
        &mut a_write,
        &IpcRequest::BeginAddAddress {
            host: "127.0.0.1".into(),
            port: b.session_port,
        },
    )
    .await;
    let showing = wait_state(&mut a_read, |state| {
        matches!(state.pairing, Some(PairingView::ShowingCode { .. }))
    })
    .await;
    let code = match showing.pairing {
        Some(PairingView::ShowingCode { code, .. }) => code,
        other => panic!("expected a code, got {other:?}"),
    };
    let _ = wait_state(&mut b_read, |state| {
        matches!(state.pairing, Some(PairingView::EnterCode { .. }))
    })
    .await;
    send(&mut b_write, &IpcRequest::SubmitCode { code }).await;
    let paired = wait_state(&mut a_read, |state| !state.paired.is_empty()).await;
    let peer = paired.paired[0].id.clone();
    let width = paired
        .screens
        .iter()
        .find(|screen| screen.id.0 == paired.local_id)
        .unwrap()
        .width as i32;
    send(
        &mut a_write,
        &IpcRequest::Place {
            id: peer,
            x: width,
            y: 0,
        },
    )
    .await;
    let _ = wait_state(&mut a_read, |state| {
        state
            .screens
            .iter()
            .any(|screen| screen.id.0 != state.local_id && screen.placed)
    })
    .await;

    let input = a.fake.as_ref().unwrap().input.clone();
    input
        .send(Captured::Key {
            usage: 0xE1,
            down: true,
        })
        .unwrap();
    input.send(Captured::MouseDelta { dx: 500, dy: 0 }).unwrap();

    let log = b.fake.as_ref().unwrap().log.clone();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let records = log.lock().unwrap().clone();
        let shifted = records.iter().any(|record| {
            matches!(record, FakeRecord::Inject(DeviceEvent::Key { usage: 0xE1, down: true }))
        });
        let moved = records.iter().any(|record| {
            matches!(record, FakeRecord::Warp { x, .. } if *x == 100)
                || matches!(record, FakeRecord::Inject(DeviceEvent::MouseAbsolute { x, .. }) if *x == 100)
        });
        if shifted && moved {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("remote did not receive the crossing: {records:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    a.stop();
    b.stop();
}

async fn split_ipc(
    home: &Path,
) -> (
    tokio::net::unix::OwnedWriteHalf,
    BufReader<tokio::net::unix::OwnedReadHalf>,
) {
    let path = ipc_path(home);
    let stream = {
        let mut last = None;
        let mut connected = None;
        for _ in 0..100 {
            match UnixStream::connect(&path).await {
                Ok(stream) => {
                    connected = Some(stream);
                    break;
                }
                Err(err) => last = Some(err),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        connected.unwrap_or_else(|| panic!("ipc {}: {last:?}", path.display()))
    };
    let (read, write) = stream.into_split();
    (write, BufReader::new(read))
}
