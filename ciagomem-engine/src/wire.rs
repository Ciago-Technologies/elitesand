//! Protocol3: single-node fixed-Record profile. Explicit framing; no BFBS/cluster claim.
use crate::{
    host::{Operation, Output, Peer},
    retry::{RequestKey, WriteCommand},
    *,
};
use ciagomem_core::{Caveat, Label, Macaroon, envelope::crc32c};
pub const HEADER: usize = 18;
pub const PREFIX: usize = 88;
pub const RESPONSE: usize = 32;
pub const VERSION: u8 = 3;
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub op: u8,
    pub flags: u16,
    pub stream: u32,
    pub length: usize,
}
impl Header {
    pub fn decode(bytes: &[u8], limit: usize) -> Result<Self> {
        if bytes.len() != HEADER
            || u16::from_le_bytes(bytes[..2].try_into().unwrap()) != 0xc1a0
            || bytes[2] != VERSION
            || crc32c(&[&bytes[..14]]) != u32::from_le_bytes(bytes[14..18].try_into().unwrap())
        {
            return Err(Error::InvalidQuery);
        }
        let header = Self {
            op: bytes[3],
            flags: u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
            stream: u32::from_le_bytes(bytes[6..10].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize,
        };
        if header.stream == 0 || header.length > limit || header.flags & !3 != 0 {
            return Err(Error::InvalidQuery);
        }
        Ok(header)
    }
    pub fn frame(self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() != self.length || payload.len() > u32::MAX as usize {
            return Err(Error::Budget);
        }
        let mut out = vec![0; HEADER];
        out[..2].copy_from_slice(&0xc1a0u16.to_le_bytes());
        out[2] = VERSION;
        out[3] = self.op;
        out[4..6].copy_from_slice(&self.flags.to_le_bytes());
        out[6..10].copy_from_slice(&self.stream.to_le_bytes());
        out[10..14].copy_from_slice(&(self.length as u32).to_le_bytes());
        let crc = crc32c(&[&out[..14]]);
        out[14..].copy_from_slice(&crc.to_le_bytes());
        out.extend(payload);
        Ok(out)
    }
}
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            return Err(Error::InvalidQuery);
        }
        let (v, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(v)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
}
pub struct Request {
    pub token: Macaroon,
    pub peer: Peer,
    pub operation: Operation,
    pub timeout_ms: u32,
}
pub fn decode_request(header: Header, payload: &[u8], uid: u32, boot: u128) -> Result<Request> {
    if header.flags != 0
        || payload.len() != header.length
        || payload.len() < PREFIX
        || payload[0] != 3
        || payload[1] != 1
    {
        return Err(Error::InvalidQuery);
    }
    let token_len = u16::from_le_bytes(payload[2..4].try_into().unwrap()) as usize;
    let key_len = u32::from_le_bytes(payload[12..16].try_into().unwrap()) as usize;
    let body_len = u32::from_le_bytes(payload[84..88].try_into().unwrap()) as usize;
    if token_len == 0
        || token_len > 16384
        || key_len > 1024
        || PREFIX
            .checked_add(token_len)
            .and_then(|n| n.checked_add(key_len))
            .and_then(|n| n.checked_add(body_len))
            != Some(payload.len())
        || payload[4..12].iter().any(|b| *b != 0)
    {
        return Err(Error::InvalidQuery);
    }
    let client = u64::from_le_bytes(payload[16..24].try_into().unwrap());
    let sequence = u64::from_le_bytes(payload[24..32].try_into().unwrap());
    let tenant = u128::from_le_bytes(payload[32..48].try_into().unwrap());
    let expected_boot = u128::from_le_bytes(payload[48..64].try_into().unwrap());
    let incarnation = u128::from_le_bytes(payload[64..80].try_into().unwrap());
    let timeout_ms = u32::from_le_bytes(payload[80..84].try_into().unwrap());
    if expected_boot != boot || tenant == 0 || client == 0 || timeout_ms == 0 || timeout_ms > 300000
    {
        return Err(Error::Permission);
    }
    let token = Macaroon::from_bytes(&payload[PREFIX..PREFIX + token_len])
        .map_err(|_| Error::Permission)?;
    if token.caveats.iter().find_map(|c| {
        if let Caveat::ClientId(id) = c {
            Some(*id)
        } else {
            None
        }
    }) != Some(client)
    {
        return Err(Error::Permission);
    }
    let session = token
        .caveats
        .iter()
        .find_map(|c| {
            if let Caveat::Session(id) = c {
                Some(*id)
            } else {
                None
            }
        })
        .ok_or(Error::Permission)?;
    let key = std::str::from_utf8(&payload[PREFIX + token_len..PREFIX + token_len + key_len])
        .map_err(|_| Error::InvalidQuery)?;
    let actor = if key.is_empty() {
        None
    } else {
        let id = key.parse::<u64>().map_err(|_| Error::InvalidQuery)?;
        if id == 0 || id.to_string() != key {
            return Err(Error::InvalidQuery);
        }
        Some(ActorRef { id, incarnation })
    };
    let mut body = Cursor(&payload[PREFIX + token_len + key_len..]);
    if body.take(1)? != [1] {
        return Err(Error::InvalidQuery);
    }
    let request_key = RequestKey {
        tenant,
        client,
        sequence,
    };
    let operation = match header.op {
        4 | 5 => {
            let expected_version = if header.op == 5 { body.u64()? } else { 0 };
            let length = body.u32()?;
            if length > 65344 {
                return Err(Error::Budget);
            }
            let bytes = body.take(length)?;
            let root = image::view(bytes)?;
            if root.integrity() != 0
                || root.sources() != 1
                || root.reader_lo().len() != 1
                || root.reader_lo().get(0) != (tenant as u64)
                || root.reader_hi().get(0) != ((tenant >> 64) as u64)
            {
                return Err(Error::Permission);
            }
            let record = crate::persistence::decode_record(
                bytes,
                Label::external(1, vec![tenant]).map_err(|_| Error::Permission)?,
                &Limits::default(),
            )?;
            if record.tenant != tenant {
                return Err(Error::Permission);
            }
            let command = if header.op == 4 {
                if actor.is_some() || incarnation != 0 {
                    return Err(Error::InvalidQuery);
                }
                WriteCommand::Create(record)
            } else {
                if actor != Some(record.actor) || incarnation == 0 {
                    return Err(Error::StaleReference);
                }
                WriteCommand::Replace {
                    record,
                    expected_version,
                }
            };
            Operation::WriteRetry {
                key: request_key,
                command,
            }
        }
        6 => {
            let minimum_version = body.u64()?;
            let actor = actor
                .filter(|a| a.incarnation != 0)
                .ok_or(Error::InvalidQuery)?;
            Operation::Read {
                actor,
                minimum_version,
            }
        }
        7 => {
            let expected_version = body.u64()?;
            let n = body.u32()?;
            if n > 65536 {
                return Err(Error::Budget);
            }
            let query = std::str::from_utf8(body.take(n)?)
                .map_err(|_| Error::Parse)?
                .into();
            let actor = actor
                .filter(|a| a.incarnation != 0)
                .ok_or(Error::InvalidQuery)?;
            Operation::WriteRetry {
                key: request_key,
                command: WriteCommand::Mutate {
                    actor,
                    expected_version,
                    query,
                },
            }
        }
        8 => {
            let expected_version = body.u64()?;
            let actor = actor
                .filter(|a| a.incarnation != 0)
                .ok_or(Error::InvalidQuery)?;
            Operation::WriteRetry {
                key: request_key,
                command: WriteCommand::Delete {
                    actor,
                    expected_version,
                },
            }
        }
        9 => {
            if actor.is_some() || incarnation != 0 {
                return Err(Error::InvalidQuery);
            }
            let n = body.u32()?;
            if n > 65536 {
                return Err(Error::Budget);
            }
            Operation::ReadQuery(
                std::str::from_utf8(body.take(n)?)
                    .map_err(|_| Error::Parse)?
                    .into(),
            )
        }
        _ => return Err(Error::Unsupported),
    };
    if !body.0.is_empty() || (matches!(operation, Operation::WriteRetry { .. }) != (sequence != 0))
    {
        return Err(Error::InvalidQuery);
    }
    Ok(Request {
        token,
        peer: Peer {
            tenant,
            session,
            uid,
        },
        operation,
        timeout_ms,
    })
}
pub fn response(
    op: u8,
    stream: u32,
    boot: u128,
    status: u16,
    kind: u8,
    sequence: u64,
    body: &[u8],
    limit: usize,
) -> Result<Vec<u8>> {
    if body.len().saturating_add(RESPONSE) > limit {
        return Err(Error::Budget);
    }
    let mut payload = vec![0; RESPONSE];
    payload[..2].copy_from_slice(&status.to_le_bytes());
    payload[2] = if status == 0 || kind == 1 { 1 } else { 0 };
    payload[3] = kind;
    payload[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
    payload[8..16].copy_from_slice(&sequence.to_le_bytes());
    payload[16..32].copy_from_slice(&boot.to_le_bytes());
    payload.extend(body);
    Header {
        op,
        stream,
        flags: 1 | if status != 0 { 2 } else { 0 },
        length: payload.len(),
    }
    .frame(&payload)
}
pub fn encode_output(
    op: u8,
    stream: u32,
    boot: u128,
    result: &Result<Output>,
    limit: usize,
) -> Result<Vec<u8>> {
    match result {
        Err(error) => response(
            op,
            stream,
            boot,
            crate::retry::status(error),
            0,
            0,
            &[],
            limit,
        ),
        Ok(Output::Receipt(r)) => {
            let mut body = Vec::new();
            body.extend(r.key.tenant.to_le_bytes());
            body.extend(r.key.client.to_le_bytes());
            body.extend(r.key.sequence.to_le_bytes());
            body.extend(r.digest);
            body.extend(&r.result);
            response(
                op,
                stream,
                boot,
                r.status,
                1,
                r.commit_sequence,
                &body,
                limit,
            )
        }
        Ok(Output::Document { snapshot, version }) => {
            response(op, stream, boot, 0, 2, *version, &snapshot.image, limit)
        }
        Ok(Output::Query(result)) => {
            let mut body = Vec::new();
            body.extend((result.rows.len() as u32).to_le_bytes());
            for row in &result.rows {
                if !row.joins.is_empty() {
                    return Err(Error::Unsupported);
                }
                let record = Record::new(
                    row.actor.unwrap_or(ActorRef {
                        id: 0,
                        incarnation: 0,
                    }),
                    0,
                    row.values.clone(),
                    row.label.clone(),
                );
                let bytes = image::encode(&record);
                if body.len().saturating_add(4 + bytes.len() + RESPONSE) > limit {
                    return Err(Error::Budget);
                }
                body.extend((bytes.len() as u32).to_le_bytes());
                body.extend(bytes);
            }
            response(op, stream, boot, 0, 3, 0, &body, limit)
        }
        _ => response(op, stream, boot, 0, 0, 0, &[], limit),
    }
}
