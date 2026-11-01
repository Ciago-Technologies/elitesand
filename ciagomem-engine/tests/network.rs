#![cfg(target_os = "linux")]
#[path = "support/service.rs"]
mod support;
use ciagomem_engine::{
    client::{Connection, IdentityFile},
    retry::WriteCommand,
    *,
};
use std::time::Duration;
use support::*;
fn connect(server: &TestServer) -> Connection {
    server.connect_named("localhost").unwrap()
}
fn created(response: &ciagomem_engine::client::Response) -> ActorRef {
    assert_eq!(response.status, 0);
    assert_eq!(response.body.len(), 88);
    ActorRef {
        id: u64::from_le_bytes(response.body[64..72].try_into().unwrap()),
        incarnation: u128::from_le_bytes(response.body[72..88].try_into().unwrap()),
    }
}
#[test]
fn authenticated_network_crud_and_cas_receipts() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let mut identity = IdentityFile::open(&d.0.join("client.identity")).unwrap();
    let token = token(1, identity.client_id());
    let mut connection = connect(&s);
    let create = connection
        .write(&mut identity, &token, WriteCommand::Create(record()))
        .unwrap();
    let actor = created(&create);
    let version = create.sequence;
    let read = connection.read(&token, actor, version).unwrap();
    assert_eq!(read.status, 0);
    assert_eq!(image::view(&read.body).unwrap().fields().len(), 1);
    let command = WriteCommand::Mutate {
        actor,
        expected_version: version,
        query: format!("@{} mutate {{state.credits -= 25}}", actor.id),
    };
    let changed = connection.write(&mut identity, &token, command).unwrap();
    assert_eq!(changed.status, 0);
    let read = connection.read(&token, actor, changed.sequence).unwrap();
    assert_eq!(
        image::view(&read.body).unwrap().fields().get(0).int_value(),
        75
    );
    let conflict = connection
        .write(
            &mut identity,
            &token,
            WriteCommand::Delete {
                actor,
                expected_version: version,
            },
        )
        .unwrap();
    assert_eq!(conflict.status, 5);
    assert!(!identity.has_pending());
    let deleted = connection
        .write(
            &mut identity,
            &token,
            WriteCommand::Delete {
                actor,
                expected_version: changed.sequence,
            },
        )
        .unwrap();
    assert_eq!(deleted.status, 0);
    assert_eq!(connection.read(&token, actor, 0).unwrap().status, 2);
    drop(connection);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn hostname_and_unmapped_principal_fail_closed() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    assert!(s.connect_named("wrong.invalid").is_err());
    s.shutdown().unwrap();
    let mut cfg = config(1);
    cfg.principals.clear();
    cfg.principals.insert([0; 32], 42);
    let s = server_with(&h, cfg);
    assert!(s.connect_named("localhost").is_err());
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn read_query_cannot_bypass_durable_retry_gate() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let mut connection = connect(&s);
    assert_eq!(
        connection
            .query(&token(1, 99), "@1 mutate {state.credits -= 25}")
            .unwrap()
            .status,
        14
    );
    drop(connection);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn client_identity_corruption_and_competing_owner_are_rejected() {
    let d = Directory::new();
    let path = d.0.join("client.identity");
    let identity = IdentityFile::open(&path).unwrap();
    assert!(IdentityFile::open(&path).is_err());
    let id = identity.client_id();
    drop(identity);
    assert_eq!(IdentityFile::open(&path).unwrap().client_id(), id);
    std::fs::write(&path, b"broken").unwrap();
    assert!(IdentityFile::open(&path).is_err());
}

#[test]
fn lost_ack_resumes_exact_intent_after_both_process_states_reopen() {
    let d = Directory::new();
    let path = d.0.join("client.identity");
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let mut identity = IdentityFile::open(&path).unwrap();
    let id = identity.client_id();
    let auth = token(1, id);
    let mut connection = connect(&s);
    let command = WriteCommand::Create(record());
    connection
        .prepare_write(&mut identity, &auth, &command)
        .unwrap();
    let (op, key, incarnation, body) = ciagomem_engine::client::encode_command(&command).unwrap();
    let payload = connection
        .payload(&auth, &key, incarnation, identity.next_sequence(), &body)
        .unwrap();
    let committed = connection.exchange(op, &payload).unwrap();
    let actor = created(&committed);
    // Crash after the server ACK, before IdentityFile records its receipt.
    assert!(identity.has_pending());
    drop(identity);
    drop(connection);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
    let h = host(&d.0, 2);
    let s = server(&h, 2);
    let mut identity = IdentityFile::open(&path).unwrap();
    assert!(identity.has_pending());
    let mut connection = connect(&s);
    assert_eq!(connection.boot_epoch(), 2);
    // An old boot token cannot resolve or discard the pending intent.
    assert_eq!(connection.resume(&mut identity, &auth).unwrap().status, 3);
    assert!(identity.has_pending());
    let renewed = token(2, id);
    let retried = connection.resume(&mut identity, &renewed).unwrap();
    assert_eq!(retried.body, committed.body);
    assert_eq!(retried.sequence, committed.sequence);
    assert!(!identity.has_pending());
    assert_eq!(identity.next_sequence(), 2);
    assert!(identity.last_result().is_some());
    assert_eq!(
        connection
            .read(&renewed, actor, committed.sequence)
            .unwrap()
            .status,
        0
    );
    drop(connection);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}

