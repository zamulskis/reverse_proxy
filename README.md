# Reverse Proxy

**A simple reverse proxy written in Rust.**

## Features

- **Listen on multiple ports:** Apply different proxy rules depending on the port.
- **Configurable worker pool:** Each port can have a variable number of workers to handle traffic.
- **WebSocket support:** Enabled by default.
- **Backend HTTPS support:** Support HTTPS backends.

## Tech Stack

All the threading and I/O is done with rust std lib.

## Installation & Usage

### Prerequisites

Make sure you have **Rust** and **Cargo** installed. If you don't, you can install them by following the instructions on the official [Rust installation page](https://www.rust-lang.org/tools/install).

### Building

To build the project in **release mode** (for production), run:

```sh
cargo build --release
```
For development mode (to run without building a release), use:
```sh
cargo run
```

### Configuration
The proxy configuration should be placed in the file ./static/reverse_proxy_conf.json.
#### Configuration Breakdown
* listener: The IP address and port the reverse proxy will listen on.
* worker_count: The number of workers to handle requests for this listener.
* backends: A list of backends that proxy_rules could refer to.
    * name: Name of the backend.
    * host: IP/or hostname.
    * hostname: Value that gets put in host header.
    * https: Optional flag to decide if we should use https for backend connections.
* proxy_rules: A list of proxy rules defining how incoming traffic should be forwarded. Each rule consists of:
    * type: The type of proxy rule (e.g., proxy_rule_host).
    * from: The source address (host
) to match.
    * to: The destination address (host
) to forward the traffic to.

#### Example configuration file:
```json
{
    "example_config_1" : {
        "listener": "127.0.0.1:8080",
        "worker_count": 1,
        "backends" : [
            {
                "name": "backend_1",
                "host": "homeassistant.local",
                "hostname": "homeassistant.local",
                "port": 32400
            },
            {
                "name": "backend_2",
                "host": "www.google.com",
                "hostname": "www.google.com",
                "port": 443,
                "https": true 
            }
        ],
        "proxy_rules": [
            {
                "type": "proxy_rule_host",
                "from": "127.0.0.1:8080",
                "to": "backend_1"
            },
            {
                "type": "proxy_rule_host",
                "from": "0.0.0.0:8080",
                "to": "backend_1"
            },
            {
                "type": "proxy_rule_host",
                "from": "localhost:8080",
                "to": "backend_2"
            }
        ]
    },
    "example_config_2" : { 
        "listener": "127.0.0.1:8081",
        "worker_count": 1,
        "backends" : [
            {
                "name": "backend_1",
                "host": "localhost",
                "hostname": "testwebsite.com",
                "port": 8000
            }
        ],
        "proxy_rules": [
            {
                "type": "proxy_rule_host",
                "from": "127.0.0.1:8081",
                "to": "backend_1"
            }
        ]
    }   
}
```
