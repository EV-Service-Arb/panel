//! Google's answers on disk, one file per request, named by the request: a paid answer is never
//! asked for twice. Only answers are kept: one cached refusal would answer for its request
//! forever.

use std::{
	io::ErrorKind,
	path::PathBuf,
	sync::atomic::{AtomicU64, Ordering},
	time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};

use crate::Error;

/// A request as the cache tells it from another: the field mask changes the answer's shape.
pub(crate) struct Request<'a> {
	pub method: &'a str,
	pub url: &'a str,
	pub mask: &'a str,
	pub body: &'a str,
}

#[derive(Debug)]
pub(crate) struct Cache(pub PathBuf);

impl Cache {
	fn path(&self, r: &Request<'_>, ext: &str) -> PathBuf {
		let key = Sha256::digest(format!("{}\n{}\n{}\n{}", r.method, r.url, r.mask, r.body));
		self.0.join(format!("{}.{ext}", hex::encode(key)))
	}

	/// The answer to `r`, unless it is older than `max_age`.
	pub async fn read(&self, r: &Request<'_>, max_age: Option<Duration>) -> Result<Option<serde_json::Value>, Error> {
		let path = self.path(r, "json");
		let text = match tokio::fs::read_to_string(&path).await {
			Ok(t) => t,
			Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
			Err(e) => return Err(Error::Cache(path, e)),
		};
		if let Some(max) = max_age {
			let written = tokio::fs::metadata(&path).await.and_then(|m| m.modified()).map_err(|e| Error::Cache(path.clone(), e))?;
			// an mtime ahead of now is a clock that moved: the answer is not old
			if SystemTime::now().duration_since(written).is_ok_and(|age| age > max) {
				return Ok(None);
			}
		}
		let json = serde_json::from_str(&text).map_err(|e| Error::Cache(path, std::io::Error::new(ErrorKind::InvalidData, e)))?;
		Ok(Some(json))
	}

	/// Keeps a paid answer for `r`. One that is not JSON is kept beside the cache, as `.rejected`.
	pub async fn write(&self, r: &Request<'_>, text: &str) -> Result<serde_json::Value, Error> {
		tokio::fs::create_dir_all(&self.0).await.map_err(|e| Error::Cache(self.0.clone(), e))?;
		let json = match serde_json::from_str(text) {
			Ok(j) => j,
			Err(e) => {
				let kept = self.path(r, "rejected");
				tokio::fs::write(&kept, text).await.map_err(|e| Error::Cache(kept.clone(), e))?;
				return Err(Error::Api {
					asked: format!("{} {}", r.method, r.url),
					status: 200,
					body: format!("not JSON ({e}); the paid answer is kept at {}", kept.display()),
				});
			}
		};
		static PARTS: AtomicU64 = AtomicU64::new(0);
		let dst = self.path(r, "json");
		let part = self.path(r, &format!("{}.{}.part", std::process::id(), PARTS.fetch_add(1, Ordering::Relaxed)));
		// renamed into place: an interrupted write never leaves a truncated answer to be read
		tokio::fs::write(&part, text).await.map_err(|e| Error::Cache(part.clone(), e))?;
		tokio::fs::rename(&part, &dst).await.map_err(|e| Error::Cache(dst, e))?;
		Ok(json)
	}
}