#[test]
fn tcp_listener_or_explicit_sandbox_skip() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = match ciagomem_engine::network::Server::spawn(h.client(), config(1)) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EPERM | libc::EACCES | libc::ENOSYS)
            ) =>
        {
            eprintln!("SKIP real TCP listener: {e}");
            h.shutdown().unwrap();
            return;
        }
        Err(e) => panic!("TCP startup: {e}"),
    };
    let c = Connection::connect(
        s.address(),
        "localhost",
        certificates().client.clone(),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(c.boot_epoch(), 1);
    drop(c);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}

struct Raw(ciagomem_core::tls::TlsStream);
impl Raw {
    fn connect(server: &TestServer) -> Self {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        server.incoming.send(b).unwrap();
        let mut tls = certificates()
            .client
            .unix_session(a, Some("localhost"))
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while !tls.handshake().unwrap() {
            assert!(std::time::Instant::now() < until);
            std::thread::yield_now();
        }
        Self(tls)
    }
    fn send(&mut self, bytes: &[u8]) {
        use std::io::Write;
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let mut offset = 0;
        while offset < bytes.len() {
            match self.0.write(&bytes[offset..]) {
                Ok(n) if n > 0 => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < until);
                    std::thread::yield_now();
                }
                other => panic!("raw send: {other:?}"),
            }
        }
    }
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<()> {
        use std::io::Read;
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let mut offset = 0;
        while offset < bytes.len() {
            match self.0.read(&mut bytes[offset..]) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= until {
                        return Err(std::io::ErrorKind::TimedOut.into());
                    }
                    std::thread::yield_now();
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn response(&mut self) -> (u32, u16) {
        let mut header = [0; 18];
        self.read(&mut header).unwrap();
        let h = ciagomem_engine::wire::Header::decode(&header, 1 << 20).unwrap();
        let mut payload = vec![0; h.length];
        self.read(&mut payload).unwrap();
        (
            h.stream,
            u16::from_le_bytes(payload[..2].try_into().unwrap()),
        )
    }
}
fn hello(stream: u32) -> Vec<u8> {
    ciagomem_engine::wire::Header {
        op: 1,
        flags: 0,
        stream,
        length: 0,
    }
    .frame(&[])
    .unwrap()
}
#[test]
fn fragmented_multiplexed_frames_and_stalled_peer_make_progress() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let mut slow = Raw::connect(&s);
    slow.send(&hello(1)[..5]);
    let mut raw = Raw::connect(&s);
    raw.send(&hello(2)[..7]);
    let mut rest = hello(2)[7..].to_vec();
    rest.extend(hello(3));
    raw.send(&rest);
    assert_eq!(raw.response(), (2, 0));
    assert_eq!(raw.response(), (3, 0));
    let mut c = connect(&s);
    assert_eq!(
        c.query(&token(1, 99), "@1 select {credits: state.credits}")
            .unwrap()
            .status,
        3
    );
    drop(c);
    drop(raw);
    drop(slow);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn malformed_crc_and_duplicate_streams_close_only_the_offending_peer() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    for malformed in 0..3 {
        let mut raw = Raw::connect(&s);
        let mut bytes = hello(1);
        if malformed == 1 {
            bytes.extend(hello(1));
        } else if malformed == 2 {
            bytes[10..14].copy_from_slice(&((1u32 << 20) + 1).to_le_bytes());
            let crc = ciagomem_core::envelope::crc32c(&[&bytes[..14]]);
            bytes[14..18].copy_from_slice(&crc.to_le_bytes());
        } else {
            bytes[14] ^= 1;
        }
        raw.send(&bytes);
        assert!(raw.read(&mut [0; 18]).is_err());
    }
    drop(connect(&s));
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn exhausted_network_credits_return_busy_without_accepting_a_write() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let mut cfg = config(1);
    cfg.memory_bytes = 4 * cfg.frame_bytes;
    let s = server_with(&h, cfg);
    let a = connect(&s);
    let b = connect(&s);
    let mut c = connect(&s);
    let mut identity = IdentityFile::open(&d.0.join("client.identity")).unwrap();
    let auth = token(1, identity.client_id());
    assert_eq!(
        c.write(&mut identity, &auth, WriteCommand::Create(record()))
            .unwrap()
            .status,
        8
    );
    assert!(identity.has_pending());
    assert_eq!(identity.next_sequence(), 1);
    drop(a);
    drop(b);
    // Wait for disconnected peers to release credits using bounded protocol attempts.
    let until = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let r = c.resume(&mut identity, &auth).unwrap();
        if r.status != 8 {
            assert_eq!(r.status, 0);
            break;
        }
        assert!(std::time::Instant::now() < until);
    }
    assert!(!identity.has_pending());
    drop(c);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}

