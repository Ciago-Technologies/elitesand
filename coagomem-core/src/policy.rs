//! Per-request authorization over typed actor references. Ownership resolution is trusted input.
use ciagomem_crypto::VerifiedMacaroon;
use coagomem_ql::{Query, Source, Stage};
pub const OP_READ: u64 = 1;
pub const OP_WRITE: u64 = 2;
pub const OP_OPERATOR: u64 = 4;
pub struct AuthContext<'a> {
    pub session: &'a VerifiedMacaroon,
    pub owner_of: &'a dyn Fn(u64) -> Option<u128>,
    pub allowed_destinations: &'a [&'a str],
}
#[derive(Debug, PartialEq, Eq)]
pub enum PolicyError {
    InvalidQuery,
    ForeignActor(u64),
    OperatorScopeRequired,
    OperationDenied,
    ForbiddenDestination,
    MutateOnScan,
}
/// Strict canonical S3 bucket/path or absolute backup path. Reject ambiguous encodings outright.
fn destination_parts(s: &str) -> Option<(&str, &str)> {
    if s.contains(['%', '\\', '?', '#', '@'])
        || s.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let (authority, path) = if let Some(s) = s.strip_prefix("s3://") {
        let (bucket, path) = s.split_once('/')?;
        if bucket.len() < 3
            || bucket.len() > 63
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || bucket.starts_with(['.', '-'])
            || bucket.ends_with(['.', '-'])
            || bucket.contains("..")
            || bucket.parse::<std::net::Ipv4Addr>().is_ok()
        {
            return None;
        }
        (bucket, path)
    } else {
        ("", s.strip_prefix("/var/backups/ciagomem/")?)
    };
    if path.split('/').any(|part| part == "." || part == "..") || path.contains("//") {
        return None;
    }
    Some((authority, path))
}
pub fn destination_allowed(destination: &str, allowlist: &[&str]) -> bool {
    let Some((bucket, path)) = destination_parts(destination) else {
        return false;
    };
    allowlist.iter().any(|prefix| {
        let Some((b, p)) = destination_parts(prefix) else {
            return false;
        };
        bucket == b && (path == p || (p.ends_with('/') && path.starts_with(p)))
    })
}
pub fn validate_query(q: &Query<'_>, auth: &AuthContext<'_>) -> Result<(), PolicyError> {
    coagomem_ql::validate_query_ast(q).map_err(|_| PolicyError::InvalidQuery)?;
    let operator = auth.session.allows(OP_OPERATOR);
    let check = |id| {
        if (auth.owner_of)(id) == Some(auth.session.tenant_incarnation()) {
            Ok(())
        } else {
            Err(PolicyError::ForeignActor(id))
        }
    };
    match &q.source {
        Source::Actor(id) => check(*id)?,
        Source::TxActors(ids) => {
            for id in ids {
                check(*id)?
            }
        }
        Source::ScanAll | Source::ScanRange { .. } => {
            if !operator {
                return Err(PolicyError::OperatorScopeRequired);
            }
        }
        Source::BackupStore { destination } => {
            if !operator {
                return Err(PolicyError::OperatorScopeRequired);
            }
            if !destination_allowed(destination, auth.allowed_destinations) {
                return Err(PolicyError::ForbiddenDestination);
            }
        }
    }
    if !auth.session.allows(OP_READ) {
        return Err(PolicyError::OperationDenied);
    }
    for stage in &q.stages {
        match stage {
            Stage::Mutate(_) => {
                if matches!(q.source, Source::ScanAll | Source::ScanRange { .. }) {
                    return Err(PolicyError::MutateOnScan);
                }
                if !auth.session.allows(OP_WRITE) {
                    return Err(PolicyError::OperationDenied);
                }
            }
            Stage::ExportTo { destination, .. } => {
                if !operator {
                    return Err(PolicyError::OperatorScopeRequired);
                }
                if !destination_allowed(destination, auth.allowed_destinations) {
                    return Err(PolicyError::ForbiddenDestination);
                }
            }
            _ => {}
        }
    }
    // ASTs are public: validate even when they were constructed without the parser.
    coagomem_ql::referenced_actors(q)
        .into_iter()
        .try_for_each(check)
}
