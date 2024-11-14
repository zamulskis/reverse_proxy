use core::fmt;
use std::{
    fs, io,
    net::{AddrParseError, SocketAddr},
    sync::Arc,
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
    to: BackendConfig,
}

impl ProxyRuleHost {
    pub fn new(from: &str, backend: BackendConfig) -> Arc<Self> {
        Arc::new(ProxyRuleHost {
            from: from.into(),
            to: backend,
        })
    }

    fn parse(
        value: &json::object::Object,
        backends: &Vec<BackendConfig>,
    ) -> Result<Self, ProxyConfigError> {
        let from: &String = match value["from"] {
            JsonValue::String(ref from) => from,
            JsonValue::Short(ref from) => &from.to_string(),
            JsonValue::Null => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule".into(),
                    "must have [from] paramter".into(),
                ))
            }

            _ => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->from".into(),
                    "paramter must be a String".into(),
                ))
            }
        };
        let to: &String = match value["to"] {
            JsonValue::String(ref to) => to,
            JsonValue::Short(ref to) => &to.to_string(),
            JsonValue::Null => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule".into(),
                    "proxy_rule must have [to] paramter".into(),
                ))
            }

            _ => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->to".into(),
                    "paramter must be a String".into(),
                ))
            }
        };

        let backend = match backends.iter().find(|&x| &x.name.to_string() == to) {
            Some(backend) => backend,
            None => {
                return Err(ProxyConfigError::ConfigParamInvalid(
                    "proxy_rule->to".into(),
                    "Must be a valid backend name".into(),
                ))
            }
        }
        .clone();
        return Ok(ProxyRuleHost {
            from: from.clone().into(),
            to: backend,
        });
    }
}

impl ProxyRule for ProxyRuleHost {
    fn matches(&self, request: &Request) -> bool {
        return request.get_headers().get("Host").unwrap() == &self.from;
    }

    fn rewrite(&self, request: &mut Request) {
        request.set_headers("Host", &self.to.host);
    }
}

#[derive(Clone)]
pub struct ProxyRuleConfig {
    rule: Arc<dyn ProxyRule + Sync + Send>,
}
impl ProxyRuleConfig {
    pub fn matches(&self, request: &Request) -> bool {
        self.rule.matches(request)
    }
    pub fn rewrite(&self, request: &mut Request) {
        self.rule.rewrite(request);
    }
}
#[derive(Clone, PartialEq)]
pub struct BackendConfig {
    name: Box<str>,
    host: Box<str>,
}

pub struct ServerConfig {
    pub server_name: Box<str>,
    pub listener_addr: SocketAddr,
    pub worker_count: usize,
    pub proxy_rules: Vec<ProxyRuleConfig>,
    pub backends: Vec<BackendConfig>,
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
                let server_object = parse_server_object(server_name, server_config)?;
                result.push(server_object);
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
                        "parameter has to be a String ".into(),
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
                        "paramter has to be a Number".into(),
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

            let backends = match server_data["backends"] {
                JsonValue::Array(ref backends) => backends,
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "backens".into(),
                        format!("paramter has to be an Array").into(),
                    ))
                }
            };

            let backends = parse_backends(&backends)?;
            Ok(ServerConfig {
                server_name: server_name.into(),
                listener_addr,
                worker_count,
                proxy_rules: parse_proxy_rules(&proxy_rules, &backends)?,
                backends,
            })
        }
        _ => Err(ProxyConfigError::ServerConfigIsNotAnObject(
            server_name.into(),
        )),
    }
}

fn parse_backends(backends: &Vec<JsonValue>) -> Result<Vec<BackendConfig>, ProxyConfigError> {
    let mut backend_list: Vec<BackendConfig> = Vec::new();
    for backend in backends {
        let backend = parse_backend_config(&backend)?;
        if backend_list
            .iter()
            .position(|x| &x.name == &backend.name)
            .is_some()
        {
            return Err(ProxyConfigError::ConfigParamInvalid(
                "backends->name".into(),
                "has to be unique in the server scope".into(),
            ));
        }
        backend_list.push(backend);
    }
    Ok(backend_list)
}

fn parse_backend_config(object: &JsonValue) -> Result<BackendConfig, ProxyConfigError> {
    match object {
        JsonValue::Object(object) => {
            let host = match object["host"] {
                JsonValue::String(ref host) => host,
                JsonValue::Short(ref host) => &host.to_string(),
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "backends->host".into(),
                        "must be a string".into(),
                    ))
                }
            };
            let name = match object["name"] {
                JsonValue::String(ref name) => name,
                JsonValue::Short(ref name) => &name.to_string(),
                _ => {
                    return Err(ProxyConfigError::ConfigParamInvalid(
                        "backends->name".into(),
                        "must be a string".into(),
                    ))
                }
            };

            Ok(BackendConfig {
                name: name[..].into(),
                host: host[..].into(),
            })
        }
        _ => {
            return Err(ProxyConfigError::BadConfigFormat(
                "backend paramter should be an object".into(),
            ))
        }
    }
}

