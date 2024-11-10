use std::{
    net::TcpStream,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    usize,
};

use http::{ProxyRuleHost, ProxyRules};
use runtime::{acceptor, request_handler};

mod http;
mod runtime;

const WORKER_THREAD_COUNT: usize = 4;
fn main() {
    let proxy_rules: Arc<Mutex<(usize, ProxyRules)>> = Arc::new(Mutex::new((0, Vec::new())));
    proxy_rules.lock().unwrap().1.push(ProxyRuleHost::new(
        "127.0.0.1:8080",
        "homeassistant.local:8123",
    ));

    let mut worker_thread_list: Vec<JoinHandle<()>> = Vec::new();
    let (stream_tx, stream_rx): (mpsc::Sender<TcpStream>, mpsc::Receiver<TcpStream>) =
        mpsc::channel();

    let stream_rx = Arc::new(Mutex::new(stream_rx));
    let listener = thread::spawn(|| {
        acceptor(8080, stream_tx);
    });

    for _ in 0..WORKER_THREAD_COUNT {
        let rules_clone = proxy_rules.clone();
        let stream_rx_clone = stream_rx.clone();
        let join_handle =
            thread::spawn(move || request_handler(rules_clone.clone(), stream_rx_clone.clone()));
        worker_thread_list.push(join_handle);
    }

    listener.join().expect("Listener has panicked");

    while !worker_thread_list.is_empty() {
        worker_thread_list
            .pop()
            .unwrap()
            .join()
            .expect("Failed to join worker handle");
    }
}
