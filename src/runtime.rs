use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::sleep,
    time::{Duration, Instant},
};

use rustls::ClientConnection;

use crate::{
    config::{BackendConfig, ProxyRuleConfig},
    http::{get_content_length, proxy_rewrite_request, Headers, Request, Response},
};

const MAX_HEADER_SIZE: usize = 16384;
const MAX_TCP_PACKET_SIZE: usize = 655350;

#[derive(Debug)]
pub enum TlsError {
    UnnableToGetBackendServerName,
    UnnableToCreateTlsConnection,
}

#[derive(Debug)]
pub enum HttpProxyErr {
    UnexpectedHeaderFormat,
    UnsupportedVersion,
    UnsupportedMethod,
    ProxyRuleNotFound,
    UnnableToConnect,
    MaxHeaderSizeExceeded,
    CorruptedClientConnection,
    TlsError(TlsError),
}

pub trait HttpTransmittable: Clone {
    // fn new(headers: &Headers, body: Vec<u8>) -> Self;
    fn parse_headers(buf: &[u8]) -> Result<(Self, usize), HttpProxyErr>;
    fn get_body(&mut self) -> &mut Vec<u8>;
    fn get_headers(&self) -> &Headers;
    fn set_headers(&mut self, key: &str, value: &str);
    fn to_vec_u8(&self) -> Vec<u8>;
}

enum HttpMessage<T: HttpTransmittable> {
    Unparsed(Vec<u8>),
    HeaderParsed(T),
    Sending(Vec<u8>),
}

#[derive(PartialEq)]
enum StreamState {
    Http,
    UpgradeWebsocket,
    WebSocket,
}

struct HttpMessageHandle<T: HttpTransmittable> {
    message: HttpMessage<T>,
}

enum StreamType {
    Raw(TcpStream),
    Tls(rustls::StreamOwned<ClientConnection, TcpStream>),
}

struct RequestStreamHandle {
    requests: Vec<HttpMessageHandle<Request>>,
    responses: Vec<HttpMessageHandle<Response>>,
    client_state: StreamState,
    client_stream: StreamType,
    backend_stream: Option<StreamType>,
    backend: Option<BackendConfig>,
}

impl<T: HttpTransmittable> Default for HttpMessageHandle<T> {
    fn default() -> Self {
        let message: HttpMessage<T> = HttpMessage::Unparsed(Vec::new());
        HttpMessageHandle { message }
    }
}

pub fn acceptor(addr: &SocketAddr, stream_tx: &mut Vec<(Arc<AtomicU64>, mpsc::Sender<TcpStream>)>) {
    let listener = TcpListener::bind(addr).expect("Failed to bind on {addr}");

    loop {
        for stream in listener.incoming() {
            if let Ok(client) = stream {
                match client.set_nonblocking(true) {
                    Ok(_) => {
                        stream_tx
                            .iter_mut()
                            .min_by_key(|x| x.0.load(Ordering::Relaxed))
                            .unwrap()
                            .1
                            .send(client)
                            .expect("Failed to send client trough channel");
                    }
                    Err(e) => println!("Failed to set socket to set_nonblocking {e}"),
                }
            }
        }
    }
}

pub fn worker_loop(
    proxy_rules: Vec<ProxyRuleConfig>,
    stream_rx: mpsc::Receiver<TcpStream>,
    client_count: Arc<AtomicU64>,
) {
    let mut request_streams: Vec<RequestStreamHandle> = Vec::new();

    loop {
        register_new_clients(&mut request_streams, &stream_rx);
        client_count.store(request_streams.len() as u64, Ordering::Relaxed);

        // Process requests
        request_streams.retain_mut(|request_stream| {
            let (retain, websocket_upgrade, backend) = process_http_messages(
                &mut request_stream.requests,
                &mut request_stream.client_stream,
                &mut request_stream.client_state,
                |mut packet| Ok(proxy_rewrite_request(&mut packet, &proxy_rules)?),
            );

            if backend.is_some() {
                request_stream.backend = Some(backend.unwrap().clone());
            }

            if websocket_upgrade {
                request_stream.client_state = StreamState::UpgradeWebsocket;
            }
            retain
        });

        send_out_requests(&mut request_streams);

        // Process responses
        request_streams.retain_mut(|request_stream| {
            let mut stream = match request_stream.backend_stream {
                Some(ref mut stream) => stream,
                None => return true,
            };

            let (retain, websocket_upgrade, _) = process_http_messages(
                &mut request_stream.responses,
                &mut stream,
                &mut request_stream.client_state,
                |&mut _| Ok(()),
            );

            if websocket_upgrade && request_stream.client_state == StreamState::UpgradeWebsocket {
                request_stream.client_state = StreamState::WebSocket;
            }
            retain
        });
        send_out_responses(&mut request_streams);
    }
}

