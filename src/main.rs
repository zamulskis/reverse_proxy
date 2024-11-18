use std::{
    net::TcpStream,
    sync::{atomic::AtomicU64, mpsc, Arc},
    thread::{self, JoinHandle},
};

use config::parse_config_file;
use runtime::{acceptor, worker_loop};

mod config;
mod http;
mod runtime;

fn main() {
    let config = match parse_config_file("./static/reverse_proxy_conf.json") {
        Ok(config) => config,
        Err(e) => {
            println!("{e}");
            return;
        }
    };

    let mut server_list_join_handle: Vec<Vec<JoinHandle<()>>> = Vec::new();
    for server in config {
        let mut worker_thread_list: Vec<JoinHandle<()>> = Vec::new();
        let mut stream_tx_list: Vec<(Arc<AtomicU64>, mpsc::Sender<TcpStream>)> = Vec::new();

        for _ in 0..server.worker_count {
            let worker_active_connections = Arc::new(AtomicU64::new(0));
            let (stream_tx, stream_rx): (mpsc::Sender<TcpStream>, mpsc::Receiver<TcpStream>) =
                mpsc::channel();
            stream_tx_list.push((worker_active_connections.clone(), stream_tx));
            let rules_clone = server.proxy_rules.clone();
            let join_handle =
                thread::spawn(move || worker_loop(rules_clone.clone(), stream_rx, worker_active_connections));
            worker_thread_list.push(join_handle);
        }

        let listener = thread::spawn(move || {
            acceptor(&server.listener_addr, &mut stream_tx_list);
        });
        worker_thread_list.push(listener);

        server_list_join_handle.push(worker_thread_list);
    }

    while let Some(mut server) = server_list_join_handle.pop() {
        while let Some(worker) = server.pop() {
            worker.join().expect("Failed to join worker threads");
        }
    }
}
