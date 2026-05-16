//! Delta pager: `/drives/{id}/root/delta` with `nextLink` and `deltaLink` handling.

use onesync_protocol::{
    primitives::{DeltaCursor, DriveId},
    remote::DeltaPage,
};
use serde::Deserialize;

use crate::error::GraphInternalError;
use crate::items::RemoteItem;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Raw wire shape of a `/delta` response page.
#[derive(Debug, Deserialize)]
struct DeltaResponse {
    #[serde(rename = "value")]
    items: Vec<RemoteItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

/// Fetch one page of delta results.
///
/// - Pass `cursor = None` for the initial scan (no `?token=` query parameter).
/// - Follow [`DeltaPage::next_link`] until it is `None`; the terminal page carries
///   [`DeltaPage::delta_token`].
///
/// # Errors
///
/// - [`GraphInternalError::ResyncRequired`] when the server signals the cursor is too old.
/// - [`GraphInternalError::Unauthorized`] / other variants on HTTP errors.
pub async fn delta_page(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    cursor: Option<&DeltaCursor>,
) -> Result<DeltaPage, GraphInternalError> {
    let url = cursor.map_or_else(
        || format!("{GRAPH_BASE}/drives/{}/root/delta", drive_id.as_str()),
        |c| {
            format!(
                "{GRAPH_BASE}/drives/{}/root/delta?token={}",
                drive_id.as_str(),
                c.as_str()
            )
        },
    );
    fetch_delta_page(http, token, &url).await
}

/// Fetch a delta page from an arbitrary URL (used for `nextLink` follow-through).
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn fetch_delta_page(
    http: &reqwest::Client,
    token: &str,
    url: &str,
) -> Result<DeltaPage, GraphInternalError> {
    let request_id = new_request_id();
    let resp = http
        .get(url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    // 410 signals resyncRequired.
    if resp.status() == reqwest::StatusCode::GONE {
        return Err(GraphInternalError::ResyncRequired {
            request_id: request_id.clone(),
        });
    }

    let checked = crate::client::check_status(resp, &request_id).await?;
    let raw: DeltaResponse = checked
        .json()
        .await
        .map_err(|e| GraphInternalError::Decode {
            detail: e.to_string(),
        })?;

    // Extract the delta token from the deltaLink URL's `?token=` query parameter.
    let delta_token = raw.delta_link.as_deref().and_then(extract_token_param);

    Ok(DeltaPage {
        items: raw.items,
        next_link: raw.next_link,
        delta_token: delta_token.map(DeltaCursor::new),
    })
}

/// RP2-F8: extract the delta token from a Graph `@odata.deltaLink` URL.
///
/// Uses `url::Url::parse` + `query_pairs()` so the returned value is
/// URL-decoded (a token containing `%3D` is restored to `=` before we
/// persist it; without this, passing it back as `?token=…` round-trips
/// through Graph as a double-encoded value and the server returns
/// `resyncRequired`).
///
/// Accepts both the canonical `token=` parameter and the legacy
/// `$deltaToken=` spelling Graph occasionally emits.
///
/// Returns `None` only when neither the URL nor the parameter parses —
/// the caller (`delta_page`) treats `None` as "no cursor available",
/// causing the next cycle to do a full resync. With the looser parser
/// this should be rare in practice.
fn extract_token_param(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    for (key, value) in parsed.query_pairs() {
        if key == "token" || key == "$deltaToken" {
            return Some(value.into_owned());
        }
    }
    None
}

fn new_request_id() -> String {
    let mut buf = [0u8; 8];
    // LINT: getrandom failure is unrecoverable.
    #[allow(clippy::expect_used)]
    getrandom::getrandom(&mut buf).expect("getrandom");
    buf.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[allow(dead_code)]
    fn drive(s: &str) -> DriveId {
        DriveId::new(s)
    }

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    #[tokio::test]
    async fn single_page_with_delta_token() {
        let server = MockServer::start().await;
        let delta_link = format!("{}/drives/drv1/root/delta?token=cursor-abc", server.uri());
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "item-1", "name": "file1.txt", "size": 100 }
                ],
                "@odata.deltaLink": delta_link
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/root/delta", server.uri());
        let page = fetch_delta_page(&http, "tok", &url).await.unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "item-1");
        assert!(page.next_link.is_none());
        assert!(page.delta_token.is_some());
        assert_eq!(page.delta_token.unwrap().as_str(), "cursor-abc");
    }

    #[tokio::test]
    async fn multi_page_with_next_link() {
        let server = MockServer::start().await;

        // Page 1: no skiptoken → returns nextLink
        let next_link = format!("{}/drives/drv1/root/delta?$skiptoken=skip1", server.uri());
        // More specific matcher (with skiptoken) must be registered FIRST so wiremock
        // prefers it when both matchers could apply.
        let delta_link = format!("{}/drives/drv1/root/delta?token=final-cursor", server.uri());
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root/delta"))
            .and(query_param("$skiptoken", "skip1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "id": "item-2", "name": "b.txt", "size": 20 }],
                "@odata.deltaLink": delta_link
            })))
            .mount(&server)
            .await;

        // Page 2: falls through to the broader matcher
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "id": "item-1", "name": "a.txt", "size": 10 }],
                "@odata.nextLink": next_link
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/root/delta", server.uri());

        let page1 = fetch_delta_page(&http, "tok", &url).await.unwrap();
        assert_eq!(page1.items.len(), 1);
        assert!(page1.next_link.is_some());
        assert!(page1.delta_token.is_none());

        let page2 = fetch_delta_page(&http, "tok", page1.next_link.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 1);
        assert_eq!(page2.items[0].id, "item-2");
        assert!(page2.next_link.is_none());
        assert_eq!(
            page2.delta_token.as_ref().map(DeltaCursor::as_str),
            Some("final-cursor")
        );
    }

    #[tokio::test]
    async fn tombstone_items_have_deleted_flag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "item-del",
                        "name": "deleted.txt",
                        "size": 0,
                        "deleted": {}
                    }
                ],
                "@odata.deltaLink": format!("{}/drives/drv1/root/delta?token=t1", server.uri())
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/root/delta", server.uri());
        let page = fetch_delta_page(&http, "tok", &url).await.unwrap();

        assert_eq!(page.items.len(), 1);
        assert!(
            page.items[0].is_deleted(),
            "tombstone item should be marked deleted"
        );
    }

    #[tokio::test]
    async fn gone_410_returns_resync_required() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root/delta"))
            .respond_with(ResponseTemplate::new(410).set_body_json(serde_json::json!({
                "error": { "code": "resyncRequired", "message": "Resync required." }
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/root/delta", server.uri());
        let err = fetch_delta_page(&http, "tok", &url).await.unwrap_err();
        assert!(
            matches!(err, GraphInternalError::ResyncRequired { .. }),
            "expected ResyncRequired, got: {err:?}"
        );
    }
}