fn register_new_clients(
    request_streams: &mut Vec<RequestStreamHandle>,
    stream_rx: &mpsc::Receiver<TcpStream>,
) {
    while match stream_rx.try_recv() {
        Ok(stream) => {
            request_streams.push(RequestStreamHandle {
                requests: Vec::new(),
                responses: Vec::new(),
                client_state: StreamState::Http,
                client_stream: StreamType::Raw(stream),
                backend: None,
                backend_stream: None,
            });
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => {
            panic!("receive_client_streams failed: client_streams channel disconnected")
        }
    } {}
}
fn check_for_websocket_upgrade<T: HttpTransmittable>(request: &T) -> bool {
    match request.get_headers().get("Upgrade") {
        Some(upgrade) => &upgrade[..] == "websocket",
        None => false,
    }
}

fn new_backend_stream(backend: &BackendConfig) -> Result<StreamType, HttpProxyErr> {
    match TcpStream::connect(format!("{}:{}", &backend.host, backend.port)) {
        Ok(stream) => {
            stream
                .set_nonblocking(true)
                .expect("Failed to set stream to set_nonblocking");

            if backend.https == false {
                Ok(StreamType::Raw(stream))
            } else {
                setup_tls_backend_socket(stream, backend.clone())
            }
        }
        Err(_) => return Err(HttpProxyErr::UnnableToConnect),
    }
}

fn send_out_requests(request_stream_list: &mut Vec<RequestStreamHandle>) {
    let mut request_stream_list_index = 0;
    'stream_handle_loop: while request_stream_list_index < request_stream_list.len() {
        let stream = &mut request_stream_list[request_stream_list_index];
        let mut stream_index = 0;
        while stream_index < stream.requests.len() {
            let request = &mut stream.requests[stream_index];

            if let HttpMessage::Sending(ref mut buf) = request.message {
                if stream.backend_stream.is_none() {
                    stream.backend_stream =
                        match new_backend_stream(stream.backend.as_ref().unwrap()) {
                            Ok(stream) => Some(stream),
                            Err(_) => {
                                println!("Failed to connect to backend");
                                request_stream_list.remove(request_stream_list_index);
                                continue 'stream_handle_loop;
                            }
                        }
                }
                let backend_stream = stream.backend_stream.as_mut();
                match write_stream_mode(&buf, &mut backend_stream.unwrap()) {
                    Ok(size) => {
                        buf.drain(0..size);
                    }
                    Err(e) => {
                        if e.kind() != io::ErrorKind::WouldBlock {
                            println!("Failed to send request to backend");
                            request_stream_list.remove(request_stream_list_index);
                            continue 'stream_handle_loop;
                        }
                    }
                };
                if buf.len() == 0 {
                    stream.requests.remove(stream_index);
                    continue;
                };
            }
            stream_index += 1;
        }
        request_stream_list_index += 1;
    }
}

fn read_stream_mode(buf: &mut [u8], stream: &mut StreamType) -> Result<usize, std::io::Error> {
    match stream {
        StreamType::Raw(ref mut tcp_stream) => tcp_stream.read(buf),
        StreamType::Tls(ref mut tls_stream) => tls_stream.read(buf),
    }
}
fn write_stream_mode(buf: &[u8], stream: &mut StreamType) -> Result<usize, std::io::Error> {
    match stream {
        StreamType::Raw(ref mut tcp_stream) => tcp_stream.write(buf),
        StreamType::Tls(ref mut tls_stream) => tls_stream.write(buf),
    }
}

fn process_http_messages<T: HttpTransmittable, A>(
    request_list: &mut Vec<HttpMessageHandle<T>>,
    stream: &mut StreamType,
    stream_state: &StreamState,
    packet_rewrite: impl Fn(&mut T) -> Result<A, HttpProxyErr>,
) -> (bool, bool, Option<A>) {
    let mut buf = [0; MAX_TCP_PACKET_SIZE];
    if request_list.is_empty() {
        request_list.push(HttpMessageHandle::default());
    }
    let mut rewrite_return = None;
    match read_stream_mode(&mut buf, stream) {
        Ok(0) => {
            return (false, false, None);
        }
        Ok(size) => {
            let mut websocket_upgrade = false;
            let mut packet = &buf[..size];
            loop {
                let response = request_list.last_mut().unwrap();
                let result = process_http_transittable(packet, response, stream_state);

                match result {
                    Ok((size, ready_to_send)) => {
                        if ready_to_send {
                            match response.message {
                                HttpMessage::HeaderParsed(ref mut response_parsed) => {
                                    websocket_upgrade =
                                        check_for_websocket_upgrade(response_parsed);
                                    rewrite_return = match packet_rewrite(response_parsed) {
                                        Ok(ret) => Some(ret),
                                        Err(_) => return (false, false, None),
                                    };
                                    response.message =
                                        HttpMessage::Sending(response_parsed.to_vec_u8());
                                }
                                HttpMessage::Unparsed(_) => {
                                    panic!("Unparsed response should never be ready to send")
                                }
                                _ => (),
                            }
                            packet = &packet[size..];
                            request_list.push(HttpMessageHandle::default());
                            if packet.len() == 0 {
                                return (true, websocket_upgrade, rewrite_return);
                            }
                            continue;
                        }
                        assert!(packet.len() == size);
                        return (true, websocket_upgrade, rewrite_return);
                    }
                    Err(_) => {
                        println!("Failed to parse response");
                        return (false, false, None);
                    }
                }
            }
        }
        Err(e) => {
            if e.kind() != io::ErrorKind::WouldBlock {
                println!("Failed to read from a client {e}");
                return (false, false, None);
            }
            return (true, false, rewrite_return);
        }
    }
}