#[test]
fn daemon_issuer_writes_bounded_private_tokens_and_never_overwrites() {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let d = Directory::new();
    let key = d.0.join("auth.key");
    let output = d.0.join("token");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key)
        .unwrap();
    f.write_all(&[7; 32]).unwrap();
    drop(f);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let expiry = (now + 60_000_000_000).to_string();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_ciagomem-server"))
            .args([
                "issue-token",
                key.to_str().unwrap(),
                output.to_str().unwrap(),
                "11",
                "1",
                "42",
                "1",
                "99",
                &expiry,
            ])
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(
        std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        ciagomem_core::Macaroon::from_bytes(&bytes)
            .unwrap()
            .caveats
            .last(),
        Some(&ciagomem_core::Caveat::ClientId(99))
    );
    assert!(!run().status.success());
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(&output).unwrap();
    assert!(!run().status.success());
    assert!(!output.exists());
}

#[test]
fn disconnected_debit_returns_one_effect_after_restart() {
    let d = Directory::new();
    let path = d.0.join("client.identity");
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let mut identity = IdentityFile::open(&path).unwrap();
    let id = identity.client_id();
    let auth = token(1, id);
    let mut connection = connect(&s);
    let create = connection
        .write(&mut identity, &auth, WriteCommand::Create(record()))
        .unwrap();
    let actor = created(&create);
    let command = WriteCommand::Mutate {
        actor,
        expected_version: create.sequence,
        query: format!("@{} mutate {{state.credits -= 25}}", actor.id),
    };
    connection
        .prepare_write(&mut identity, &auth, &command)
        .unwrap();
    let (op, key, inc, body) = ciagomem_engine::client::encode_command(&command).unwrap();
    let payload = connection
        .payload(&auth, &key, inc, identity.next_sequence(), &body)
        .unwrap();
    let mut raw = Raw::connect(&s);
    raw.send(
        &wire::Header {
            op,
            flags: 0,
            stream: 88,
            length: payload.len(),
        }
        .frame(&payload)
        .unwrap(),
    );
    let until = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let read = connection.read(&auth, actor, 0).unwrap();
        if image::view(&read.body).unwrap().fields().get(0).int_value() == 75 {
            break;
        }
        assert!(std::time::Instant::now() < until);
    }
    // Never read the mutation response; close the stream and both process-owned states.
    drop(raw);
    drop(connection);
    drop(identity);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
    let h = host(&d.0, 2);
    let s = server(&h, 2);
    let mut connection = connect(&s);
    let mut identity = IdentityFile::open(&path).unwrap();
    let auth = token(2, id);
    let receipt = connection.resume(&mut identity, &auth).unwrap();
    assert_eq!(receipt.status, 0);
    let read = connection.read(&auth, actor, receipt.sequence).unwrap();
    assert_eq!(
        image::view(&read.body).unwrap().fields().get(0).int_value(),
        75
    );
    assert!(!identity.has_pending());
    drop(connection);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
#[test]
fn request_decoder_rejects_every_truncated_prefix_and_body() {
    let d = Directory::new();
    let h = host(&d.0, 1);
    let s = server(&h, 1);
    let c = connect(&s);
    let (op, key, inc, body) =
        ciagomem_engine::client::encode_command(&WriteCommand::Create(record())).unwrap();
    let payload = c.payload(&token(1, 99), &key, inc, 1, &body).unwrap();
    for length in 0..payload.len() {
        assert!(
            wire::decode_request(
                wire::Header {
                    op,
                    flags: 0,
                    stream: 1,
                    length
                },
                &payload[..length],
                42,
                1
            )
            .is_err()
        );
    }
    assert!(
        wire::decode_request(
            wire::Header {
                op,
                flags: 0,
                stream: 1,
                length: payload.len()
            },
            &payload,
            42,
            1
        )
        .is_ok()
    );
    drop(c);
    s.shutdown().unwrap();
    h.shutdown().unwrap();
}
