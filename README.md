# Printbridge

A free, open-source background worker that lets web apps print to local
hardware — a from-scratch alternative to [QZ Tray](https://qz.io), written in Rust.

A small process runs on the user's machine, hosts a WebSocket server on
`localhost`, and forwards print jobs from a browser straight to a printer. No
JVM, no per-seat license, single native binary per OS.

```
browser (your POS web app)  ──ws://127.0.0.1:8181──▶  printbridge  ──tcp:9100──▶  printer
```

> **Status: Phase 1 MVP.** Raw printing to network printers works end to end.
> Request signing, `wss://`, a system-tray icon, and USB/spooler printing are on
> the roadmap. Do not deploy to real users before reading [Security](#security).

---

## Features

- Localhost WebSocket server (`127.0.0.1:8181`)
- Raw printing to any network printer on the RAW/JetDirect port (`9100`)
- Works with ESC/POS, ZPL, EPL — any printer language (you send the bytes)
- `plain` and `base64` payload encodings (use base64 for raw control bytes)
- Connect/write timeouts so a dead printer never hangs a connection
- One async task per browser connection — many terminals print concurrently
- Optional origin allowlist
- Stateless: no database, no config server, no shared state

---

## Quick start

### Build

```bash
cargo build --release
./target/release/printbridge
# -> printbridge listening on ws://127.0.0.1:8181
```

> If you build with an older Cargo and hit an `edition2024` error, it comes from
> a transitive dependency. The pinned `url`/`idna` lines in `Cargo.toml` work
> around it; on current Rust you can remove those two lines.

### Try it

1. Run the bridge.
2. Open `examples/client.html` in a browser.
3. Enter your printer's IP, type some text, click **Print**.

No printer handy? Simulate one and watch the bytes arrive:

```bash
nc -l 9100        # or: ncat -l 9100
```

---

## Protocol

The browser opens a WebSocket to `ws://127.0.0.1:8181` and sends a JSON text
frame:

```json
{
  "id": "any-string",
  "action": "print",
  "printer": { "host": "192.168.1.50", "port": 9100 },
  "encoding": "base64",
  "data": "G0BIRUxMTwo="
}
```

| Field      | Required | Notes                                                            |
|------------|----------|-----------------------------------------------------------------|
| `id`       | no       | Echoed back so the client can match replies to requests         |
| `action`   | yes      | Currently only `"print"`                                        |
| `printer`  | yes      | `{ "host": "...", "port": 9100 }` — `port` defaults to `9100`   |
| `encoding` | no       | `"plain"` (text) or `"base64"` (raw bytes). Defaults to `plain` |
| `data`     | yes      | The payload, encoded per `encoding`                             |

Use `base64` for ESC/POS, ZPL, or EPL streams — they contain non-printable
control bytes that cannot ride safely inside JSON as plain text.

The server replies:

```json
{ "id": "any-string", "ok": true }
{ "id": "any-string", "ok": false, "error": "could not connect to 192.168.1.50:9100: ..." }
```

### Minimal browser example

```js
const ws = new WebSocket("ws://127.0.0.1:8181");
ws.onopen = () => ws.send(JSON.stringify({
  id: "job-1",
  action: "print",
  printer: { host: "192.168.1.50", port: 9100 },
  encoding: "base64",
  data: btoa("\x1b@Hello printer\n\x1d\x56\x00"), // ESC @ ... GS V 0 (cut)
}));
ws.onmessage = (e) => console.log(JSON.parse(e.data));
```

---

## Architecture

printbridge is a one-way transformation pipeline that narrows a JSON command
over a WebSocket down to raw bytes over TCP, running inside a fresh async task
per browser connection.

```
            Browser  (your POS web app)
               │  ws://127.0.0.1:8181
               ▼
   WebSocket handshake  ── origin allowlist gate  ◀── the one trust boundary
               │
               ▼
     Connection task    ── one tokio task per client
               │
               ▼
      Parse + decode     ── JSON → typed request → raw bytes (serde, base64)
               │
               ▼
    Printer transport    ── fresh outbound TCP, connect/write timeouts
               │  tcp :9100
               ▼
            Printer  (ESC/POS · ZPL · EPL)

   reply (JSON) flows back from the connection task to the browser
```

**Acceptor.** `main` binds a `TcpListener` and loops on `accept()`, handing each
socket to `tokio::spawn`. The acceptor never does real work, so it never
bottlenecks.

**Per-connection task.** Every browser tab gets its own task. Ten terminals are
ten concurrent tasks; none blocks the others. Frames within a single connection
are handled sequentially (each print is awaited before the next frame is read).

**Trust boundary.** `accept_hdr_async` runs the origin-allowlist callback during
the handshake. This is the single place where "is this caller allowed?" is
decided, and where Phase 2 request signing will live.

**Parse + decode (`process`).** The only stage that changes data shape: JSON →
`PrintRequest` (serde) → `Vec<u8>` (`decode_data`).

**Transport (`send_to_printer`).** Opens a *fresh* `TcpStream` per job with
timeouts, writes, flushes, drops. No pooling — simple and robust; a new TCP
handshake per job is an acceptable tradeoff for receipt-style printing.

**Properties.** The bridge is **stateless** (no shared state, no locks) and data
flows **one direction** — only the small JSON reply returns. Nothing is read
back from the printer yet, so paper-out/status queries are not available; that
is a deliberate Phase 3 concern.

---

## Security

Read this before shipping to anyone.

The MVP binds to **loopback only** (`127.0.0.1`) — never bind to `0.0.0.0`, which
would expose the printer to the whole network. There is an optional origin
allowlist (`ALLOWED_ORIGINS` in `src/main.rs`). **With an empty allowlist, any
website the user visits can send print jobs.** That is acceptable for local
development, not for production.

Phase 2 adds **request signing**, the model QZ Tray uses: the integrator holds a
private key and signs each command; the bridge verifies the signature against a
registered public certificate before executing. This is what stops an arbitrary
page from driving the printer. Do not deploy to real users without it.

---

## Roadmap

- **Phase 1 — done.** WebSocket server + raw printing to network printers.
- **Phase 2.** Request signing/verification; `wss://` with a trusted localhost
  certificate; system-tray icon + autostart (the tray owns the main thread's
  event loop while the pipeline runs on a background tokio runtime).
- **Phase 3.** USB and OS-spooler printing (Windows winspool, CUPS on
  Linux/macOS) as a second transport behind the same pipeline; printer
  discovery; bidirectional device status; a small JS client library + docs.
- **Phase 4.** Mobile — a separate architecture, since the desktop
  browser-to-tray model does not port directly to Android/iOS.

---

## Project layout

```
printbridge/
├── Cargo.toml
├── src/
│   └── main.rs          # acceptor, WebSocket handshake, process(), send_to_printer()
└── examples/
    └── client.html      # browser test page
```

---

## Contributing

Issues and pull requests are welcome. Good first areas: USB/spooler transports,
the signing layer, and the tray/autostart integration.

---

## License

Choose a permissive license such as MIT or Apache-2.0 for a free tool.

For context: QZ Tray itself is open source under LGPL-2.1 — what they charge for
is a signed certificate (to suppress the trust prompt) and support, not the
source code. printbridge is an independent implementation.