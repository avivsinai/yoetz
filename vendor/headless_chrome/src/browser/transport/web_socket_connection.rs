use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use log::{debug, info, trace, warn};
use tungstenite::handshake::HandshakeError;
use tungstenite::http::Response;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use url::Url;

use crate::types::{RoutedMessage, parse_raw_message};

type TungsteniteWebsocketConnection = tungstenite::protocol::WebSocket<MaybeTlsStream<TcpStream>>;

const READ_TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_millis(100);
const DEFAULT_HANDSHAKE_TIMEOUT_DURATION: Duration = Duration::from_secs(30);
const YOETZ_DEBUG_CDP_ENV: &str = "YOETZ_DEBUG_CDP";

pub struct WebSocketConnection {
    connection: Arc<Mutex<TungsteniteWebsocketConnection>>,
    thread: std::thread::JoinHandle<()>,
    process_id: Option<u32>,
}

// TODO websocket::sender::Writer is not :Debug...
impl std::fmt::Debug for WebSocketConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(f, "WebSocketConnection {{}}")
    }
}

impl WebSocketConnection {
    pub fn new(
        ws_url: &Url,
        process_id: Option<u32>,
        messages_tx: mpsc::Sender<RoutedMessage>,
    ) -> Result<Self> {
        let (connection, _) = Self::websocket_connection(ws_url)?;

        let connection = Arc::new(Mutex::new(connection));

        let thread = {
            let sender = connection.clone();
            std::thread::spawn(move || {
                trace!("Starting msg dispatching loop");
                Self::dispatch_incoming_messages(sender, messages_tx, process_id);
                trace!("Quit loop msg dispatching loop");
            })
        };

        Ok(Self {
            connection,
            thread,
            process_id,
        })
    }

    pub fn shutdown(&self) {
        trace!(
            "Shutting down WebSocket connection for Chrome {:?}",
            self.process_id
        );
        if let Err(err) = self.connection.lock().unwrap().close(None) {
            debug!(
                "Couldn't shut down WS connection for Chrome {:?}: {}",
                self.process_id, err
            );
        }

        self.connection.lock().unwrap().flush().ok();
        self.thread.thread().unpark();
    }

    fn dispatch_incoming_messages(
        receiver: Arc<Mutex<TungsteniteWebsocketConnection>>,
        messages_tx: mpsc::Sender<RoutedMessage>,
        process_id: Option<u32>,
    ) {
        loop {
            let message = receiver.lock().unwrap().read();

            match message {
                Err(err) => match err {
                    tungstenite::Error::Io(err) => {
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) {
                            std::thread::park_timeout(READ_TIMEOUT_DURATION);
                        } else {
                            debug!("WS IO Error for Chrome #{process_id:?}: {err}");
                            break;
                        }
                    }
                    tungstenite::Error::ConnectionClosed
                    | tungstenite::Error::AlreadyClosed
                    | tungstenite::Error::Protocol(
                        tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    ) => {
                        emit_debug_cdp_transport_message(&format!(
                            "websocket closed for Chrome #{process_id:?}: {err}"
                        ));
                        break;
                    }
                    error => {
                        emit_debug_cdp_transport_message(&format!(
                            "unhandled websocket error for Chrome #{process_id:?}: {error:?}"
                        ));
                        panic!("Unhandled WebSocket error for Chrome #{process_id:?}: {error:?}");
                    }
                },
                Ok(message) => {
                    if let tungstenite::protocol::Message::Text(message_string) = message {
                        if let Ok(message) = parse_raw_message(&message_string) {
                            if messages_tx.send(message).is_err() {
                                break;
                            }
                        } else {
                            trace!(
                                "Incoming message isn't recognised as event or method response: {message_string}",
                            );
                        }
                    } else if let tungstenite::protocol::Message::Close(close_frame) = message {
                        match close_frame {
                            Some(tungstenite::protocol::CloseFrame { code, reason }) => {
                                debug!(
                                    "Received close frame from Chrome #{process_id:?}: {code:?} {reason:?}",
                                );
                                emit_debug_cdp_transport_message(&format!(
                                    "received close frame from Chrome #{process_id:?}: code={code:?} reason={reason:?}"
                                ));
                                match code {
                                    tungstenite::protocol::frame::coding::CloseCode::Normal => {
                                        debug!("Normal close code, shutting down");
                                    }
                                    _ => {
                                        panic!("Abnormal close code {code:?}, shutting down");
                                    }
                                }
                            }
                            None => {
                                debug!("Received close frame from Chrome #{process_id:?}: None");
                            }
                        }
                        break;
                    } else {
                        panic!("Got a weird message: {message:?}");
                    }
                }
            }
        }

