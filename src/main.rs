use std::fmt::format;
use std::mem::forget;
use std::net::{TcpListener, TcpStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value::String;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Response};
use tokio_tungstenite::tungstenite::http::{Request, StatusCode};
use tokio_tungstenite::tungstenite::Message;

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

fn default_pos()-> u16 {
    9100
}

#[derive(Debug, Deserialize)]
struct PrintRequest {
    #[serde(default)]
    id:String,
    action:String,
    printer:PrinterTarget,
    #[serde(default = "default_encoding")]
    encoding:String,
    #[serde(default)]
    data:String,
}

fn default_encoding()-> String {
    "plain".to_string()
}

#[derive(Debug, Deserialize)]
struct Reply {
    id:String,
    ok:bool,
    #[serde(skip_serializing_if = "Option:is_none")]
    error:Option<String>,
}

impl Reply {
    fn ok(id:String) -> Self {
        Reply { id, ok:true, error:None }
    }
    fn error(id:String, msg: impl Into<String>) -> Self {
        Reply { id, ok:false, error:Some(msg.into()) }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(BIND_ADDR).await?;
    println!("printbridge listening on ws://{BIND_ADDR}");
    if ALLOWED_ORIGINS.is_empty() {
        println!("WARNING: no origin allowlist set — accepting any web origin.");
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
                ws.send(Message::Text(json)).await?;
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
        Err(e)=> return Reply::error(String::new(),format!("Invalid json: {e}"))
    };
    if req.action != "print" {
        return Reply::error(req.id, format!("Unknow action : {}",req.action));
    }

    let bytes = match  decode_data(&req.encoding,&req.data){
        Ok(b) => b,
        Err(e) => return Reply::error(req.id,e),
    }
}

