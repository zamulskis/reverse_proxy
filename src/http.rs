use std::{collections::HashMap, str::FromStr};

use crate::config::BackendConfig;
use crate::config::ProxyRuleConfig;
use crate::runtime::HttpProxyErr;
use crate::runtime::HttpTransmittable;

pub type Headers = HashMap<Box<str>, Box<str>>;
#[derive(Clone, PartialEq)]
pub enum HttpVersion {
    HTTPv1,
    HTTPv1_1,
}

#[derive(Clone)]
enum HttpMethod {
    GET,
    POST,
}

#[derive(Clone)]
pub struct RequestHeader {
    version: HttpVersion,
    method: HttpMethod,
    uri: Box<str>,
    headers: Headers,
}

#[derive(Clone)]
pub struct ResponseHeader {
    version: HttpVersion,
    status_code: u32,
    msg: Box<str>,
    headers: Headers,
}

#[derive(Clone)]
pub struct Response {
    header: ResponseHeader,
    body: Vec<u8>,
}

#[derive(Clone)]
pub struct Request {
    header: RequestHeader,
    body: Vec<u8>,
}

impl HttpTransmittable for Request {
    fn parse_headers(buf: &[u8]) -> Result<(Request, usize), HttpProxyErr> {
        let end_line_pos = find_in_u8_slice(&buf, b"\r\n");
        if end_line_pos.is_none() {
            return Err(HttpProxyErr::UnexpectedHeaderFormat);
        }
        let end_line_pos = end_line_pos.unwrap();
        let (method, uri, version) = parse_request_line(&buf[..end_line_pos])?;
        let slice_headers = &buf[end_line_pos + 2..];
        let version_len = slice_headers.as_ptr() as usize - buf.as_ptr() as usize;
        let (headers, size) = parse_headers(slice_headers)?;

        // Validate that Host header is in a request
        if headers.get("Host").is_none() {
            return Err(HttpProxyErr::UnexpectedHeaderFormat);
        }

        let header = RequestHeader {
            version,
            method,
            uri,
            headers,
        };
        let request = Request {
            header,
            body: Vec::new(),
        };
        Ok((request, size + version_len))
    }

    fn get_body(&mut self) -> &mut Vec<u8> {
        self.body.as_mut()
    }

    fn get_headers(&self) -> &Headers {
        &self.header.headers
    }

    fn set_headers(&mut self, key: &str, value: &str) {
        self.header.headers.insert(key.into(), value.into());
    }

    fn to_vec_u8(&self) -> Vec<u8> {
        self.into()
    }
}

impl HttpTransmittable for Response {
    fn parse_headers(buf: &[u8]) -> Result<(Response, usize), HttpProxyErr> {
        let end_line_pos = find_in_u8_slice(&buf, b"\r\n");
        if end_line_pos.is_none() {
            return Err(HttpProxyErr::UnexpectedHeaderFormat);
        }
        let end_line_pos = end_line_pos.unwrap();
        let (version, status_code, msg) = parse_status_line(&buf[..end_line_pos])?;
        let slice_headers = &buf[end_line_pos + 2..];
        let version_len = slice_headers.as_ptr() as usize - buf.as_ptr() as usize;
        let (headers, size) = parse_headers(slice_headers)?;
        let header = ResponseHeader {
            version,
            status_code,
            msg,
            headers,
        };
        let response = Response {
            header,
            body: Vec::new(),
        };
        Ok((response, size + version_len))
    }

    fn get_body(&mut self) -> &mut Vec<u8> {
        self.body.as_mut()
    }

    fn get_headers(&self) -> &Headers {
        &self.header.headers
    }

    fn set_headers(&mut self, key: &str, value: &str) {
        self.header.headers.insert(key.into(), value.into());
    }

    fn to_vec_u8(&self) -> Vec<u8> {
        self.into()
    }
}

impl FromStr for HttpVersion {
    type Err = HttpProxyErr;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "HTTP/1.1" {
            return Ok(HttpVersion::HTTPv1_1);
        }
        if s == "HTTP/1.0" {
            return Ok(HttpVersion::HTTPv1);
        }
        return Err(HttpProxyErr::UnsupportedVersion);
    }
}
impl ToString for HttpVersion {
    fn to_string(&self) -> String {
        match self {
            HttpVersion::HTTPv1_1 => return "HTTP/1.1".to_string(),
            HttpVersion::HTTPv1 => return "HTTP/1.0".to_string(),
        }
    }
}

impl FromStr for HttpMethod {
    type Err = HttpProxyErr;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "GET" {
            return Ok(HttpMethod::GET);
        }
        if s == "POST" {
            return Ok(HttpMethod::POST);
        }
        return Err(HttpProxyErr::UnsupportedMethod);
    }
}

impl ToString for HttpMethod {
    fn to_string(&self) -> String {
        match self {
            HttpMethod::GET => return "GET".to_string(),
            HttpMethod::POST => return "POST".to_string(),
        }
    }
}

impl Into<Vec<u8>> for &Request {
    fn into(self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend(self.header.method.to_string().as_bytes());
        buffer.extend(b" ");
        buffer.extend(self.header.uri.as_bytes());
        buffer.extend(b" ");
        buffer.extend(self.header.version.to_string().as_bytes());
        buffer.extend(b"\r\n");
        for (key, value) in self.header.headers.iter() {
            buffer.extend(key.as_bytes());
            buffer.extend(b": ");
            buffer.extend(value.as_bytes());
            buffer.extend(b"\r\n");
        }
        buffer.extend(b"\r\n");
        buffer.extend(self.body.iter());
        return buffer;
    }
}

