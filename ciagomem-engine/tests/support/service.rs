use ciagomem_core::*;
use ciagomem_engine::{
    host::*,
    network::{self, Server},
    persistence::DurableJournal,
    *,
};
use ciagomem_wal::gclog::{LogConfig, TailMode};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};
pub struct Time;
impl Clock for Time {
    fn now_ns(&self) -> u64 {
        1
    }
}
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("ciagomem-network-test-{}", random_id().unwrap()));
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
        }
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub struct Certificates {
    pub _directory: Directory,
    pub server: Arc<tls::TlsContext>,
    pub client: Arc<tls::TlsContext>,
    pub fingerprint: [u8; 32],
}
fn openssl(directory: &Path, args: &[&str]) {
    let output = Command::new("openssl")
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
pub fn certificates() -> &'static Certificates {
    static CERTS: OnceLock<Certificates> = OnceLock::new();
    CERTS.get_or_init(||{
  let directory=Directory::new();let p=&directory.0;
  openssl(p,&["req","-x509","-newkey","ed25519","-keyout","ca.key","-out","ca.pem","-days","1","-nodes","-subj","/CN=Ciagomem Test CA"]);
  for (name,purpose,san) in [("server","serverAuth","subjectAltName=DNS:localhost\n"),("client","clientAuth","")] {
   openssl(p,&["req","-new","-newkey","ed25519","-keyout",&format!("{name}.key"),"-out",&format!("{name}.csr"),"-nodes","-subj",&format!("/CN={name}")]);
   std::fs::write(p.join("extensions"),format!("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage={purpose}\n{san}")).unwrap();
   openssl(p,&["x509","-req","-in",&format!("{name}.csr"),"-CA","ca.pem","-CAkey","ca.key","-CAcreateserial","-out",&format!("{name}.pem"),"-days","1","-extfile","extensions"]);
  }
  openssl(p,&["x509","-in","client.pem","-outform","DER","-out","client.der"]);
  let fingerprint=content_digest(&std::fs::read(p.join("client.der")).unwrap());
  let server=tls::TlsContext::new(&p.join("server.pem"),&p.join("server.key"),&p.join("ca.pem"),true).unwrap();
  let client=tls::TlsContext::new(&p.join("client.pem"),&p.join("client.key"),&p.join("ca.pem"),false).unwrap();
  Certificates{_directory:directory,server,client,fingerprint}
 })
}
pub fn token(boot: u128, client: u64) -> Macaroon {
    Macaroon::issue(
        &KeyedPrf([7; 32]),
        vec![
            Caveat::Tenant(11),
            Caveat::Session(1),
            Caveat::PeerUid(42),
            Caveat::Audience(7),
            Caveat::KeyEpoch(1),
            Caveat::BootEpoch(boot),
            Caveat::ExpiresAt(10000),
            Caveat::Ops(OP_READ | OP_WRITE),
            Caveat::ClientId(client),
        ],
    )
    .unwrap()
}
pub fn record() -> Record {
    Record::new(
        ActorRef {
            id: 1,
            incarnation: 1,
        },
        11,
        [("credits".into(), Scalar::Int(100))].into(),
        Label::external(1, vec![11]).unwrap(),
    )
}
pub fn host(directory: &Path, boot: u128) -> Host {
    let engine = Engine::<DurableJournal>::open_durable(
        directory,
        LogConfig {
            block: 4096,
            buf_bytes: 32768,
            buffers: 2,
            max_inflight: 1,
            min_batch_bytes: 4096,
            max_delay_ns: 0,
            capacity: 1 << 20,
            file: 0,
            tail: TailMode::Pad,
            max_retries: 0,
        },
        4 << 20,
        1,
        Limits::default(),
        schema::Schemas::new(),
        KeyedPrf([9; 32]),
        boot,
        Arc::new(Time),
    )
    .unwrap();
    Host::spawn(
        engine,
        Authentication {
            key: KeyedPrf([7; 32]),
            audience: 7,
            key_epoch: 1,
            boot_epoch: boot,
            clock: Arc::new(Time),
        },
        HostLimits::default(),
    )
    .unwrap()
}
pub fn config(boot: u128) -> network::Config {
    let certs = certificates();
    network::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        tls: certs.server.clone(),
        principals: BTreeMap::from([(certs.fingerprint, 42)]),
        boot,
        audience: 7,
        mode: reactor::Mode::Portable,
        cpu: None,
        connections: 16,
        streams: 8,
        frame_bytes: 1 << 20,
        memory_bytes: 64 << 20,
        handshake_timeout: Duration::from_secs(3),
        idle_timeout: Duration::from_secs(10),
        drain_timeout: Duration::from_secs(1),
    }
}
pub struct TestServer {
    pub server: Server,
    pub incoming: std::sync::mpsc::SyncSender<std::os::unix::net::UnixStream>,
}
impl TestServer {
    pub fn connect_named(
        &self,
        name: &str,
    ) -> std::io::Result<ciagomem_engine::client::Connection> {
        let (client, server) = std::os::unix::net::UnixStream::pair()?;
        self.incoming
            .send(server)
            .map_err(|_| std::io::Error::other("server stopped"))?;
        ciagomem_engine::client::Connection::from_unix_stream(
            client,
            name,
            certificates().client.clone(),
            Duration::from_secs(3),
        )
    }
    pub fn shutdown(self) -> std::io::Result<()> {
        self.server.shutdown()
    }
}
pub fn server_with(host: &Host, config: network::Config) -> TestServer {
    let (server, incoming) = Server::spawn_connected(host.client(), config).unwrap();
    TestServer { server, incoming }
}
pub fn server(host: &Host, boot: u128) -> TestServer {
    server_with(host, config(boot))
}
