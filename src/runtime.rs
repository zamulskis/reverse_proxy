use std::{
    cell::RefCell,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    rc::Rc,
    str,
    sync::{mpsc, Arc, Mutex},
    thread::sleep,
    time::Duration,
};

use crate::http::{
    get_content_length, proxy_rewrite_request, Headers, HttpVersion, ProxyRules, Request, Response,
};

const MAX_HEADER_SIZE: usize = 8096;

pub enum HttpProxyErr {
    UnexpectedHeaderFormat,
    UnsupportedVersion,
    UnsupportedMethod,
    ProxyRuleNotFound,
    UnnableToConnect,
    MaxHeaderSizeExceeded,
    CorruptedClientConnection,
}

pub trait HttpReceivable: Clone {
    // fn new(headers: &Headers, body: Vec<u8>) -> Self;
    fn parse_headers(buf: &[u8]) -> Result<(Self, usize), HttpProxyErr>;
    fn get_body(&mut self) -> &mut Vec<u8>;
    fn get_headers(&self) -> &Headers;
    fn get_http_version(&self) -> &HttpVersion;
}

enum HttpMessage<T: HttpReceivable> {
    Unparsed(Vec<u8>),
    WebSocket(Vec<u8>),
    HeaderParsed(T),
    Ready(T),
}

#[derive(PartialEq)]
enum StreamHandleMode {
    Http,
    UpgradeWebsocket,
    WebSocket,
}

struct HttpMessageHandle<T: HttpReceivable> {
    message: HttpMessage<T>,
}

struct ResponseStreamHandle {
    endpoint: TcpStream,
    client: std::rc::Weak<RefCell<TcpStream>>,
    messages: Vec<HttpMessageHandle<Response>>,
    host: Box<str>,
    mode: StreamHandleMode,
}

struct RequestStreamHandle {
    client: Rc<RefCell<TcpStream>>,
    messages: Vec<HttpMessageHandle<Request>>,
    mode: StreamHandleMode,
}

impl<T: HttpReceivable> HttpMessageHandle<T> {
    pub fn new(message: HttpMessage<T>) -> Self {
        HttpMessageHandle { message }
    }
}

impl<T: HttpReceivable> Default for HttpMessageHandle<T> {
    fn default() -> Self {
        let message: HttpMessage<T> = HttpMessage::Unparsed(Vec::new());
        HttpMessageHandle { message }
    }
}

pub fn acceptor(port: u32, stream_tx: mpsc::Sender<TcpStream>) {
    let listener =
        TcpListener::bind(format!("127.0.0.1:{port}")).expect("Could not bind to address");

    loop {
        for stream in listener.incoming() {
            if let Ok(client) = stream {
                match client.set_nonblocking(true) {
                    Ok(_) => {
                        stream_tx
                            .send(client)
                            .expect("Failed to send client trough channel");
                    }
                    Err(e) => println!("Failed to set socket to set_nonblocking {e}"),
                }
            }
        }
    }
}
fn manage_connections(
    requests: &mut Vec<RequestStreamHandle>,
    responses: &mut Vec<ResponseStreamHandle>,
) {
    // Upgrade to websocket protocol
    for request_handle in requests {
        if request_handle.mode == StreamHandleMode::UpgradeWebsocket {
            if let Some(position) = responses.iter().position(|x| {
                std::rc::Weak::ptr_eq(&Rc::downgrade(&request_handle.client), &x.client)
            }) {
                let response_handle = responses.get_mut(position).unwrap();
                if response_handle.mode == StreamHandleMode::UpgradeWebsocket {
                    request_handle.mode = StreamHandleMode::WebSocket;
                    response_handle.mode = StreamHandleMode::WebSocket;
                }
            }
        }
    }
}