impl Into<Vec<u8>> for &Response {
    fn into(self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend(self.header.version.to_string().as_bytes());
        buffer.extend(b" ");
        buffer.extend(self.header.status_code.to_string().as_bytes());
        buffer.extend(b" ");
        buffer.extend(self.header.msg.as_bytes());
        buffer.extend(b"\r\n");
        for (key, value) in self.header.headers.iter() {
            buffer.extend(key.as_bytes());
            buffer.extend(b": ");
            buffer.extend(value.as_bytes());
            buffer.extend(b"\r\n");
        }
        buffer.extend(b"\r\n");
        buffer.extend(self.body.iter());
        return buffer;
    }
}
pub fn proxy_rewrite_request<'a>(
    request: &mut Request,
    proxy_rules: &'a Vec<ProxyRuleConfig>,
) -> Result<&'a BackendConfig, HttpProxyErr> {
    for rule in proxy_rules.iter() {
        if rule.matches(&request) {
            rule.rewrite(request);
            return Ok(rule.backend());
        }
    }
    return Err(HttpProxyErr::ProxyRuleNotFound);
}


fn find_in_u8_slice(slice: &[u8], target: &[u8]) -> Option<usize> {
    for window in slice.windows(target.len()) {
        if window == target {
            return Some(window.as_ptr() as usize - slice.as_ptr() as usize);
        }
    }
    return None;
}

fn parse_header_key_value(buf: &[u8]) -> Result<(&str, &str), HttpProxyErr> {
    match find_in_u8_slice(&buf, b":") {
        Some(key_end_pos) => {
            let key = &buf[..key_end_pos];
            let value = &buf[key_end_pos + 1..];

            let key_str = std::str::from_utf8(key);
            let value_str = std::str::from_utf8(value);

            if key_str.is_err() || value_str.is_err() {
                return Err(HttpProxyErr::UnexpectedHeaderFormat);
            }

            let key_str = key_str.unwrap();
            let value_str = value_str.unwrap().trim_start();

            return Ok((key_str, value_str));
        }
        None => {
            return Err(HttpProxyErr::UnexpectedHeaderFormat);
        }
    }
}

fn parse_request_line(buf: &[u8]) -> Result<(HttpMethod, Box<str>, HttpVersion), HttpProxyErr> {
    match std::str::from_utf8(buf) {
        Ok(line) => {
            let mut parts = line.split(' ');

            let method = parts.next();
            let uri = parts.next();
            let version = parts.next();

            if method.is_none() || uri.is_none() || version.is_none() {
                return Err(HttpProxyErr::UnexpectedHeaderFormat);
            }
            let method = HttpMethod::from_str(method.unwrap())?;
            let uri = uri.unwrap();
            let version = HttpVersion::from_str(version.unwrap())?;

            return Ok((method, uri.into(), version));
        }
        Err(_) => return Err(HttpProxyErr::UnexpectedHeaderFormat),
    }
}

fn parse_status_line(buf: &[u8]) -> Result<(HttpVersion, u32, Box<str>), HttpProxyErr> {
    match std::str::from_utf8(buf) {
        Ok(line) => {
            // println!("response line {line}");
            let mut parts = line.split(' ');

            let version = parts.next();
            let status_code = parts.next();
            let msg = parts.next();

            if status_code.is_none() || version.is_none() {
                return Err(HttpProxyErr::UnexpectedHeaderFormat);
            }

            let version = HttpVersion::from_str(version.unwrap())?;
            match status_code.unwrap().parse::<u32>() {
                Ok(status_code) => Ok((version, status_code, msg.unwrap_or("").into())),
                Err(_) => Err(HttpProxyErr::UnexpectedHeaderFormat),
            }
        }
        Err(_) => return Err(HttpProxyErr::UnexpectedHeaderFormat),
    }
}

fn parse_headers(buf: &[u8]) -> Result<(HashMap<Box<str>, Box<str>>, usize), HttpProxyErr> {
    let mut headers: Headers = HashMap::new();
    let mut slice = &buf[..];
    let mut parsing = true;
    while parsing {
        let end_line_pos = find_in_u8_slice(&slice, b"\r\n");
        if slice.len() == 0 {
            return Err(HttpProxyErr::MaxHeaderSizeExceeded);
        }
        match end_line_pos {
            Some(end_line_pos) => {
                if end_line_pos == 0 {
                    slice = &slice[end_line_pos + 2..];
                    parsing = false;
                    continue;
                }
                let (key, value) = parse_header_key_value(&slice[..end_line_pos])?;
                headers.insert(key.into(), value.into());
                slice = &slice[end_line_pos + 2..];
            }
            None => parsing = false,
        }
    }

    if let Some(content_length) = headers.get("Content-Length") {
        if content_length.parse::<usize>().is_err() {
            return Err(HttpProxyErr::UnexpectedHeaderFormat);
        }
    }

    Ok((headers, slice.as_ptr() as usize - buf.as_ptr() as usize))
}

pub fn get_content_length(headers: &Headers) -> usize {
    return headers
        .get("Content-Length")
        .unwrap_or(&"0".to_string().into_boxed_str())
        .parse::<usize>()
        .expect("Content-Length incorrect format");
}
