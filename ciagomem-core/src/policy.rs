//! Per-request authorization over typed actor references. Ownership resolution is trusted input.
use ciagomem_crypto::VerifiedMacaroon;
use ciagomem_ql::{Query, Source, Stage};
pub const OP_READ: u64 = 1;
pub const OP_WRITE: u64 = 2;
/// Tenant administration only. No fleet/root authority can be represented by this bit.
pub const OP_OPERATOR: u64 = 4;
#[derive(Debug, Clone, Copy)]
pub struct DestinationRule<'a> {
    pub tenant: u128,
    pub prefix: &'a str,
    pub expected_owner: Option<&'a str>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryScope {
    Tenant(u128),
}
#[derive(Debug, PartialEq)]
pub struct ValidatedQuery<'q, 'a> {
    query: &'q Query<'a>,
    scope: QueryScope,
    session: u128,
    owner_version: u64,
}
impl<'q, 'a> ValidatedQuery<'q, 'a> {
    pub fn query(&self) -> &'q Query<'a> {
        self.query
    }
    pub fn scope(&self) -> QueryScope {
        self.scope
    }
    pub fn session_id(&self) -> u128 {
        self.session
    }
    pub fn owner_version(&self) -> u64 {
        self.owner_version
    }
}
pub struct AuthContext<'a> {
    pub session: &'a VerifiedMacaroon,
    pub owner_of: &'a dyn Fn(u64) -> Option<u128>,
    pub allowed_destinations: &'a [DestinationRule<'a>],
    pub owner_version: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum PolicyError {
    InvalidQuery,
    ForeignActor(u64),
    OperatorScopeRequired,
    OperationDenied,
    ForbiddenDestination,
    MutateOnScan,
    UnsupportedFrameResolution,
    FleetScopeRequired,
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
        bucket == b && (p.is_empty() || path == p || (p.ends_with('/') && path.starts_with(p)))
    })
}
pub fn validate_query<'q, 'a>(
    q: &'q Query<'a>,
    auth: &AuthContext<'_>,
) -> Result<ValidatedQuery<'q, 'a>, PolicyError> {
    ciagomem_ql::validate_query_ast(q).map_err(|_| PolicyError::InvalidQuery)?;
    let operator = auth.session.allows(OP_OPERATOR);
    let destination = |uri: &str| {
        auth.allowed_destinations.iter().any(|r| {
            r.tenant == auth.session.tenant_incarnation()
                && (!uri.starts_with("s3://") || r.expected_owner.is_some_and(|o| !o.is_empty()))
                && destination_allowed(uri, &[r.prefix])
        })
    };
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
        // Store-wide backup needs a separately rooted fleet capability and adapter.
        Source::BackupStore { .. } => return Err(PolicyError::FleetScopeRequired),
    }
    if !auth.session.allows(OP_READ) {
        return Err(PolicyError::OperationDenied);
    }
    for stage in &q.stages {
        match stage {
            Stage::HydrateFrame { .. } => return Err(PolicyError::UnsupportedFrameResolution),
            Stage::Mutate(_) | Stage::LinkAdd { .. } => {
                if matches!(q.source, Source::ScanAll | Source::ScanRange { .. }) {
                    return Err(PolicyError::MutateOnScan);
                }
                if !auth.session.allows(OP_WRITE) {
                    return Err(PolicyError::OperationDenied);
                }
            }
            Stage::ExportTo {
                destination: uri, ..
            } => {
                if !operator {
                    return Err(PolicyError::OperatorScopeRequired);
                }
                if !destination(uri) {
                    return Err(PolicyError::ForbiddenDestination);
                }
            }
            _ => {}
        }
    }
    // ASTs are public: validate even when they were constructed without the parser.
    ciagomem_ql::referenced_actors(q)
        .map_err(|_| PolicyError::InvalidQuery)?
        .into_iter()
        .try_for_each(check)?;
    Ok(ValidatedQuery {
        query: q,
        scope: QueryScope::Tenant(auth.session.tenant_incarnation()),
        session: auth.session.session_id(),
        owner_version: auth.owner_version,
    })
}
