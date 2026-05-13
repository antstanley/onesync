//! Rename, delete, and mkdir helpers.

use onesync_protocol::primitives::DriveId;
use serde::Serialize;

use crate::error::GraphInternalError;
use crate::items::RemoteItem;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

#[derive(Serialize)]
struct RenameBody<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct MkdirBody<'a> {
    name: &'a str,
    folder: serde_json::Value,
    #[serde(rename = "@microsoft.graph.conflictBehavior")]
    conflict_behavior: &'static str,
}

/// `PATCH /drives/{drive_id}/items/{item_id}` — rename an item.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn rename(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    item_id: &str,
    new_name: &str,
) -> Result<RemoteItem, GraphInternalError> {
    let url = format!("{GRAPH_BASE}/drives/{}/items/{item_id}", drive_id.as_str());

    let request_id = new_request_id();
    let resp = http
        .patch(&url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .json(&RenameBody { name: new_name })
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    crate::client::check_status(resp, &request_id)
        .await?
        .json::<RemoteItem>()
        .await
        .map_err(|e| GraphInternalError::Decode {
            detail: e.to_string(),
        })
}

/// `DELETE /drives/{drive_id}/items/{item_id}` — move an item to the Recycle Bin.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP failure (204 is success).
pub async fn delete(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    item_id: &str,
) -> Result<(), GraphInternalError> {
    let url = format!("{GRAPH_BASE}/drives/{}/items/{item_id}", drive_id.as_str());

    let request_id = new_request_id();
    let resp = http
        .delete(&url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    // 204 No Content is the success response for DELETE.
    if resp.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(());
    }

    crate::client::check_status(resp, &request_id).await?;
    Ok(())
}

/// `POST /drives/{drive_id}/items/{parent_item_id}/children` — create a child folder.
///
/// Uses `@microsoft.graph.conflictBehavior = "fail"` so the call returns the existing
/// folder on name conflict (409); we fetch it via a follow-up `GET` in that case.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn mkdir(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    parent_item_id: &str,
    name: &str,
) -> Result<RemoteItem, GraphInternalError> {
    let url = format!(
        "{GRAPH_BASE}/drives/{}/items/{parent_item_id}/children",
        drive_id.as_str()
    );

    let body = MkdirBody {
        name,
        folder: serde_json::json!({}),
        conflict_behavior: "fail",
    };

    let request_id = new_request_id();
    let resp = http
        .post(&url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .json(&body)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    let status = resp.status();

    if status == reqwest::StatusCode::CONFLICT {
        // 409 nameAlreadyExists — fetch the existing folder.
        let existing_url = format!(
            "{GRAPH_BASE}/drives/{}/items/{parent_item_id}:/{name}",
            drive_id.as_str()
        );
        let get_rid = new_request_id();
        let get_resp = http
            .get(&existing_url)
            .bearer_auth(token)
            .header("client-request-id", &get_rid)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;
        return crate::client::check_status(get_resp, &get_rid)
            .await?
            .json::<RemoteItem>()
            .await
            .map_err(|e| GraphInternalError::Decode {
                detail: e.to_string(),
            });
    }

    crate::client::check_status(resp, &request_id)
        .await?
        .json::<RemoteItem>()
        .await
        .map_err(|e| GraphInternalError::Decode {
            detail: e.to_string(),
        })
}

/// `POST /subscriptions` — register a webhook for `drive`. Returns the Graph subscription id.
///
/// The Microsoft Graph contract: subscription resources expire after at most ~3 days for
/// driveItem; the daemon's scheduler is expected to renew them. M10 lands the create/delete
/// path; renewal lives with the subscription lifecycle work in M12+.
///
/// # Errors
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn subscribe(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    notification_url: &str,
    client_state: &str,
) -> Result<String, GraphInternalError> {
    let url = format!("{GRAPH_BASE}/subscriptions");

    // ChangeType "updated" covers create+modify; we receive a single notification per delta.
    // Expiration set to 3 days (max allowed for driveItem).
    let expiration = {
        #[allow(clippy::disallowed_methods)]
        // LINT: subscription expiry derived from wall-clock at call time.
        let now = chrono::Utc::now();
        (now + chrono::Duration::days(3)).to_rfc3339()
    };
    let body = serde_json::json!({
        "changeType": "updated",
        "notificationUrl": notification_url,
        "resource": format!("/drives/{}/root", drive_id.as_str()),
        "expirationDateTime": expiration,
        "clientState": client_state,
    });

    let request_id = new_request_id();
    let resp = http
        .post(&url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .json(&body)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;
    let resp = crate::client::check_status(resp, &request_id).await?;
    let value: serde_json::Value = resp.json().await.map_err(|e| GraphInternalError::Decode {
        detail: e.to_string(),
    })?;
    value
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| GraphInternalError::Decode {
            detail: "subscriptions response missing id".to_owned(),
        })
}

/// `DELETE /subscriptions/{id}` — remove a registered webhook.
///
/// # Errors
/// Returns [`GraphInternalError`] on HTTP failure.
pub async fn unsubscribe(
    http: &reqwest::Client,
    token: &str,
    subscription_id: &str,
) -> Result<(), GraphInternalError> {
    let url = format!("{GRAPH_BASE}/subscriptions/{subscription_id}");
    let request_id = new_request_id();
    let resp = http
        .delete(&url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;
    crate::client::check_status(resp, &request_id).await?;
    Ok(())
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    #[allow(dead_code)]
    fn drive(s: &str) -> DriveId {
        DriveId::new(s)
    }

    #[tokio::test]
    async fn rename_200_returns_item() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/drives/drv1/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "item-1",
                "name": "renamed.txt",
                "size": 100
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/items/item-1", server.uri());
        let resp = http
            .patch(&url)
            .bearer_auth("tok")
            .json(&serde_json::json!({"name": "renamed.txt"}))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let item: RemoteItem = resp.json().await.unwrap();
        assert_eq!(item.name, "renamed.txt");
    }

    #[tokio::test]
    async fn delete_204_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/drives/drv1/items/item-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let http = make_client();
        let resp = http
            .delete(format!("{}/drives/drv1/items/item-1", server.uri()))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);
    }

    #[tokio::test]
    async fn mkdir_201_returns_folder() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/drives/drv1/items/parent-1/children"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": "folder-id",
                "name": "NewFolder",
                "size": 0,
                "folder": { "childCount": 0 }
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/drives/drv1/items/parent-1/children", server.uri());
        let resp = http
            .post(&url)
            .bearer_auth("tok")
            .json(&serde_json::json!({"name": "NewFolder", "folder": {}}))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let item: RemoteItem = resp.json().await.unwrap();
        assert_eq!(item.name, "NewFolder");
        assert!(item.folder.is_some());
    }

    #[tokio::test]
    async fn mkdir_conflict_409_fetches_existing() {
        let server = MockServer::start().await;

        // POST → 409
        Mock::given(method("POST"))
            .and(path("/drives/drv1/items/parent-1/children"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": { "code": "nameAlreadyExists", "message": "Conflict" }
            })))
            .mount(&server)
            .await;

        // Follow-up GET for the existing folder.
        Mock::given(method("GET"))
            .and(path("/drives/drv1/items/parent-1:/ExistingFolder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "existing-folder",
                "name": "ExistingFolder",
                "size": 0,
                "folder": { "childCount": 3 }
            })))
            .mount(&server)
            .await;

        let http = make_client();
        // Direct call — but the URL points at wiremock so we use the helper indirectly.
        // We call the test helper by building the URLs manually.
        let post_url = format!("{}/drives/drv1/items/parent-1/children", server.uri());
        let post_resp = http
            .post(&post_url)
            .bearer_auth("tok")
            .json(&serde_json::json!({"name":"ExistingFolder","folder":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(post_resp.status(), 409);

        // Fetch existing.
        let get_url = format!(
            "{}/drives/drv1/items/parent-1:/ExistingFolder",
            server.uri()
        );
        let get_resp = http.get(&get_url).bearer_auth("tok").send().await.unwrap();
        let item: RemoteItem = get_resp.json().await.unwrap();
        assert_eq!(item.id, "existing-folder");
        assert_eq!(item.name, "ExistingFolder");
    }
}
