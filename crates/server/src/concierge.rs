//! concierge over gRPC: the calls a relying party makes (spec §4). Redeeming a code, rotating
//! a refresh token and publishing the `sa` catalog authenticate with the panel's client
//! secret; `GetMe` with the user's access token, the only RPC that token opens.
//!
//! In development there may be no concierge at all: [`Concierge::dev`] answers the sign-in's
//! three calls for one made-up user (`PANEL_DEV_SIGN_IN`, refused outside development and off
//! loopback by the settings), so the sign-in, the sessions and the gate run as they would.

use std::{sync::Arc, time::Duration};

use jiff::{SignedDuration, Timestamp};
use sa_auth::{Catalog, PermissionSet};
use sa_contracts::concierge::v1::{
	CatalogAlias, ClientTokenResponse, ExchangeCodeRequest, GetMeRequest, PublishCatalogRequest, RefreshClientTokenRequest, UserProfile, auth_service_client::AuthServiceClient,
	user_directory_client::UserDirectoryClient,
};
use sa_panel::session::{RefreshError, Refresher, Tokens, random_token};
use sha2::{Digest, Sha256};
use tonic::{
	Code, Request, Status,
	metadata::MetadataValue,
	transport::{Channel, Endpoint},
};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The panel's client id at concierge.
pub const CLIENT_ID: &str = "sa";

/// The code `/auth/login` hands straight to the callback under [`Concierge::dev`]: there is no
/// concierge to issue one. Any other code is refused, as concierge would refuse it.
pub const DEV_CODE: &str = "dev-sign-in";

/// How long a call to concierge may take, connecting included.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// What a failed call to concierge means to the panel.
#[derive(Debug, thiserror::Error)]
pub enum ConciergeError {
	/// concierge answered no: UNAUTHENTICATED or PERMISSION_DENIED.
	#[error("concierge refused: {0:?}")]
	Refused(Code),
	/// concierge could not be reached, or did not answer in time.
	#[error("concierge is unavailable: {0}")]
	Unavailable(String),
	/// Anything else: a bug on one side or the other.
	#[error("concierge failed: {0}")]
	Failed(String),
}

impl From<Status> for ConciergeError {
	fn from(s: Status) -> Self {
		match s.code() {
			Code::Unauthenticated | Code::PermissionDenied => Self::Refused(s.code()),
			Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => Self::Unavailable(s.message().to_owned()),
			_ => Self::Failed(format!("{:?}: {}", s.code(), s.message())),
		}
	}
}

/// The signed-in user as concierge tells it (`GetMe`), what the panel needs of it.
#[derive(Clone, Debug)]
pub struct Me {
	pub user_id: Uuid,
	pub email: String,
	pub email_verified: bool,
	pub preferred_name: String,
	/// What the user may do in the panel: concrete `sa` permissions.
	pub permissions: PermissionSet,
}

impl TryFrom<UserProfile> for Me {
	type Error = ConciergeError;

	fn try_from(p: UserProfile) -> Result<Self, ConciergeError> {
		if let Some(stray) = p.permissions.iter().find(|perm| !perm.starts_with("sa:")) {
			return Err(ConciergeError::Failed(format!("GetMe answered `{stray}`, outside the sa namespace")));
		}
		Ok(Self {
			user_id: Uuid::parse_str(&p.user_id).map_err(|_| ConciergeError::Failed("GetMe answered a user id that is not a UUID".into()))?,
			email: p.email,
			email_verified: p.email_verified,
			preferred_name: p.preferred_name,
			permissions: p.permissions.into_iter().collect(),
		})
	}
}

/// A token pair and whose it is.
pub struct Issued {
	pub user_id: Uuid,
	pub tokens: Tokens,
}

