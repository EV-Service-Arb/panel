//! Access requests: a signed-in user who lacks a permission asks the admins for it, who grant
//! it in concierge's cabinet; the `access_requested` Telegram rule tells them. Which needs
//! exist and whether the user holds one is the caller's to judge, against the `sa` catalog.

use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

pub use crate::store::access::AccessRequest;
use crate::{Panel, store::access as db};

/// A request stands this long: asked again within it, nothing changes and nobody is told again.
pub const ASK_AGAIN_AFTER: SignedDuration = SignedDuration::from_hours(24);

impl Panel {
	/// The user's request for `need`: the standing one (`false`), or a new one made at `now` (`true`).
	pub async fn request_access(&self, user: Uuid, email: &str, name: &str, need: &str, now: Timestamp) -> eyre::Result<(AccessRequest, bool)> {
		let now = crate::store::from_db(crate::store::to_db(now))?; // as it will read back
		let mut tx = self.store.begin_write().await?;
		if let Some(standing) = db::of_user(&mut tx, user).await?.into_iter().find(|r| r.need == need && now < r.requested_at + ASK_AGAIN_AFTER) {
			return Ok((standing, false));
		}
		let request = AccessRequest {
			user_id: user,
			need: need.to_owned(),
			email: email.to_owned(),
			name: name.to_owned(),
			requested_at: now,
			event_id: crate::alerts::derived_event_id(&["access_requested", &user.to_string(), need, &now.to_string()]),
		};
		db::put(&mut tx, &request).await?;
		tx.commit().await.wrap_err("committing an access request")?;
		tracing::info!(user_id = %user, need, "access requested");
		Ok((request, true))
	}

	/// The user's requests still standing: those whose need they now `hold` are dropped.
	pub async fn pending_access(&self, user: Uuid, holds: impl Fn(&str) -> bool) -> eyre::Result<Vec<AccessRequest>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for access requests")?;
		let (held, pending): (Vec<_>, Vec<_>) = db::of_user(&mut conn, user).await?.into_iter().partition(|r| holds(&r.need));
		for r in held {
			db::delete(&mut conn, user, &r.need).await?;
		}
		Ok(pending)
	}
}
