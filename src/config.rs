use core::fmt;
use std::{
    fs, io,
    net::{AddrParseError, SocketAddr},
    sync::{Arc, Mutex},
    usize,
};

use json::JsonValue;

use crate::{http::Request, runtime::HttpReceivable};

#[derive(Debug)]
pub enum ProxyConfigError {
    FailedToReadFile(io::Error),
    FailedToParseConfig(json::Error),
    BadConfigFormat(Box<str>),
    ServerConfigIsNotAnObject(Box<str>),
    ConfigParamInvalid(Box<str>, Box<str>),
    InvalidListnerAddr(AddrParseError),
}

impl From<io::Error> for ProxyConfigError {
    fn from(value: io::Error) -> Self {
        return Self::FailedToReadFile(value);
    }
}

impl From<json::Error> for ProxyConfigError {
    fn from(value: json::Error) -> Self {
        return Self::FailedToParseConfig(value);
    }
}

impl fmt::Display for ProxyConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            ProxyConfigError::FailedToReadFile(error) => {
                write!(f, "Failed to read config file\n{error}")
            }

            ProxyConfigError::FailedToParseConfig(error) => {
                write!(f, "Failed to parse json\n{error}")
            }

            ProxyConfigError::BadConfigFormat(error) => {
                write!(f, "Config file format incorrect \n{error}")
            }

            ProxyConfigError::ServerConfigIsNotAnObject(server_name) => write!(
                f,
                "Server config is not an object. ServerName - {server_name}"
            ),

            ProxyConfigError::ConfigParamInvalid(parameter, error) => {
                write!(f, "Server config paramter:[{parameter}] Error:[{error}]")
            }
            ProxyConfigError::InvalidListnerAddr(error) => {
                write!(f, "Invalid SocketAddr in listener\n {error}")
            }
        }
    }
}

pub trait ProxyRule {
    fn matches(&self, request: &Request) -> bool;
    fn rewrite(&self, request: &mut Request);
}

pub struct ProxyRuleHost {
    from: Box<str>,
    to: Box<str>,
}

impl ProxyRuleHost {
    pub fn new(from: &str, to: &str) -> Arc<Self> {
        Arc::new(ProxyRuleHost {
            from: from.into(),
            to: to.into(),
        })
    }
}

impl TryFrom<&json::object::Object> for ProxyRuleHost {
    type Error = ProxyConfigError;

    fn try_from(value: &json::object::Object) -> Result<Self, Self::Error> {
        let from: &String = match value["from"] {
            JsonValue::String(ref from) => from,
            JsonValue::Short(ref from) => &from.to_string(),
            JsonValue::Null => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->from".into(),
                    "proxy_rule_host must have [from] paramter".into(),
                ))
            }

            _ => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->from".into(),
                    "proxy_rule_host [from] paramter must be a String".into(),
                ))
            }
        };
        let to: &String = match value["to"] {
            JsonValue::String(ref to) => to,
            JsonValue::Short(ref to) => &to.to_string(),
            JsonValue::Null => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->to".into(),
                    "proxy_rule_host must have [to] paramter".into(),
                ))
            }

            _ => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->to".into(),
                    "proxy_rule_host [to] paramter must be a String".into(),
                ))
            }
        };
        return Ok(ProxyRuleHost {
            from: from.clone().into(),
            to: to.clone().into(),
        });
    }
}

impl ProxyRule for ProxyRuleHost {
    fn matches(&self, request: &Request) -> bool {
        return request.get_headers().get("Host").unwrap() == &self.from;
    }

    fn rewrite(&self, request: &mut Request) {
        request.set_headers("Host", &self.to);
    }
}

// pub type ProxyRules = Vec<Arc<ProxyRule + Send + Sync>>;
//
pub struct ProxyConfig {
    pub rules: Vec<Box<dyn ProxyRule + Sync + Send>>,
}

impl ProxyConfig {
    fn new() -> Self {
        return ProxyConfig { rules: Vec::new() };
    }
}

