//! `GET /api/v1/sources/sites`: every active site key's own report (`<origin>/.well-known/sa-site`)
//! beside what sites saw: the last ingest per key, the last internal read per brand. All asked at
//! once; whatever fails is a value in its place, not the page's error.

use std::{collections::BTreeMap, time::Duration};

use axum::{Json, extract::State};
use eyre::WrapErr;
use jiff::Timestamp;
use sa_auth::SourceKind;
use sa_core::ids::SiteOrigin;
use sa_panel::store::sources::SourceRow;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{api::ApiResult, forward::Services, http::ApiState};

const REPORT_PATH: &str = "/.well-known/sa-site";
const REPORT_MAX: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const SITES_TIMEOUT: Duration = Duration::from_secs(5);

/// The client the landings are asked with: their answer as it comes, never a redirect's.
pub(crate) fn probe_client() -> reqwest::Client {
	reqwest::Client::builder()
		.connect_timeout(Duration::from_secs(3))
		.timeout(PROBE_TIMEOUT)
		.redirect(reqwest::redirect::Policy::none())
		.build()
		.expect("the TLS backend initialises at boot")
}

/// One source of a landing's, as it measured it (kitstart's `siteReportRoute`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SourceState {
	Unset,
	Live,
	Empty,
	Withdrawn,
	Down { detail: String },
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportSources {
	places: BTreeMap<String, SourceState>,
	pricing: SourceState,
	experiments: SourceState,
}

#[derive(Debug, Deserialize, Serialize)]
struct SiteReport {
	brand: String,
	kitstart: String,
	checked_at: Timestamp,
	sources: ReportSources,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Probe {
	NoOrigin,
	Reported { report: SiteReport },
	Failed { detail: String },
}

#[derive(Debug, Serialize)]
struct Site {
	key_id: String,
	brands: Vec<String>,
	origin: Option<String>,
	report: Probe,
}

/// sites' `/api/conductor/internal-reads`: an endpoint is absent when not read since `since`.
#[derive(Debug, Deserialize, Serialize)]
struct InternalReads {
	since: Timestamp,
	reads: BTreeMap<String, BrandReads>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BrandReads {
	locations: Option<Timestamp>,
	pricing: Option<Timestamp>,
	experiments: Option<Timestamp>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Read<T> {
	Read { value: T },
	Failed { detail: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct ConnectedSites {
	sites: Vec<Site>,
	received: Read<BTreeMap<String, Timestamp>>,
	internal_reads: Read<InternalReads>,
}

pub(crate) async fn sites(State(s): State<ApiState>) -> ApiResult<Json<ConnectedSites>> {
	let keys: Vec<SourceRow> = s
		.panel
		.store()
		.sources()
		.await?
		.into_iter()
		.filter(|k| k.grant.kind == SourceKind::Site && k.revoked_at.is_none())
		.collect();
	let reports = futures::future::join_all(keys.iter().map(|k| probe(&s.probe, k)));
	let received = sites_read(s.sites.as_ref(), "/api/conductor/sources/received");
	let internal_reads = sites_read(s.sites.as_ref(), "/api/conductor/internal-reads");
	let (reports, received, internal_reads) = tokio::join!(reports, received, internal_reads);
	let sites = keys
		.into_iter()
		.zip(reports)
		.map(|(k, report)| Site {
			brands: k.grant.brands.iter().map(|b| b.as_str().to_owned()).collect(),
			key_id: k.grant.key_id,
			origin: k.origin.map(|o| o.to_string()),
			report,
		})
		.collect();
	Ok(Json(ConnectedSites { sites, received, internal_reads }))
}

async fn sites_read<T: DeserializeOwned>(sites: Option<&Services>, path: &str) -> Read<T> {
	let Some(sites) = sites else {
		return Read::Failed {
			detail: "the forward is not configured: sites is not asked".into(),
		};
	};
	match tokio::time::timeout(SITES_TIMEOUT, sites.conductor_get(path)).await {
		Ok(Ok(value)) => Read::Read { value },
		Ok(Err(e)) => Read::Failed { detail: format!("{e:#}") },
		Err(_) => Read::Failed {
			detail: format!("sites did not answer {path} within {SITES_TIMEOUT:?}"),
		},
	}
}

async fn probe(client: &reqwest::Client, key: &SourceRow) -> Probe {
	let Some(origin) = &key.origin else { return Probe::NoOrigin };
	match fetch(client, origin).await {
		Ok(report) if key.grant.brands.iter().any(|b| b.as_str() == report.brand) => Probe::Reported { report },
		Ok(report) => Probe::Failed {
			detail: format!("reports brand {}, not one of the key's", report.brand),
		},
		Err(e) => Probe::Failed { detail: format!("{e:#}") },
	}
}

async fn fetch(client: &reqwest::Client, origin: &SiteOrigin) -> eyre::Result<SiteReport> {
	let mut res = client.get(format!("{origin}{REPORT_PATH}")).header(reqwest::header::ACCEPT, "application/json").send().await?;
	eyre::ensure!(res.status() == reqwest::StatusCode::OK, "{REPORT_PATH} answered {}", res.status());
	let mut body = Vec::new();
	while let Some(chunk) = res.chunk().await? {
		body.extend_from_slice(&chunk);
		eyre::ensure!(body.len() <= REPORT_MAX, "a report over {REPORT_MAX} bytes");
	}
	serde_json::from_slice(&body).wrap_err("an unreadable report")
}