        info!("Sending shutdown message to message handling loop");
        if messages_tx.send(RoutedMessage::connection_shutdown()).is_err() {
            warn!("Couldn't send message to transport loop telling it to shut down");
        }
    }

    pub fn websocket_connection(
        ws_url: &Url,
    ) -> Result<(
        tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
        Response<Option<Vec<u8>>>,
    )> {
        let mut client = websocket_connection_with_timeout(
            ws_url,
            handshake_timeout_duration(),
            Some(
                WebSocketConfig::default()
                    .accept_unmasked_frames(true)
                    .max_message_size(None)
                    .max_frame_size(None),
            ),
        )?;

        let stream = client.0.get_mut();

        // this should be handled in tungstenite
        let stream = match stream {
            MaybeTlsStream::Plain(s) => s,
            #[cfg(feature = "native-tls")]
            MaybeTlsStream::NativeTls(s) => s.get_mut(),
            #[cfg(feature = "rustls")]
            MaybeTlsStream::Rustls(s) => &mut s.sock,

            _ => todo!(),
        };
        stream.set_read_timeout(Some(READ_TIMEOUT_DURATION))?;
        stream.set_write_timeout(None)?;

        debug!("Successfully connected to WebSocket: {ws_url}");

        Ok(client)
    }

    pub fn send_message(&self, message_text: &str) -> Result<()> {
        let message = tungstenite::protocol::Message::text(message_text);
        let mut sender = self.connection.lock().unwrap();
        sender.send(message)?;
        self.thread.thread().unpark();
        Ok(())
    }
}

fn emit_debug_cdp_transport_message(message: &str) {
    if std::env::var(YOETZ_DEBUG_CDP_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
    {
        eprintln!("info: cdp-transport {message}");
    }
}

impl Drop for WebSocketConnection {
    fn drop(&mut self) {
        info!("dropping websocket connection");
    }
}

fn handshake_timeout_duration() -> Duration {
    std::env::var("YOETZ_CDP_WS_HANDSHAKE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_DURATION)
}

fn websocket_connection_with_timeout(
    ws_url: &Url,
    timeout: Duration,
    config: Option<WebSocketConfig>,
) -> Result<(
    tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    Response<Option<Vec<u8>>>,
)> {
    let host = ws_url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("websocket URL is missing a host: {ws_url}"))?;
    let port = ws_url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("websocket URL is missing a port: {ws_url}"))?;
    let mut addrs = (host, port).to_socket_addrs()?;
    let mut last_err = None;
    let stream = addrs
        .find_map(|addr| match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => Some(stream),
            Err(err) => {
                last_err = Some(err);
                None
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "timed out connecting to Chrome websocket {ws_url}: {}",
                last_err
                    .map(|err| err.to_string())
                    .unwrap_or_else(|| "no reachable addresses".to_string())
            )
        })?;

    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;

    #[cfg(not(any(feature = "native-tls", feature = "rustls")))]
    let client = tungstenite::client::client_with_config(
        ws_url.as_str(),
        MaybeTlsStream::Plain(stream),
        config,
    );
    #[cfg(any(feature = "native-tls", feature = "rustls"))]
    let client = tungstenite::client_tls_with_config(ws_url.as_str(), stream, config, None);

    client.map_err(|err| match err {
        HandshakeError::Failure(err) => err.into(),
        HandshakeError::Interrupted(_) => {
            anyhow::anyhow!("timed out waiting for Chrome websocket handshake")
        }
    })
}