pub struct ServerConfig {
    pub server_name: Box<str>,
    pub listener_addr: SocketAddr,
    pub worker_count: usize,
    pub proxy_rules: Arc<Mutex<ProxyConfig>>,
}

pub fn parse_config_file(path: &str) -> Result<Vec<ServerConfig>, ProxyConfigError> {
    let file_contents = fs::read_to_string(path)?;
    let config_json = json::parse(&file_contents)?;
    return parse_config_json(&config_json);
}

fn parse_config_json(config_json: &JsonValue) -> Result<Vec<ServerConfig>, ProxyConfigError> {
    let mut result: Vec<ServerConfig> = Vec::new();
    match config_json {
        JsonValue::Object(ref config) => {
            for (server_name, server_config) in config.iter() {
                result.push(parse_server_object(server_name, server_config)?);
            }
        }
        _ => {
            return Err(ProxyConfigError::BadConfigFormat(
                "Expected json Object in {config_json}".into(),
            ))
        }
    }
    return Ok(result);
}

fn parse_server_object(
    server_name: &str,
    server_config: &JsonValue,
) -> Result<ServerConfig, ProxyConfigError> {
    match server_config {
        JsonValue::Object(server_data) => {
            let listener = match server_data["listener"] {
                JsonValue::String(ref listener) => listener,
                JsonValue::Short(ref listener) => &listener.to_string(),
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "listener".into(),
                        "parameter should be a String".into(),
                    ))
                }
            };

            let listener_addr: SocketAddr = match listener.parse() {
                Ok(addr) => addr,
                Err(error) => return Err(ProxyConfigError::InvalidListnerAddr(error)),
            };

            let worker_count: f64 = match server_data["worker_count"] {
                JsonValue::Number(number) => number.into(),
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "worker_count".into(),
                        "paramter has be a Number".into(),
                    ))
                }
            };

            let worker_count: usize = if worker_count >= 0.0 && worker_count <= usize::MAX as f64 {
                worker_count as usize
            } else {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "worker_count".into(),
                    format!("parameter has to be > 0 and < {}", usize::MAX).into(),
                ));
            };

            let proxy_rules = match server_data["proxy_rules"] {
                JsonValue::Array(ref proxy_rules) => proxy_rules,
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "proxy_rules".into(),
                        format!("paramter has to be an Array").into(),
                    ))
                }
            };

            Ok(ServerConfig {
                server_name: server_name.into(),
                listener_addr,
                worker_count,
                proxy_rules: parse_proxy_rules(&proxy_rules)?,
            })
        }
        _ => Err(ProxyConfigError::ServerConfigIsNotAnObject(
            server_name.into(),
        )),
    }
}

fn parse_proxy_rules(
    proxy_rules: &Vec<JsonValue>,
) -> Result<Arc<Mutex<ProxyConfig>>, ProxyConfigError> {
    let mut proxy_config = ProxyConfig::new();
    for proxy_rule in proxy_rules {
        proxy_config.rules.push(parse_config_rule(&proxy_rule)?);
    }

    Ok(Arc::new(Mutex::new(proxy_config)))
}

fn parse_config_rule(object: &JsonValue) -> Result<Box<dyn ProxyRule + Send + Sync>, ProxyConfigError> {
    match object {
        JsonValue::Object(object) => {
            let rule_type = match object["type"] {
                JsonValue::String(ref rule_type) => rule_type,
                JsonValue::Short(ref rule_type) => &rule_type.to_string(),
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "proxy_rule->type".into(),
                        "must be a string".into(),
                    ))
                }
            };
            if rule_type == "proxy_rule_host" {
                return Ok(Box::new(ProxyRuleHost::try_from(object)?));
            }
            Err(ProxyConfigError::ConfigParamInvalid(
                "proxy_rule->type".into(),
                "Rule type[{type}] unsupported".into(),
            ))
        }
        _ => {
            return Err(ProxyConfigError::BadConfigFormat(
                "proxy_rule paramter should be an object".into(),
            ))
        }
    }
}
