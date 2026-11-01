//! Single-node supervised Linux service. No implicit plaintext or I/O backend downgrade.
#![deny(unsafe_code)]
#[cfg(target_os = "linux")]
mod linux {
    use ciagomem_core::{reactor, tls::TlsContext, *};
    use ciagomem_engine::{host::*, network, persistence::DurableJournal, *};
    use ciagomem_wal::gclog::{LogConfig, TailMode};
    use std::{
        collections::BTreeMap,
        fs::{File, OpenOptions},
        io::{self, Read, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        path::Path,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const USAGE: &str = "ciagomem-server serve DATA_DIR AUTH_KEY STORAGE_KEY CERT_PEM KEY_PEM CA_PEM PRINCIPALS BIND [--portable-loopback]\nciagomem-server issue-token AUTH_KEY OUTPUT TENANT SESSION PEER_UID BOOT_EPOCH CLIENT_ID EXPIRES_UNIX_NS\nAll integers are decimal; key files are exactly 32 raw bytes, mode 0600. PRINCIPALS lines: SHA256_LEAF_CERT_HEX PEER_UID. Audience=7, key epoch=1. Native Linux io_uring + CPU affinity are mandatory unless loopback test mode is explicitly selected.";
    struct Time;
    impl Clock for Time {
        fn now_ns(&self) -> u64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(u64::MAX, |n| n.as_nanos().min(u64::MAX as u128) as u64)
        }
    }
    fn private_file(path: &Path, max: u64) -> io::Result<Vec<u8>> {
        let mut f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = f.metadata()?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "require a private regular file within the size limit",
            ));
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn key(path: &str) -> AnyResult<[u8; 32]> {
        private_file(Path::new(path), 32)?
            .try_into()
            .map_err(|_| "key must contain exactly 32 raw bytes".into())
    }
    fn principals(path: &str) -> AnyResult<BTreeMap<[u8; 32], u32>> {
        let bytes = private_file(Path::new(path), 1 << 20)?;
        let mut map = BTreeMap::new();
        for line in std::str::from_utf8(&bytes)?.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() != 2 || parts[0].len() != 64 || !parts[0].is_ascii() {
                return Err("invalid principal line".into());
            }
            let mut digest = [0; 32];
            for (i, v) in digest.iter_mut().enumerate() {
                *v = u8::from_str_radix(&parts[0][i * 2..i * 2 + 2], 16)?;
            }
            if map.insert(digest, parts[1].parse()?).is_some() {
                return Err("duplicate principal".into());
            }
        }
        if map.is_empty() || map.len() > 4096 {
            return Err("principal count must be 1..4096".into());
        }
        Ok(map)
    }
    fn serve(a: &[String]) -> AnyResult<()> {
        if a.len() != 9 && a.len() != 10 {
            return Err(USAGE.into());
        }
        let portable = a.len() == 10 && a[9] == "--portable-loopback";
        if a.len() == 10 && !portable {
            return Err(USAGE.into());
        }
        let directory = std::path::PathBuf::from(&a[1]);
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let meta = std::fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            return Err("data directory must be a private directory (0700), not a symlink".into());
        }
        let auth_key = key(&a[2])?;
        let storage_key = key(&a[3])?;
        if auth_key == storage_key {
            return Err("authentication and storage keys must differ".into());
        }
        let tls = TlsContext::new(Path::new(&a[4]), Path::new(&a[5]), Path::new(&a[6]), true)?;
        private_file(Path::new(&a[5]), 1 << 20)?;
        let principals = principals(&a[7])?;
        let bind = a[8].parse()?;
        let boot = SessionEpoch::new()?.id();
        let cpus = reactor::allowed_cpus()?;
        if !portable && cpus.len() < 2 {
            return Err("native service needs two allowed CPUs".into());
        }
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(Time);
        let config = network::Config {
            bind,
            tls,
            principals,
            boot,
            audience: 7,
            mode: if portable {
                reactor::Mode::Portable
            } else {
                reactor::Mode::Uring
            },
            cpu: if portable { None } else { Some(cpus[0]) },
            connections: 64,
            streams: 32,
            frame_bytes: 1 << 20,
            memory_bytes: 128 << 20,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            drain_timeout: Duration::from_secs(10),
        };
        config.validate()?;
        let cpu = cpus.first().copied().and_then(|_| cpus.get(1).copied());
        let engine_clock = clock.clone();
        reactor::install_shutdown_handlers()?;
        let host = Host::spawn_with(
            move || {
                if !portable {
                    reactor::pin(cpu.ok_or(Error::InvalidValue)?).map_err(|_| Error::Journal)?;
                }
                let mut engine = Engine::<DurableJournal>::open_durable(
                    &directory,
                    LogConfig {
                        block: 4096,
                        buf_bytes: 1 << 20,
                        buffers: 2,
                        max_inflight: 1,
                        min_batch_bytes: 4096,
                        max_delay_ns: 0,
                        capacity: 64 << 20,
                        file: 0,
                        tail: TailMode::Pad,
                        max_retries: 0,
                    },
                    64 << 20,
                    1,
                    Limits::default(),
                    schema::Schemas::new(),
                    KeyedPrf(storage_key),
                    boot,
                    engine_clock,
                )?;
                if !portable {
                    engine.enable_uring().map_err(|_| Error::Journal)?;
                }
                Ok(engine)
            },
            Authentication {
                key: KeyedPrf(auth_key),
                audience: 7,
                key_epoch: 1,
                boot_epoch: boot,
                clock,
            },
            HostLimits::default(),
        )?;
        let server = network::Server::spawn(host.client(), config)?;
        println!(
            "READY address={} boot_epoch={} audience=7 key_epoch=1 mode={}",
            server.address(),
            boot,
            if portable {
                "portable-loopback-test"
            } else {
                "native"
            }
        );
        while !reactor::shutdown_requested() && !server.is_finished() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        server.shutdown()?;
        host.shutdown().map_err(|_| "storage owner panicked")?;
        Ok(())
    }
    fn issue(a: &[String]) -> AnyResult<()> {
        if a.len() != 9 {
            return Err(USAGE.into());
        }
        let auth = key(&a[1])?;
        let tenant = a[3].parse::<u128>()?;
        let session = a[4].parse::<u128>()?;
        let uid = a[5].parse::<u32>()?;
        let boot = a[6].parse::<u128>()?;
        let client = a[7].parse::<u64>()?;
        let expiry = a[8].parse::<u64>()?;
        let now = Time.now_ns();
        if tenant == 0
            || session == 0
            || boot == 0
            || client == 0
            || expiry <= now
            || expiry.saturating_sub(now) > 86_400_000_000_000
        {
            return Err("require nonzero identity and expiry within 24 hours".into());
        }
        let token = Macaroon::issue(
            &KeyedPrf(auth),
            vec![
                Caveat::Tenant(tenant),
                Caveat::Session(session),
                Caveat::PeerUid(uid),
                Caveat::Audience(7),
                Caveat::KeyEpoch(1),
                Caveat::BootEpoch(boot),
                Caveat::ExpiresAt(expiry),
                Caveat::Ops(OP_READ | OP_WRITE),
                Caveat::ClientId(client),
            ],
        )
        .map_err(|e| format!("token issuance: {e:?}"))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&a[2])?;
        output.write_all(
            &token
                .to_bytes()
                .map_err(|e| format!("token encoding: {e:?}"))?,
        )?;
        output.sync_all()?;
        let parent = Path::new(&a[2])
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        println!("Token written; keep it private. audience=7 key_epoch=1");
        Ok(())
    }
    pub fn run() -> AnyResult<()> {
        let a: Vec<_> = std::env::args().skip(1).collect();
        match a.first().map(String::as_str) {
            Some("serve") => serve(&a),
            Some("issue-token") => issue(&a),
            Some("--help") | Some("-h") => {
                println!("{USAGE}");
                Ok(())
            }
            _ => Err(USAGE.into()),
        }
    }
}
fn main() {
    #[cfg(target_os = "linux")]
    if let Err(e) = linux::run() {
        eprintln!("ciagomem-server: {e}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("ciagomem-server requires Linux");
        std::process::exit(1);
    }
}
