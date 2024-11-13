use std::{
    net::TcpStream,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
};

use config::parse_config_file;
use runtime::{acceptor, request_handler};

mod config;
mod http;
mod runtime;

fn main() {
    let config =
        match parse_config_file("./static/reverse_proxy_conf.json") {
            Ok(config) => config,
            Err(e) => {
                println!("{e}");
                return;
            }
        };

    let mut server_list_join_handle: Vec<Vec<JoinHandle<()>>> = Vec::new();
    for server in config {
        let mut worker_thread_list: Vec<JoinHandle<()>> = Vec::new();
        let (stream_tx, stream_rx): (mpsc::Sender<TcpStream>, mpsc::Receiver<TcpStream>) =
            mpsc::channel();

        let stream_rx = Arc::new(Mutex::new(stream_rx));
        let listener = thread::spawn(move || {
            acceptor(&server.listener_addr, stream_tx);
        });
        worker_thread_list.push(listener);

        for _ in 0..server.worker_count {
            let rules_clone = server.proxy_rules.clone();
            let stream_rx_clone = stream_rx.clone();
            let join_handle = thread::spawn(move || {
                request_handler(rules_clone.clone(), stream_rx_clone.clone())
            });
            worker_thread_list.push(join_handle);
        }
        server_list_join_handle.push(worker_thread_list);
    }

    while let Some(mut server) = server_list_join_handle.pop() {
        while let Some(worker) = server.pop() {
            worker.join().expect("Failed to join worker threads");
        }
    } 
}