pub fn request_handler(
    proxy_rules: Arc<Mutex<(usize, ProxyRules)>>,
    stream_rx: Arc<Mutex<mpsc::Receiver<TcpStream>>>,
) {
    let mut requests: Vec<RequestStreamHandle> = Vec::new();
    let mut responses: Vec<ResponseStreamHandle> = Vec::new();
    // #TODO Do inside a loop every 5 mintes
    let rules = match proxy_rules.lock() {
        Ok(rules) => rules.1.clone(),
        Err(_) => panic!("Failed to unlock proxy_rules"),
    };
    loop {
        register_new_clients(&mut requests, &stream_rx);
        process_requests(&mut requests);
        send_out_requests(&mut requests, &mut responses, &rules);
        process_responses(&mut responses);
        send_out_responses(&mut responses);
        manage_connections(&mut requests, &mut responses);
        sleep(Duration::new(0, 1000));
    }
}

fn register_new_clients(
    client_streams: &mut Vec<RequestStreamHandle>,
    stream_rx: &Arc<Mutex<mpsc::Receiver<TcpStream>>>,
) {
    match stream_rx.try_lock() {
        Ok(stream_rx) => {
            while match stream_rx.try_recv() {
                Ok(stream) => {
                    client_streams.push(RequestStreamHandle {
                        client: Rc::new(RefCell::new(stream)),
                        messages: Vec::new(),
                        mode: StreamHandleMode::Http,
                    });
                    true
                }
                Err(mpsc::TryRecvError::Empty) => false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("receive_client_streams failed: client_streams channel disconnected")
                }
            } {}
        }
        Err(std::sync::TryLockError::WouldBlock) => (),
        Err(_) => panic!("Failed to lock stream_rx"),
    }
}
fn check_for_websocket_upgrade<T: HttpReceivable>(request: &T) -> bool {
    match request.get_headers().get("Upgrade") {
        Some(upgrade) => &upgrade[..] == "websocket",
        None => false,
    }
}