#[derive(Clone)]
pub struct Concierge {
	backend: Backend,
}
impl Concierge {
	/// Connects on first use, so the panel starts (and serves ingest) while concierge is down.
	pub fn try_new(grpc_addr: &str, client_secret: &str) -> eyre::Result<Self> {
		let channel = Endpoint::from_shared(grpc_addr.to_owned())
			.map_err(|e| eyre::eyre!("CONCIERGE_GRPC_ADDR {grpc_addr:?}: {e}"))?
			.connect_timeout(CALL_TIMEOUT)
			.timeout(CALL_TIMEOUT)
			.connect_lazy();
		Ok(Self {
			backend: Backend::Grpc(Arc::new(Grpc {
				auth: AuthServiceClient::new(channel.clone()),
				directory: UserDirectoryClient::new(channel),
				secret: Zeroizing::new(client_secret.to_owned()),
			})),
		})
	}

	/// No concierge: every sign-in is `who`. Development only — the settings refuse
	/// `PANEL_DEV_SIGN_IN` in any other profile and on any origin but loopback.
	pub fn dev(who: DevIdentity) -> Self {
		Self {
			backend: Backend::Dev(Arc::new(who)),
		}
	}

	/// Whether this is [`Concierge::dev`]: `/auth/login` then skips the trip to concierge.
	pub fn is_dev(&self) -> bool {
		matches!(self.backend, Backend::Dev(_))
	}

	/// Redeems the code the browser brought back, with the verifier of the challenge sent to
	/// authorize.
	pub async fn exchange_code(&self, code: &str, redirect_uri: &str, verifier: &str) -> Result<Issued, ConciergeError> {
		let (auth, secret) = match &self.backend {
			Backend::Grpc(g) => (&g.auth, &g.secret),
			Backend::Dev(who) => {
				if code != DEV_CODE {
					return Err(ConciergeError::Refused(Code::Unauthenticated));
				}
				return Ok(Issued {
					user_id: who.user_id,
					tokens: who.tokens()?,
				});
			}
		};
		let answer = auth
			.clone()
			.exchange_code(ExchangeCodeRequest {
				client_id: CLIENT_ID.to_owned(),
				client_secret: secret.to_string(),
				code: code.to_owned(),
				redirect_uri: redirect_uri.to_owned(),
				code_verifier: verifier.to_owned(),
			})
			.await?
			.into_inner();
		issued(answer)
	}

	/// Tells concierge what the `sa` namespace defines. Refused when the catalog is older than
	/// the one it holds, or the same version with other content.
	pub async fn publish_catalog(&self, catalog: &Catalog) -> Result<(), ConciergeError> {
		let Backend::Grpc(g) = &self.backend else {
			unreachable!("dev sign-in has no concierge to publish to; serve does not ask it");
		};
		let aliases = catalog
			.aliases
			.iter()
			.map(|(name, members)| CatalogAlias {
				name: name.clone(),
				members: members.iter().cloned().collect(),
				delegates: catalog.delegations.get(name).map(|d| d.iter().cloned().collect()).unwrap_or_default(), // only aliases that delegate are in `delegations`
			})
			.collect();
		g.auth
			.clone()
			.publish_catalog(PublishCatalogRequest {
				client_id: CLIENT_ID.to_owned(),
				client_secret: g.secret.to_string(),
				version: catalog.version,
				permissions: catalog.permissions.iter().cloned().collect(),
				aliases,
			})
			.await?;
		Ok(())
	}

	/// The user behind an access token.
	pub async fn me(&self, access: &str) -> Result<Me, ConciergeError> {
		let directory = match &self.backend {
			Backend::Grpc(g) => &g.directory,
			// The token is the session's own, read from its sealed row: whoever holds the
			// session is the dev user.
			Backend::Dev(who) => return Ok(who.me()),
		};
		let mut req = Request::new(GetMeRequest {});
		let bearer: MetadataValue<_> = format!("Bearer {access}")
			.parse()
			.map_err(|_| ConciergeError::Failed("an access token that is not a header value".into()))?;
		req.metadata_mut().insert("authorization", bearer);
		directory.clone().get_me(req).await?.into_inner().try_into()
	}
}

