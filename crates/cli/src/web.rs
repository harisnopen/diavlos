//! Browser UI on localhost. One HTML file plus a WebSocket. Prints a
//! one-time login link; the link's token is swapped for a session cookie
//! on first use and then dies.

use std::sync::Arc;

use axum::extract::ws::{Message as WsMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use diavlos_client::proto::{DraftWire, Request};
use diavlos_client::{Client, Paths};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const PAGE: &str = include_str!("web.html");

struct App {
    client: Client,
    identity: String,
    /// The one-time login token, until it is used.
    login_token: Mutex<Option<String>>,
    session: String,
}

type Shared = Arc<App>;

fn random_token() -> String {
    let bytes: [u8; 24] = rand::random();
    data_encoding::BASE64URL_NOPAD.encode(&bytes)
}

pub async fn serve(paths: Paths, identity: String, port: u16) -> anyhow::Result<()> {
    let app = Arc::new(App {
        client: Client::new(paths),
        identity,
        login_token: Mutex::new(Some(random_token())),
        session: random_token(),
    });
    let router = Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/rooms/{room}/who", get(api_who))
        .route("/api/rooms/{room}/messages", get(api_messages))
        .route("/api/rooms/{room}/send", post(api_send))
        .route("/api/rooms/{room}/approve", post(api_approve))
        .route("/api/rooms/{room}/deny", post(api_deny))
        .route("/ws", get(ws))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let token = app.login_token.lock().await.clone().unwrap_or_default();
    println!("Open this once (it logs you in, then it dies):\n");
    println!("    http://{addr}/?token={token}\n");
    axum::serve(listener, router).await?;
    Ok(())
}

fn has_session(app: &App, headers: &HeaderMap) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .map(|c| {
            c.split(';')
                .any(|kv| kv.trim() == format!("diavlos_session={}", app.session))
        })
        .unwrap_or(false)
}

async fn index(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<Value>) -> Response {
    if let Some(t) = q.get("token").and_then(|t| t.as_str()) {
        let mut slot = app.login_token.lock().await;
        if slot.as_deref() == Some(t) {
            *slot = None;
            let cookie = format!(
                "diavlos_session={}; HttpOnly; SameSite=Strict; Path=/",
                app.session
            );
            return ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response();
        }
        return (
            StatusCode::FORBIDDEN,
            "that login link was already used or is wrong",
        )
            .into_response();
    }
    if !has_session(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            "not logged in. Run `diavlos web` and open the link it prints.",
        )
            .into_response();
    }
    Html(PAGE).into_response()
}

/// `Some(response)` when the caller is not logged in.
fn guard(app: &App, headers: &HeaderMap) -> Option<Response> {
    if has_session(app, headers) {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "not logged in").into_response())
    }
}

fn err_response(e: diavlos_core::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"code": e.code(), "error": e.to_string()})),
    )
        .into_response()
}

async fn api_status(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    match app.client.call(&Request::Status).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn api_who(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(room): AxPath<String>,
) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    match app.client.call(&Request::Who { room }).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn api_messages(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(room): AxPath<String>,
    Query(q): Query<Value>,
) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    let since = q
        .get("since")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse().ok());
    match app
        .client
        .call(&Request::Read {
            room,
            identity: app.identity.clone(),
            since: Some(since.unwrap_or(1)),
            limit: 500,
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn api_send(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(room): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    let draft = DraftWire {
        text: body["text"].as_str().unwrap_or("").to_string(),
        kind: body["type"].as_str().and_then(|k| k.parse().ok()),
        to: body["to"].as_str().map(String::from),
        reply_to: body["reply_to"].as_str().map(String::from),
        ..Default::default()
    };
    match app
        .client
        .call(&Request::Send {
            room,
            identity: app.identity.clone(),
            draft,
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn api_approve(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(room): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    match app
        .client
        .call(&Request::Approve {
            room,
            identity: app.identity.clone(),
            msg_id: body["msg_id"].as_str().unwrap_or("").to_string(),
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

async fn api_deny(
    State(app): State<Shared>,
    headers: HeaderMap,
    AxPath(room): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    match app
        .client
        .call(&Request::Deny {
            room,
            identity: app.identity.clone(),
            msg_id: body["msg_id"].as_str().unwrap_or("").to_string(),
            reason: body["reason"].as_str().unwrap_or("no").to_string(),
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

/// Live feed: every helper event, as JSON lines over the socket.
async fn ws(State(app): State<Shared>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if let Some(r) = guard(&app, &headers) {
        return r;
    }
    upgrade.on_upgrade(move |socket| feed(app, socket))
}

async fn feed(app: Shared, mut socket: WebSocket) {
    let Ok(mut stream) = app.client.stream(&Request::Events { follow: true }).await else {
        return;
    };
    loop {
        tokio::select! {
            line = stream.next() => match line {
                Ok(Some(v)) => {
                    if socket.send(WsMsg::Text(v.to_string().into())).await.is_err() {
                        return;
                    }
                }
                _ => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            }
        }
    }
}