fn send_out_responses(responses: &mut Vec<RequestStreamHandle>) {
    let mut index = 0;
    while index < responses.len() {
        let response_stream_handle = &mut responses[index];
        response_stream_handle
            .responses
            .retain_mut(|response_handler| match response_handler.message {
                HttpMessage::Sending(ref mut buf) => {
                    match write_stream_mode(&buf, &mut response_stream_handle.client_stream) {
                        Ok(size) => {
                            buf.drain(0..size);
                            if buf.len() == 0 {
                                return false;
                            }
                            return true;
                        }
                        Err(e) => {
                            if e.kind() != io::ErrorKind::WouldBlock {
                                println!("Failed to write to backend");
                                return false;
                            }
                            return true;
                        }
                    }
                }
                _ => return true,
            });
        index += 1;
    }
}

fn setup_tls_backend_socket(
    stream: TcpStream,
    backend: BackendConfig,
) -> Result<StreamType, HttpProxyErr> {
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let server_name: rustls::pki_types::ServerName<'static> =
        match backend.host.to_string().try_into() {
            Ok(server_name) => server_name,
            Err(_) => {
                return Err(HttpProxyErr::TlsError(
                    TlsError::UnnableToGetBackendServerName,
                ))
            }
        };
    let conn = match rustls::ClientConnection::new(Arc::new(config), server_name) {
        Ok(conn) => conn,
        Err(_) => {
            return Err(HttpProxyErr::TlsError(
                TlsError::UnnableToCreateTlsConnection,
            ))
        }
    };
    let tls_stream = rustls::StreamOwned::new(conn, stream);

    return Ok(StreamType::Tls(tls_stream));
}

fn process_http_transittable<T: HttpTransmittable>(
    packet: &[u8],
    transittable_handle: &mut HttpMessageHandle<T>,
    state: &StreamState,
) -> Result<(usize, bool), HttpProxyErr> {
    match transittable_handle.message {
        HttpMessage::Unparsed(ref mut buffer) => {
            if let StreamState::WebSocket = state {
                buffer.extend(packet);
                transittable_handle.message = HttpMessage::Sending(buffer.clone());
                return Ok((packet.len(), true));
            }
            let initial_buffer_len = buffer.len();
            buffer.extend(packet.iter());

            match T::parse_headers(&buffer) {
                Ok((mut transittable, header_size)) => {
                    let content_length = get_content_length(&transittable.get_headers());
                    // Part of incomming packet that got included into header
                    let incomming_packet_header_size = header_size - initial_buffer_len;

                    if content_length == 0 {
                        transittable_handle.message = HttpMessage::HeaderParsed(transittable);
                        return Ok((incomming_packet_header_size, true));
                    }

                    let body = &buffer[header_size..];
                    transittable.get_body().extend(body);
                    if content_length > body.len() {
                        transittable_handle.message = HttpMessage::HeaderParsed(transittable);
                        return Ok((packet.len(), false));
                    }

                    transittable_handle.message = HttpMessage::HeaderParsed(transittable);
                    return Ok((incomming_packet_header_size + content_length, true));
                }
                Err(_) => {
                    if buffer.len() >= MAX_HEADER_SIZE {
                        println!("Faield to parse headers!");
                        return Err(HttpProxyErr::CorruptedClientConnection);
                    }
                    return Ok((packet.len(), false));
                }
            }
        }
        HttpMessage::HeaderParsed(ref mut transittable) => {
            let content_length = get_content_length(&transittable.get_headers());

            assert!(content_length != 0);
            let content_missing_len = content_length - transittable.get_body().len();
            if content_missing_len > packet.len() {
                transittable.get_body().extend(packet);
                return Ok((packet.len(), false));
            }

            transittable
                .get_body()
                .extend(&packet[..content_missing_len]);
            return Ok((content_missing_len, true));
        }
        HttpMessage::Sending(_) => panic!("Trying to process request"),
    }
}