/// Who [`Concierge::dev`] signs everyone in as.
#[derive(Clone, Debug)]
pub struct DevIdentity {
	pub user_id: Uuid,
	pub permissions: PermissionSet,
	pub email: String,
}
impl DevIdentity {
	/// The user of `email` in a database that has none yet: named by it, so a local database keeps
	/// one user per dev identity across restarts.
	pub fn named(email: &str) -> Uuid {
		let digest = Sha256::digest(format!("sa-panel/dev-sign-in/{email}"));
		uuid::Builder::from_custom_bytes(digest[..16].try_into().expect("SHA-256 is 32 bytes")).into_uuid()
	}

	/// What the panel shows as the user's name: the sidebar says that this is dev sign-in.
	pub fn display_name(&self) -> String {
		format!("Dev sign-in ({})", self.email)
	}

	fn me(&self) -> Me {
		Me {
			user_id: self.user_id,
			email: self.email.clone(),
			email_verified: true,
			preferred_name: self.display_name(),
			permissions: self.permissions.clone(),
		}
	}

	/// A pair as concierge would issue one; the access token outlives a working day, so a dev
	/// session rarely rotates (when it does, it rotates through [`Refresher`] as usual).
	fn tokens(&self) -> Result<Tokens, ConciergeError> {
		let now = Timestamp::now();
		let token = || random_token().map(Zeroizing::new).map_err(|e| ConciergeError::Failed(format!("{e:#}")));
		Ok(Tokens {
			access: token()?,
			access_expires_at: now + SignedDuration::from_hours(12),
			refresh: token()?,
			refresh_expires_at: now + SignedDuration::from_hours(24 * 30),
		})
	}
}

#[derive(Clone)]
enum Backend {
	Grpc(Arc<Grpc>),
	Dev(Arc<DevIdentity>),
}

struct Grpc {
	auth: AuthServiceClient<Channel>,
	directory: UserDirectoryClient<Channel>,
	secret: Zeroizing<String>,
}

impl std::fmt::Debug for Concierge {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.backend {
			Backend::Grpc(_) => f.debug_struct("Concierge").finish_non_exhaustive(),
			Backend::Dev(who) => f.debug_tuple("Concierge::Dev").field(who).finish(),
		}
	}
}

impl Refresher for Concierge {
	async fn refresh(&self, refresh_token: &str) -> Result<Tokens, RefreshError> {
		let (auth, secret) = match &self.backend {
			Backend::Grpc(g) => (&g.auth, &g.secret),
			Backend::Dev(who) => return who.tokens().map_err(|e| RefreshError::Failed(eyre::eyre!(e))),
		};
		let answer = auth
			.clone()
			.refresh_client_token(RefreshClientTokenRequest {
				client_id: CLIENT_ID.to_owned(),
				client_secret: secret.to_string(),
				refresh_token: refresh_token.to_owned(),
			})
			.await
			.map_err(ConciergeError::from)
			.and_then(|r| issued(r.into_inner()));
		match answer {
			Ok(issued) => Ok(issued.tokens),
			Err(ConciergeError::Refused(_)) => Err(RefreshError::Rejected),
			Err(ConciergeError::Unavailable(why)) => Err(RefreshError::Unavailable(why)),
			Err(e @ ConciergeError::Failed(_)) => Err(RefreshError::Failed(eyre::eyre!(e))),
		}
	}
}

fn issued(r: ClientTokenResponse) -> Result<Issued, ConciergeError> {
	let at = |secs: i64, what: &str| Timestamp::from_second(secs).map_err(|_| ConciergeError::Failed(format!("{what} out of range")));
	Ok(Issued {
		user_id: Uuid::parse_str(&r.user_id).map_err(|_| ConciergeError::Failed("a token pair for a user id that is not a UUID".into()))?,
		tokens: Tokens {
			access_expires_at: at(r.access_expires_at, "access_expires_at")?,
			refresh_expires_at: at(r.refresh_expires_at, "refresh_expires_at")?,
			access: Zeroizing::new(r.access_token),
			refresh: Zeroizing::new(r.refresh_token),
		},
	})
}
