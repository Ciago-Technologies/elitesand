//! Single-owner file driver for GroupCommitLog. Blocking disk calls belong on a storage worker.
//! A durable acknowledgement anchor detects recovery stopping before an acknowledged record.
use crate::gclog::{self, GroupCommitLog, LogConfig, TailMode};
use ciagomem_core::{
    CoreControl,
    envelope::{KIND_DATA, crc32c},
    io::{IoBackend, SqFull},
};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
};

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "WAL needs repair")
}
struct FileIo {
    file: File,
    completed: VecDeque<(u64, i32)>,
    #[cfg(target_os = "linux")]
    ring: Option<(ciagomem_core::uring::UringIo, std::thread::ThreadId)>,
}
impl IoBackend for FileIo {
    unsafe fn write_at(
        &mut self,
        token: u64,
        file: u16,
        ptr: *const u8,
        len: u32,
        offset: u64,
        dsync: bool,
    ) -> Result<(), SqFull> {
        #[cfg(target_os = "linux")]
        if let Some((ring, owner)) = &mut self.ring {
            if *owner != std::thread::current().id() {
                return Err(SqFull);
            }
            // SAFETY: forward the same owned log buffer and completion token to the ring.
            return unsafe { ring.write_at(token, file, ptr, len, offset, dsync) };
        }
        if file != 0 || self.completed.len() >= 8 {
            return Err(SqFull);
        }
        // SAFETY: caller's IoBackend contract retains readable bytes for this synchronous write.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        let result = self
            .file
            .write_all_at(bytes, offset)
            .and_then(|_| if dsync { self.file.sync_data() } else { Ok(()) });
        self.completed.push_back((
            token,
            if result.is_ok() {
                len as i32
            } else {
                -libc::EIO
            },
        ));
        Ok(())
    }
    unsafe fn read_at(&mut self, _: u64, _: u16, _: *mut u8, _: u32, _: u64) -> Result<(), SqFull> {
        Err(SqFull)
    }
}
/// Local worker integration; not an io_uring reactor or a replacement for device qualification.
pub struct FileLog {
    io: FileIo,
    log: GroupCommitLog,
    anchor: File,
    owner_lock: Option<File>,
    anchor_seq: u64,
    fenced: bool,
    cfg: LogConfig,
    recovery_start: u64,
    recovery_budget: usize,
    retained_payload_bytes: usize,
    last_payload_bytes: usize,
}
impl Drop for FileLog {
    fn drop(&mut self) {
        if let Some(lock) = self.owner_lock.take() {
            if self.log.quiescent() {
                // Explicit unlock avoids fork/exec-inherited descriptors delaying a clean reopen.
                loop {
                    match lock.unlock() {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        _ => break,
                    }
                }
            } else {
                // Pending device access must retain both backing (GroupCommitLog) and ownership.
                std::mem::forget(lock);
            }
        }
    }
}
impl FileLog {
    pub fn open(
        path: &Path,
        cfg: LogConfig,
        max_recovery_bytes: usize,
    ) -> io::Result<(Self, Vec<Vec<u8>>)> {
        if !cfg.block.is_power_of_two()
            || cfg.block < 512
            || cfg.block > 65536
            || cfg.buffers < 2
            || cfg.buffers > 64
            || cfg.max_inflight == 0
            || cfg.max_inflight >= cfg.buffers
            || cfg.buf_bytes % cfg.block != 0
            || cfg.buf_bytes < 2 * cfg.block
            || cfg.capacity % cfg.block as u64 != 0
            || cfg
                .buf_bytes
                .checked_mul(cfg.buffers)
                .is_none_or(|n| n > 64 << 20 || cfg.capacity < n as u64)
            || max_recovery_bytes == 0
            || cfg.file != 0
            || !matches!(cfg.tail, TailMode::Pad)
            || cfg.max_retries > 8
            || cfg.buf_bytes > 16 << 20
            || cfg.capacity > 1 << 40
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported worker log configuration",
            ));
        }
        cfg.validate();
        fn owned_file(path: &Path) -> io::Result<(File, bool)> {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW);
            match options.create_new(true).open(path) {
                Ok(file) => Ok((file, true)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Ok((options.create_new(false).create(false).open(path)?, false))
                }
                Err(error) => Err(error),
            }
        }
        let anchor_path = path.with_extension("ack");
        if anchor_path == path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anchor aliases WAL",
            ));
        }
        let (file, created) = owned_file(path)?;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::Error(error))
                    if error.kind() == io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "WAL already owned",
                    ));
                }
            }
        }
        let owner_lock = file.try_clone()?;
        let (anchor, anchor_created) = owned_file(&anchor_path)?;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        let anchor_metadata = anchor.metadata()?;
        if !metadata.is_file()
            || !anchor_metadata.is_file()
            || (metadata.dev(), metadata.ino()) == (anchor_metadata.dev(), anchor_metadata.ino())
        {
            return Err(invalid());
        }
        let fresh = created && anchor_created;
        if created != anchor_created {
            return Err(invalid());
        }
        if fresh {
            file.set_len(cfg.data_offset() + cfg.capacity)?;
            anchor.set_len(128)?;
            file.sync_all()?;
            anchor.sync_all()?;
            File::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        if file.metadata()?.len() != cfg.data_offset() + cfg.capacity
            || anchor.metadata()?.len() != 128
        {
            return Err(invalid());
        }
        let mut slots = [0u8; 128];
        anchor.read_exact_at(&mut slots, 0)?;
        let acknowledged = slots
            .chunks_exact(64)
            .filter_map(|b| {
                if &b[..8] != b"CMACK001"
                    || crc32c(&[&b[..60]]) != u32::from_le_bytes(b[60..64].try_into().unwrap())
                {
                    return None;
                }
                if u64::from_le_bytes(b[24..32].try_into().unwrap()) != cfg.capacity
                    || u64::from_le_bytes(b[32..40].try_into().unwrap()) != cfg.block as u64
                {
                    return None;
                }
                Some((
                    u64::from_le_bytes(b[8..16].try_into().unwrap()),
                    u64::from_le_bytes(b[16..24].try_into().unwrap()),
                ))
            })
            .max_by_key(|v| v.0);
        if !fresh && acknowledged.is_none() {
            return Err(invalid());
        }
        let (anchor_seq, ack_lsn) = acknowledged.unwrap_or((0, 0));
        let mut used = 0usize;
        let mut payloads = Vec::new();
        let bad = std::cell::Cell::new(false);
        let recovery = gclog::recover(
            &cfg,
            &mut |at, bytes| {
                if bad.get() {
                    return Err(invalid());
                }
                file.read_exact_at(bytes, at)
            },
            &mut |header, bytes| {
                used = used.saturating_add(bytes.len());
                if header.kind != KIND_DATA || used > max_recovery_bytes {
                    bad.set(true);
                } else {
                    payloads.push(bytes.to_vec());
                }
            },
        )?;
        if bad.get() || recovery.next_lsn.saturating_sub(1) < ack_lsn {
            return Err(invalid());
        }
        // Missing both superblocks in an existing store must not become a fresh database.
        if !fresh && recovery.sb.seq == 0 {
            return Err(invalid());
        }
        if recovery.max_epoch == u32::MAX
            || recovery.sb.epoch == u32::MAX
            || recovery.sb.seq == u64::MAX
            || recovery.next_lsn > u64::MAX - 4
        {
            return Err(invalid());
        }
        let (mut log, sb) = GroupCommitLog::open(cfg, &recovery, CoreControl::shared(1), 0);
        let mut block = vec![0; cfg.block];
        sb.encode(&mut block);
        file.write_all_at(&block, sb.file_offset(cfg.block))?;
        file.sync_data()?;
        log.superblock_durable(&sb);
        let last_payload_bytes = payloads.last().map_or(0, Vec::len);
        let mut result = Self {
            io: FileIo {
                file,
                completed: VecDeque::new(),
                #[cfg(target_os = "linux")]
                ring: None,
            },
            log,
            anchor,
            owner_lock: Some(owner_lock),
            anchor_seq,
            fenced: false,
            cfg,
            recovery_start: recovery.sb.start_lsn,
            recovery_budget: max_recovery_bytes,
            retained_payload_bytes: used,
            last_payload_bytes,
        };
        result.persist_anchor(result.log.durable_lsn())?;
        Ok((result, payloads))
    }
    fn persist_anchor(&mut self, lsn: u64) -> io::Result<()> {
        let seq = self.anchor_seq.checked_add(1).ok_or_else(invalid)?;
        let mut b = [0; 64];
        b[..8].copy_from_slice(b"CMACK001");
        b[8..16].copy_from_slice(&seq.to_le_bytes());
        b[16..24].copy_from_slice(&lsn.to_le_bytes());
        b[24..32].copy_from_slice(&self.cfg.capacity.to_le_bytes());
        b[32..40].copy_from_slice(&(self.cfg.block as u64).to_le_bytes());
        let crc = crc32c(&[&b[..60]]);
        b[60..].copy_from_slice(&crc.to_le_bytes());
        self.anchor.write_all_at(&b, (seq % 2) * 64)?;
        self.anchor.sync_data()?;
        self.anchor.write_all_at(&b, ((seq ^ 1) % 2) * 64)?;
        self.anchor.sync_data()?;
        self.anchor_seq = seq;
        Ok(())
    }
    pub fn append(&mut self, payload: &[u8]) -> io::Result<u64> {
        if self.fenced {
            return Err(invalid());
        }
        if payload.len()
            > self
                .recovery_budget
                .saturating_sub(self.retained_payload_bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "recovery budget full; checkpoint required",
            ));
        }
        let lsn = match self
            .log
            .append(0, KIND_DATA, payload, None, 0, &mut self.io)
        {
            Ok(lsn) => lsn,
            Err(gclog::LogError::Full) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "WAL full; checkpoint required",
                ));
            }
            Err(gclog::LogError::TooLarge) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "WAL batch too large",
                ));
            }
            Err(error) => {
                self.fenced = true;
                return Err(io::Error::other(format!("WAL fenced: {error:?}")));
            }
        };
        let result = (|| {
            self.log.poll(u64::MAX, true, &mut self.io);
            loop {
                #[cfg(target_os = "linux")]
                if self.io.completed.is_empty() && self.log.durable_lsn() < lsn {
                    if let Some((ring, owner)) = &mut self.io.ring {
                        if *owner != std::thread::current().id() {
                            return Err(invalid());
                        }
                        ring.submit_and_wait(1)?;
                        ring.reap(|token, result| self.io.completed.push_back((token, result)));
                    }
                }
                let Some((token, res)) = self.io.completed.pop_front() else {
                    break;
                };
                // SAFETY: this exact completion follows the finished positional write and sync.
                unsafe {
                    self.log
                        .on_cqe(token, res, &mut self.io, &mut Vec::new(), &mut Vec::new())
                }
                .map_err(|_| invalid())?;
            }
            if self.log.durable_lsn() < lsn {
                return Err(invalid());
            }
            self.persist_anchor(lsn)
        })();
        if result.is_err() {
            self.fenced = true;
        }
        result.map(|_| {
            self.retained_payload_bytes += payload.len();
            self.last_payload_bytes = payload.len();
            lsn
        })
    }
    /// Enable native writes on the calling owner thread. Errors never select buffered fallback.
    #[cfg(target_os = "linux")]
    pub fn enable_uring(&mut self) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        if !self.log.quiescent() || self.io.ring.is_some() {
            return Err(invalid());
        }
        let mut ring = ciagomem_core::uring::UringIo::new(32)?;
        ring.register_files(&[self.io.file.as_raw_fd()])?;
        self.io.ring = Some((ring, std::thread::current().id()));
        Ok(())
    }
    pub fn is_fenced(&self) -> bool {
        self.fenced
    }
    pub fn recovery_start(&self) -> u64 {
        self.recovery_start
    }
    pub fn checkpointed(&mut self, lsn: u64) -> io::Result<()> {
        if self.fenced || lsn != self.log.durable_lsn() {
            return Err(invalid());
        }
        if let Some(sb) = self.log.truncation(lsn.checked_add(1).ok_or_else(invalid)?) {
            let mut block = vec![0; self.cfg.block];
            sb.encode(&mut block);
            let result = self
                .io
                .file
                .write_all_at(&block, sb.file_offset(self.cfg.block))
                .and_then(|_| self.io.file.sync_data());
            if let Err(error) = result {
                self.fenced = true;
                return Err(error);
            }
            self.log.superblock_durable(&sb);
        }
        // Each synchronous append seals one application batch. The latest buffer may
        // remain in the ring; charging its payload is conservative even after a reopen.
        self.retained_payload_bytes = self.last_payload_bytes;
        Ok(())
    }
    pub fn durable_lsn(&self) -> u64 {
        self.log.durable_lsn()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ciagomem-driver-{}",
            ciagomem_core::random_id().unwrap()
        ));
        std::fs::create_dir(&p).unwrap();
        p
    }
    fn config() -> LogConfig {
        LogConfig {
            block: 4096,
            buf_bytes: 16384,
            buffers: 2,
            max_inflight: 1,
            min_batch_bytes: 4096,
            max_delay_ns: 0,
            capacity: 262144,
            file: 0,
            tail: TailMode::Pad,
            max_retries: 0,
        }
    }
    #[test]
    fn real_write_failure_fences_driver_and_disallows_checkpoint() {
        let d = directory();
        let path = d.join("engine.wal");
        let (mut driver, _) = FileLog::open(&path, config(), 1 << 20).unwrap();
        driver.io.file = File::open(&path).unwrap();
        assert!(driver.append(b"unacknowledged").is_err());
        assert!(driver.is_fenced());
        assert!(driver.checkpointed(driver.durable_lsn()).is_err());
        drop(driver);
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn acknowledgement_failure_is_indeterminate_not_rollback() {
        let d = directory();
        let path = d.join("engine.wal");
        let (mut driver, _) = FileLog::open(&path, config(), 1 << 20).unwrap();
        driver.anchor = File::open(path.with_extension("ack")).unwrap();
        assert!(driver.append(b"written-before-lost-ack").is_err());
        assert!(driver.is_fenced());
        assert!(driver.checkpointed(driver.durable_lsn()).is_err());
        drop(driver);
        let (driver, recovered) = FileLog::open(&path, config(), 1 << 20).unwrap();
        assert_eq!(recovered, vec![b"written-before-lost-ack".to_vec()]);
        drop(driver);
        std::fs::remove_dir_all(d).unwrap();
    }
}