fn process_requests(request_list: &mut Vec<RequestStreamHandle>) {
    let mut index = 0;
    while index < request_list.len() {
        let request_stream_handle = request_list.get_mut(index).unwrap();
        if request_stream_handle.messages.is_empty() {
            request_stream_handle
                .messages
                .push(HttpMessageHandle::default());
        }

        let stream = request_stream_handle.client.clone();

        let mut buf = [0; 8096];

        match stream.borrow_mut().read(&mut buf) {
            Ok(0) => {
                request_list.remove(index);
                continue;
            }
            Ok(size) => {
                let mut packet = &buf[..size];
                loop {
                    let request = request_stream_handle.messages.last_mut().unwrap();
                    let result = match request_stream_handle.mode {
                        StreamHandleMode::Http | StreamHandleMode::UpgradeWebsocket => {
                            process_http_receivable(packet, request)
                        }
                        StreamHandleMode::WebSocket => {
                            process_websocket_receivable(packet, request)
                        }
                    };

                    if let HttpMessage::Ready(ref message) = request.message {
                        if check_for_websocket_upgrade(message) {
                            request_stream_handle.mode = StreamHandleMode::UpgradeWebsocket;
                        }
                        request_stream_handle
                            .messages
                            .push(HttpMessageHandle::default());
                    }

                    match result {
                        Ok(size) => {
                            if size == packet.len() {
                                break;
                            }
                            packet = &packet[size..];
                        }
                        Err(_) => {
                            request_list.remove(index);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::WouldBlock {
                    println!("Failed to read from a client {e}");
                    request_list.remove(index);
                    continue;
                }
            }
        }
        index += 1;
    }
}

fn send_out_requests(
    requests: &mut Vec<RequestStreamHandle>,
    responses: &mut Vec<ResponseStreamHandle>,
    proxy_rules: &ProxyRules,
) {
    for stream_handle in requests {
        stream_handle.messages.retain_mut(|request_element| {
            match request_element.message {
                HttpMessage::Ready(ref mut request) => {
                    // TODO handle this..
                    proxy_rewrite_request(request, proxy_rules);
                    let host = request
                        .get_headers()
                        .get("Host")
                        .expect("No Host in request");

                    if request.get_http_version() == &HttpVersion::HTTPv1_1 {
                        // Try find existing TcpStream to the server from the same client stream.
                        if let Some(position) = responses.iter().position(|x| {
                            std::rc::Weak::ptr_eq(&Rc::downgrade(&stream_handle.client), &x.client)
                                && &x.host == host
                        }) {
                            let _ = send_http_request(
                                &request,
                                &mut responses.get_mut(position).unwrap().endpoint,
                            );
                            return false;
                        }
                    }
                    if let Ok(stream) = send_http_request_new_stream(&request) {
                        responses.push(ResponseStreamHandle {
                            endpoint: stream,
                            client: Rc::downgrade(&stream_handle.client),
                            messages: Vec::new(),
                            host: request.get_headers().get("Host").unwrap().clone(),
                            mode: StreamHandleMode::Http,
                        });
                    }
                    return false;
                }
                HttpMessage::WebSocket(ref buffer) => {
                    match responses.iter().position(|x| {
                        std::rc::Weak::ptr_eq(&Rc::downgrade(&stream_handle.client), &x.client)
                    }) {
                        Some(position) => {
                            let _ = send_ws_request(
                                &buffer,
                                &mut responses.get_mut(position).unwrap().endpoint,
                            );
                            return false;
                        }
                        None => {
                            return true;
                        }
                    };
                }
                _ => return true, // Keep all requests that are not ready to be sent out yet.
            }
        })
    }
}

fn process_responses(response_list: &mut Vec<ResponseStreamHandle>) {
    let mut buf = [0; 8096];
    let mut index = 0;
    while index < response_list.len() {
        let response_stream_handle = response_list.get_mut(index).unwrap();
        if response_stream_handle.messages.is_empty() {
            response_stream_handle
                .messages
                .push(HttpMessageHandle::default());
        }
        match response_stream_handle.endpoint.read(&mut buf) {
            Ok(0) => {
                if let Some(client) = std::rc::Weak::upgrade(&response_stream_handle.client) {
                    _ = client.borrow_mut().shutdown(std::net::Shutdown::Both);
                }
                response_list.remove(index);
                // we shut down client if response had an error

                continue;
            }
            Ok(size) => {
                let mut packet = &buf[..size];
                loop {
                    let response = response_stream_handle.messages.last_mut().unwrap();
                    let result = match response_stream_handle.mode {
                        StreamHandleMode::Http | StreamHandleMode::UpgradeWebsocket => {
                            process_http_receivable(packet, response)
                        }
                        StreamHandleMode::WebSocket => {
                            process_websocket_receivable(packet, response)
                        }
                    };

                    if let HttpMessage::Ready(ref message) = response.message {
                        if check_for_websocket_upgrade(message) {
                            response_stream_handle.mode = StreamHandleMode::UpgradeWebsocket;
                        }

                        response_stream_handle
                            .messages
                            .push(HttpMessageHandle::default());
                    }
                    match result {
                        Ok(size) => {
                            if size == packet.len() {
                                break;
                            }
                            packet = &packet[size..];
                        }
                        Err(_) => {
                            if let Some(client) =
                                std::rc::Weak::upgrade(&response_stream_handle.client)
                            {
                                _ = client.borrow_mut().shutdown(std::net::Shutdown::Both);
                            }

                            response_list.remove(index);
                            // we shut down client if response had an error
                            break;
                        }
                    }
                }
            }

            Err(e) => {
                if e.kind() != io::ErrorKind::WouldBlock {
                    // we shut down client if response had an error
                    if let Some(client) = std::rc::Weak::upgrade(&response_stream_handle.client) {
                        _ = client.borrow_mut().shutdown(std::net::Shutdown::Both);
                    }

                    response_list.remove(index);
                }
            }
        }
        index += 1;
    }
}

fn send_out_responses(responses: &mut Vec<ResponseStreamHandle>) {
    let mut index = 0;
    while index < responses.len() {
        let response_stream_handle = responses.get_mut(index).unwrap();
        match response_stream_handle.client.upgrade() {
            Some(client_stream) => response_stream_handle.messages.retain(|response_handler| {
                match response_handler.message {
                    HttpMessage::Ready(ref response) => {
                        let buf: Box<[u8]> = Box::from(response);
                        // TODO handle this..
                        _ = client_stream.borrow_mut().write(&buf[..]);
                        return false;
                    }
                    HttpMessage::WebSocket(ref buffer) => {
                        _ = client_stream.borrow_mut().write(&buffer);
                        return false;
                    }
                    _ => return true,
                }
            }),
            None => response_stream_handle.messages.clear(),
        }
        index += 1;
    }
}
fn send_ws_request(packet: &[u8], stream: &mut TcpStream) -> Result<(), HttpProxyErr> {
    match stream.write(&packet) {
        Ok(_) => {
            return Ok(());
        }
        Err(_) => return Err(HttpProxyErr::UnnableToConnect),
    }
}

fn send_http_request(request: &Request, stream: &mut TcpStream) -> Result<(), HttpProxyErr> {
    let packet: Box<[u8]> = request.into();
    match stream.write(&packet) {
        Ok(_) => {
            return Ok(());
        }
        Err(_) => return Err(HttpProxyErr::UnnableToConnect),
    }
}
fn send_http_request_new_stream(request: &Request) -> Result<TcpStream, HttpProxyErr> {
    let host = request
        .get_headers()
        .get("Host")
        .expect("No Host in request");

    match TcpStream::connect(&host[..]) {
        Ok(mut stream) => {
            stream
                .set_nonblocking(true)
                .expect("Failed to set stream to set_nonblocking");
            let packet: Box<[u8]> = request.into();
            match stream.write(&packet) {
                Ok(_) => {
                    return Ok(stream);
                }
                Err(_) => return Err(HttpProxyErr::UnnableToConnect),
            }
        }
        Err(_) => return Err(HttpProxyErr::UnnableToConnect),
    }
}

fn process_http_receivable<T: HttpReceivable>(
    packet: &[u8],
    receivable_handle: &mut HttpMessageHandle<T>,
) -> Result<usize, HttpProxyErr> {
    match receivable_handle.message {
        HttpMessage::Unparsed(ref mut buffer) => {
            let initial_buffer_len = buffer.len();
            buffer.extend(packet.iter());
            match T::parse_headers(&buffer) {
                Ok((mut receivable, header_size)) => {
                    let content_length = get_content_length(&receivable.get_headers());
                    // Part of incomming packet that got included into header
                    let incomming_packet_header_size = header_size - initial_buffer_len;

                    if content_length == 0 {
                        receivable_handle.message = HttpMessage::Ready(receivable);
                        return Ok(incomming_packet_header_size);
                    }

                    let body = &buffer[header_size..];
                    if content_length > body.len() {
                        receivable.get_body().extend(body);
                        receivable_handle.message = HttpMessage::HeaderParsed(receivable);
                        return Ok(packet.len());
                    }

                    let body = &body[..content_length];
                    receivable.get_body().extend(body);
                    receivable_handle.message = HttpMessage::Ready(receivable);
                    return Ok(incomming_packet_header_size + content_length);
                }
                Err(_) => {
                    if buffer.len() >= MAX_HEADER_SIZE {
                        return Err(HttpProxyErr::CorruptedClientConnection);
                    }
                    return Ok(packet.len());
                }
            }
        }
        HttpMessage::HeaderParsed(ref mut receivable) => {
            let content_length = get_content_length(&receivable.get_headers());
            assert!(content_length != 0);
            let content_missing_len = content_length - receivable.get_body().len();
            if content_missing_len > packet.len() {
                receivable.get_body().extend(packet);
                return Ok(packet.len());
            }

            receivable.get_body().extend(&packet[..content_missing_len]);
            receivable_handle.message = HttpMessage::Ready(receivable.clone());
            return Ok(content_missing_len);
        }
        HttpMessage::Ready(_) => panic!("Trying to process ready request"),
        HttpMessage::WebSocket(_) => {
            panic!("Websocket should not be handled by process_http_receivable")
        }
    }
}

fn process_websocket_receivable<T: HttpReceivable>(
    packet: &[u8],
    receivable_handle: &mut HttpMessageHandle<T>,
) -> Result<usize, HttpProxyErr> {
    match receivable_handle.message {
        HttpMessage::Unparsed(ref mut buffer) => {
            buffer.extend(packet);
            receivable_handle.message = HttpMessage::WebSocket(buffer.clone());
            Ok(packet.len())
        }
        _ => panic!("Websocket packets should be ready the moment they are created"),
    }
}
