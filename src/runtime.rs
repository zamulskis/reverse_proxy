use std::{
    cell::RefCell,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    rc::Rc,
    str::FromStr,
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
    New(Vec<u8>),
    HeaderParsed(T),
    Ready(T),
}

struct HttpMessageHandle<T: HttpReceivable> {
    message: HttpMessage<T>,
}

struct ResponseStreamHandle {
    endpoint: TcpStream,
    client: std::rc::Weak<RefCell<TcpStream>>,
    messages: Vec<HttpMessageHandle<Response>>,
    host: Box<str>,
}

impl<T: HttpReceivable> HttpMessageHandle<T> {
    pub fn new(message: HttpMessage<T>) -> Self {
        HttpMessageHandle { message }
    }
}

impl<T: HttpReceivable> Default for HttpMessageHandle<T> {
    fn default() -> Self {
        let message: HttpMessage<T> = HttpMessage::New(Vec::new());
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

pub fn request_handler(
    proxy_rules: Arc<Mutex<(usize, ProxyRules)>>,
    stream_rx: Arc<Mutex<mpsc::Receiver<TcpStream>>>,
) {
    let mut requests: Vec<(Rc<RefCell<TcpStream>>, Vec<HttpMessageHandle<Request>>)> = Vec::new();
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
        sleep(Duration::new(0, 1000));
    }
}

fn register_new_clients(
    client_streams: &mut Vec<(Rc<RefCell<TcpStream>>, Vec<HttpMessageHandle<Request>>)>,
    stream_rx: &Arc<Mutex<mpsc::Receiver<TcpStream>>>,
) {
    match stream_rx.try_lock() {
        Ok(stream_rx) => {
            while match stream_rx.try_recv() {
                Ok(stream) => {
                    client_streams.push((Rc::new(RefCell::new(stream)), Vec::new()));
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

fn process_requests(
    request_list: &mut Vec<(Rc<RefCell<TcpStream>>, Vec<HttpMessageHandle<Request>>)>,
) {
    let mut index = 0;
    while index < request_list.len() {
        let (stream, stream_requests) = request_list.get_mut(index).unwrap();
        if stream_requests.is_empty() {
            stream_requests.push(HttpMessageHandle::default());
        }

        let stream = stream.clone();

        let mut buf = [0; 8096];
        match stream.borrow_mut().read(&mut buf) {
            Ok(0) => {
                request_list.remove(index);
                continue;
            }
            Ok(size) => {
                let mut packet = &buf[..size];
                loop {
                    let request = stream_requests.last_mut().unwrap();
                    let result = process_http_receivable(packet, request);

                    if let HttpMessage::Ready(_) = request.message {
                        stream_requests.push(HttpMessageHandle::default());
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
    requests: &mut Vec<(Rc<RefCell<TcpStream>>, Vec<HttpMessageHandle<Request>>)>,
    responses: &mut Vec<ResponseStreamHandle>,
    proxy_rules: &ProxyRules,
) {
    for (client_stream, request_list) in requests {
        request_list.retain_mut(|request_element| {
            if let HttpMessage::Ready(ref mut request) = request_element.message {
               
                // TODO handle this..
                proxy_rewrite_request(request, proxy_rules);
                let host = request
                    .get_headers()
                    .get("Host")
                    .expect("No Host in request");

                if request.get_http_version() == &HttpVersion::HTTPv1_1 {
                    // Try find existing TcpStream to the server from the same client stream.
                    if let Some(position) = responses.iter().position(|x| {
                        std::rc::Weak::ptr_eq(&Rc::downgrade(&client_stream), &x.client)
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
                        client: Rc::downgrade(client_stream),
                        messages: Vec::new(),
                        host: request.get_headers().get("Host").unwrap().clone(),
                    });
                }
                return false;
            }
            // Keep all requests that are not ready to be sent out yet.
            return true;
        })
    }
}
fn process_responses(
    response_list: &mut Vec<ResponseStreamHandle>,
) {
    let mut buf = [0; 8096];
    let mut index = 0;
    while index < response_list.len() {
        let response_stream_handle = response_list.get_mut(index).unwrap();
        if response_stream_handle.messages.is_empty() {
            response_stream_handle.messages.push(HttpMessageHandle::default());
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
                    let result = process_http_receivable(packet, response);
                    if let HttpMessage::Ready(_) = response.message {
                        response_stream_handle.messages.push(HttpMessageHandle::default());
                    }
                    match result {
                        Ok(size) => {
                            if size == packet.len() {
                                break;
                            }
                            packet = &packet[size..];
                        }
                        Err(_) => {
                            if let Some(client) = std::rc::Weak::upgrade(&response_stream_handle.client) {
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
                if let HttpMessage::Ready(ref response) = response_handler.message {
                    let buf: Box<[u8]> = Box::from(response);
                    // TODO handle this..
                    client_stream.borrow_mut().write(&buf[..]);
                    return false;
                }
                // Keep all responses that are not ready to be sent out yet.
                return true;
            }),
            None => response_stream_handle.messages.clear(),
        }
        index += 1;
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
        HttpMessage::New(ref mut buffer) => {
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
    }
}