fn parse_proxy_rules(
    proxy_rules: &Vec<JsonValue>,
    backends: &Vec<BackendConfig>,
) -> Result<Vec<ProxyRuleConfig>, ProxyConfigError> {
    let mut proxy_config: Vec<ProxyRuleConfig> = Vec::new();
    for proxy_rule in proxy_rules {
        proxy_config.push(parse_config_rule(&proxy_rule, backends)?);
    }

    Ok(proxy_config)
}

fn parse_config_rule(
    object: &JsonValue,
    backends: &Vec<BackendConfig>,
) -> Result<ProxyRuleConfig, ProxyConfigError> {
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
                return Ok(ProxyRuleConfig {
                    rule: Arc::new(ProxyRuleHost::parse(object, backends)?),
                });
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

#[cfg(test)]
mod tests {
    use std::any::Any;

    use crate::runtime::HttpProxyErr;

    use super::*;

    #[test]
    fn test_json_parse_config_rule() {
        let backend_intput = r#"
        [
            {
                "name": "backend_1",
                "host": "localhost:8000"
            },
            {
                "name": "backend_2",
                "host": "localhost:8081"
            }
        ]
        "#;

        let json_input = json::parse(&backend_intput).expect("Failed to parse json");
        assert!(json_input.is_array());
        let json_input = match json_input {
            JsonValue::Array(array) => array,
            _ => panic!("Backends should be json array"),
        };

        let backends = parse_backends(&json_input);

        let input = r#"{
                "type": "proxy_rule_host",
                "from": "127.0.0.1:8080",
                "to": "backend_2"
            }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        assert!(json_input.is_object());
        let rule = parse_config_rule(&json_input, &backends.unwrap());
        assert!(rule.is_ok());
    }

    #[test]
    fn test_json_parse_proxy_rule_host() {
        let backend_intput = r#"
        [
            {
                "name": "backend_1",
                "host": "localhost:8000"
            },
            {
                "name": "backend_2",
                "host": "localhost:8081"
            }
        ]
        "#;

        let json_input = json::parse(&backend_intput).expect("Failed to parse json");
        assert!(json_input.is_array());
        let json_input = match json_input {
            JsonValue::Array(array) => array,
            _ => panic!("Backends should be json array"),
        };

        let backends = parse_backends(&json_input);


        let input = r#"{
                "type": "proxy_rule_host",
                "from": "127.0.0.1:8080",
                "to": "backend_1"
            }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        let proxy_rule = match json_input {
            JsonValue::Object(ref object) => ProxyRuleHost::parse(object, &backends.unwrap()),
            _ => {
                assert!(false);
                return;
            }
        };
        assert!(proxy_rule.is_ok());
        let proxy_rule = proxy_rule.unwrap();
        assert!(proxy_rule.from == "127.0.0.1:8080".into());
        assert!(proxy_rule.to.host == "localhost:8000".into());
    }

    #[test]
    fn test_json_parse_server() {
        let input = r#"
        {
            "test" : {
                "listener": "127.0.0.1:8081",
                "worker_count": 5,
                "backends": [
                    {
                        "name": "backend_1",
                        "host": "localhost:8000"
                    },
                    {
                        "name": "backend_2",
                        "host": "localhost:8081"
                    }
                ],
                "proxy_rules": [
                    {
                        "type": "proxy_rule_host",
                        "from": "127.0.0.1:8081",
                        "to": "backend_1"
                    },
                    {
                        "type": "proxy_rule_host",
                        "from": "localhost:8081",
                        "to": "backend_2"
                    }
                ]
            }
        }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        match parse_server_object("test", &json_input["test"]) {
            Ok(server_config) => {
                assert!(server_config.server_name == "test".into());
                assert!(server_config.listener_addr == "127.0.0.1:8081".parse().unwrap());
                assert!(server_config.worker_count == 5);
                assert!(server_config.proxy_rules.len() == 2);
                assert!(server_config.backends.len() == 2);
            }
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn test_json_parse_config() {
        let input = r#"
        {
            "test" : {
                "listener": "127.0.0.1:8081",
                "worker_count": 5,
                "backends" : [
                    {
                        "name": "backend_1",
                        "host": "0.0.0.0:80"
                    }
                ],
                "proxy_rules": [
                    {
                        "type": "proxy_rule_host",
                        "from": "127.0.0.1:8081",
                        "to": "backend_1"
                    }
                ]
            },
            "test2" : {
                "listener": "127.0.0.1:8081",
                "worker_count": 2,
                "backends" : [
                    {
                        "name": "backend_1",
                        "host": "0.0.0.0:80"
                    },
                    {
                        "name": "backend_2",
                        "host": "0.0.0.0:80"
                    },
                    {
                        "name": "backend_3",
                        "host": "0.0.0.0:80"
                    }
                ],

                "proxy_rules": [
                    {
                        "type": "proxy_rule_host",
                        "from": "127.0.0.1:8081",
                        "to": "backend_1"
                    },
                    {
                        "type": "proxy_rule_host",
                        "from": "localhost:8081",
                        "to": "backend_3"
                    }
                ]
            }
        }"#;

        let json_input = json::parse(input).expect("Failed to parse json");

        match parse_config_json(&json_input) {
            Ok(config) => {
                assert!(config.len() == 2);
                assert!(config.first().unwrap().worker_count == 5);
                assert!(config.first().unwrap().proxy_rules.len() == 1);
                assert!(config.first().unwrap().server_name == "test".into());

                assert!(config.last().unwrap().server_name == "test2".into());
                assert!(config.last().unwrap().worker_count == 2);
                assert!(config.last().unwrap().proxy_rules.len() == 2);
            }
            Err(e) => panic!("{e}"),
        }
    }
    #[test]
    fn test_json_parse_proxy_rule_host_failed() {
    let backend_intput = r#"
        [
            {
                "name": "backend_1",
                "host": "localhost:8000"
            },
            {
                "name": "backend_2",
                "host": "localhost:8081"
            }
        ]
        "#;

        let json_input = json::parse(&backend_intput).expect("Failed to parse json");
        assert!(json_input.is_array());
        let json_input = match json_input {
            JsonValue::Array(array) => array,
            _ => panic!("Backends should be json array"),
        };

        let backends = parse_backends(&json_input).unwrap();


        // Fail because from not present
        let input = r#"{
                "type": "proxy_rule_host",
                "fro": "127.0.0.1:8080",
                "to": "backend_1"
            }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        let proxy_rule = match json_input {
            JsonValue::Object(ref object) => ProxyRuleHost::parse(object, &backends),
            _ => {
                assert!(false);
                return;
            }
        };
        assert!(proxy_rule.is_err());

        match proxy_rule.err().unwrap() {
            ProxyConfigError::ConfigParamInvalid(param_name, _) => {
                assert!(param_name == "proxy_rule".into());
            }
            _ => panic!("Bad error type"),
        }

        // Fail because to is not present
        let input = r#"{
                "type": "proxy_rule_host",
                "from": "127.0.0.1:8080",
                "t": "backend_1" }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        let proxy_rule = match json_input {
            JsonValue::Object(ref object) => ProxyRuleHost::parse(object, &backends),
            _ => {
                assert!(false);
                return;
            }
        };
        assert!(proxy_rule.is_err());
        match proxy_rule.err().unwrap() {
            ProxyConfigError::ConfigParamInvalid(param_name, _) => {
                assert!(param_name == "proxy_rule".into());
            }
            _ => panic!("Bad error type"),
        }

        // Fail because from is not a string
        let input = r#"{
                "type": "proxy_rule_host",
                "from": 5,
                "to": "backend_2"
            }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        let proxy_rule = match json_input {
            JsonValue::Object(ref object) => ProxyRuleHost::parse(object, &backends),
            _ => {
                assert!(false);
                return;
            }
        };
        assert!(proxy_rule.is_err());
        match proxy_rule.err().unwrap() {
            ProxyConfigError::ConfigParamInvalid(param_name, _) => {
                assert!(param_name == "proxy_rule->from".into());
            }
            _ => panic!("Bad error type"),
        }

        // Fail because not pointing to a backend
        let input = r#"{
                "type": "proxy_rule_host",
                "from": "0.0.0.0:8080",
                "to": "backend_21"
            }"#;

        let json_input = json::parse(input).expect("Failed to parse json");
        let proxy_rule = match json_input {
            JsonValue::Object(ref object) => ProxyRuleHost::parse(object, &backends),
            _ => {
                assert!(false);
                return;
            }
        };
        assert!(proxy_rule.is_err());
        match proxy_rule.err().unwrap() {
            ProxyConfigError::ConfigParamInvalid(param_name, _) => {
                assert!(param_name == "proxy_rule->to".into());
            }
            _ => panic!("Bad error type"),
        }
    }

    #[test]
    fn test_json_parse_backends() {
        let input = r#"
        [
            {
                "name": "backend_1",
                "host": "localhost:8000"
            },
            {
                "name": "backend_2",
                "host": "localhost:8081"
            }
        ]
        "#;

        let json_input = json::parse(input).expect("Failed to parse json");
        assert!(json_input.is_array());
        let json_input = match json_input {
            JsonValue::Array(array) => array,
            _ => panic!("Backends should be json array"),
        };

        let parsed = parse_backends(&json_input);
        assert!(parsed.is_ok());
        let backends = parsed.unwrap();
        let backend_1 = backends.get(0).unwrap();
        let backend_2 = backends.get(1).unwrap();

        assert!(backend_1.name == "backend_1".into());
        assert!(backend_2.name == "backend_2".into());
        assert!(backend_2.host == "localhost:8081".into());
    }
}
