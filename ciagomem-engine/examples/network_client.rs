//! Runnable fixed-Record Rust client; keeps unresolved intents on disk.
use ciagomem_core::{Label, Macaroon, tls::TlsContext};
use ciagomem_engine::{
    client::{Connection, IdentityFile},
    retry::WriteCommand,
    *,
};
use std::{path::Path, time::Duration};
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "identity" {
        let identity = IdentityFile::open(Path::new(&args[1]))?;
        println!(
            "client_id={} next_sequence={} pending={}",
            identity.client_id(),
            identity.next_sequence(),
            identity.has_pending()
        );
        return Ok(());
    }
    if args.len() != 7 {
        return Err("usage: network_client identity IDENTITY_FILE | ADDRESS SERVER_NAME CLIENT_CERT CLIENT_KEY CA_PEM TOKEN_FILE IDENTITY_FILE".into());
    }
    let tls = TlsContext::new(
        Path::new(&args[2]),
        Path::new(&args[3]),
        Path::new(&args[4]),
        false,
    )?;
    let token =
        Macaroon::from_bytes(&std::fs::read(&args[5])?).map_err(|e| format!("token: {e:?}"))?;
    let mut identity = IdentityFile::open(Path::new(&args[6]))?;
    let mut client = Connection::connect(args[0].parse()?, &args[1], tls, Duration::from_secs(5))?;
    let response = if identity.has_pending() {
        client.resume(&mut identity, &token)?
    } else {
        let tenant = token
            .caveats
            .iter()
            .find_map(|c| match c {
                ciagomem_core::Caveat::Tenant(id) => Some(*id),
                _ => None,
            })
            .ok_or("missing tenant")?;
        let record = Record::new(
            ActorRef {
                id: 1,
                incarnation: 1,
            },
            tenant,
            [("credits".into(), Scalar::Int(100))].into(),
            Label::external(1, vec![tenant])?,
        );
        client.write(&mut identity, &token, WriteCommand::Create(record))?
    };
    println!(
        "status={} commit_sequence={} result_bytes={} pending={}",
        response.status,
        response.sequence,
        response.body.len(),
        identity.has_pending()
    );
    if response.kind == 1 && response.status == 0 && response.body.len() == 88 {
        let actor = ActorRef {
            id: u64::from_le_bytes(response.body[64..72].try_into()?),
            incarnation: u128::from_le_bytes(response.body[72..88].try_into()?),
        };
        let read = client.read(&token, actor, response.sequence)?;
        println!(
            "actor_id={} read_status={} flatbuffer_bytes={}",
            actor.id,
            read.status,
            read.body.len()
        );
    }
    if identity.has_pending() || response.status != 0 {
        return Err(
            "request unresolved or rejected; retain identity file and inspect status".into(),
        );
    }
    Ok(())
}
