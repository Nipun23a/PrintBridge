use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

// Loopback only.
const BIND_ADDR: &str = "127.0.0.1:8181";

/// Web origin allowed to connect. Empty = allow any origin
const ALLOWED_ORIGINS: &[&str] = &[
    // "https://your-pos-app.example.com",
];

#[derive(Debug, Deserialize)]
struct PrinterTarget {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_port() -> u16 {
    9100
}

#[derive(Debug, Deserialize)]
struct PrintRequest {
    #[serde(default)]
    id: String,
    action: String,
    printer: PrinterTarget,
    #[serde(default = "default_encoding")]
    encoding: String,
    #[serde(default)]
    data: String,
}

fn default_encoding() -> String {
    "plain".to_string()
}

#[derive(Debug, Serialize)]
struct Reply {
    id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Reply {
    fn ok(id: String) -> Self {
        Reply {
            id,
            ok: true,
            error: None,
        }
    }
    fn error(id: String, msg: impl Into<String>) -> Self {
        Reply {
            id,
            ok: false,
            error: Some(msg.into()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(BIND_ADDR).await?;
    println!("printbridge listening on ws://{BIND_ADDR}");
    if ALLOWED_ORIGINS.is_empty() {
        println!("WARNING: no origin allowlist set - accepting any web origin.");
    }

    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream).await {
                eprintln!("connection from {peer} closed: {e}");
            }
        });
    }
}

async fn handle_conn(stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
    let callback = |req: &Request, response: Response| -> Result<Response, ErrorResponse> {
        if ALLOWED_ORIGINS.is_empty() {
            return Ok(response);
        }
        let origin = req
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ALLOWED_ORIGINS.contains(&origin) {
            Ok(response)
        } else {
            let resp = Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Some("origin not allowed".to_string()))
                .expect("valid response");
            Err(resp)
        }
    };

    let mut ws = accept_hdr_async(stream, callback).await?;

    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(txt) => {
                let reply = process(&txt).await;
                let json = serde_json::to_string(&reply)?;
                ws.send(Message::Text(json.into())).await?;
            }
            Message::Ping(payload) => ws.send(Message::Pong(payload)).await?,
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

async fn process(txt: &str) -> Reply {
    let req: PrintRequest = match serde_json::from_str(txt) {
        Ok(r) => r,
        Err(e) => return Reply::error(String::new(), format!("Invalid json: {e}")),
    };
    if req.action != "print" {
        return Reply::error(req.id, format!("Unknown action: {}", req.action));
    }

    let bytes = match decode_data(&req.encoding, &req.data) {
        Ok(b) => b,
        Err(e) => return Reply::error(req.id, e),
    };

    match send_to_printer(&req.printer.host, req.printer.port, &bytes).await {
        Ok(()) => Reply::ok(req.id),
        Err(e) => Reply::error(req.id, e),
    }
}

fn decode_data(encoding: &str, data: &str) -> Result<Vec<u8>, String> {
    match encoding {
        "plain" => Ok(data.as_bytes().to_vec()),
        "base64" => STANDARD
            .decode(data)
            .map_err(|e| format!("base64 decode error: {e}")),
        other => Err(format!("unknown encoding: {other}")),
    }
}

/// Raw TCP to the printer's RAW/JetDirect port (9100). OS-independent path that
/// works for any network ESC/POS, ZPL, or EPL printer.
async fn send_to_printer(host: &str, port: u16, bytes: &[u8]) -> Result<(), String> {
    let addr = format!("{host}:{port}");

    let mut stream = timeout(Duration::from_secs(5), TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("timed out connecting to {addr}"))?
        .map_err(|e| format!("could not connect to {addr}: {e}"))?;

    timeout(Duration::from_secs(10), stream.write_all(bytes))
        .await
        .map_err(|_| "timed out writing to printer".to_string())?
        .map_err(|e| format!("write failed: {e}"))?;

    stream
        .flush()
        .await
        .map_err(|e| format!("flush failed: {e}"))?;
    Ok(())
}
