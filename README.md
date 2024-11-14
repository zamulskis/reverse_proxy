# Reverse Proxy

**A simple reverse proxy written in Rust.**

## Features

- **Listen on multiple ports:** Apply different proxy rules depending on the port.
- **Configurable worker pool:** Each port can have a variable number of workers to handle traffic.
- **WebSocket support:** Enabled by default.

## Tech Stack

This project is written in **pure Rust** using only the standard library. The only external crate used is for JSON parsing.

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
    * host: Address it points to.
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
        "worker_count": 5,
        "backends" : [
            {
                "name": "backend_1",
                "host": "homeassistant.local:32400"
            },
            {
                "name": "backend_2",
                "host": "homeassistant.local:8123"
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
                "to": "backend_2"
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
        "worker_count": 5,
        "backends" : [
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
                "from": "0.0.0.0:8081",
                "to": "backend_2"
            },
            {
                "type": "proxy_rule_host",
                "from": "localhost:8081",
                "to": "backend_2"
            }
        ]
    }   
}
```
