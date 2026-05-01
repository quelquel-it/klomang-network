#[tokio::main]
async fn main() {
    env_logger::init();

    match klomang_node::network::initialize_network_stack() {
        Ok((peer_id, swarm_handle)) => {
            println!("Klomang node started successfully!");
            println!("Peer ID: {}", peer_id);
            println!("Peer discovery and routing are running in the background.");

            if let Err(err) = swarm_handle.await {
                eprintln!("Network swarm task ended unexpectedly: {}", err);
                std::process::exit(1);
            }
        }
        Err(err) => {
            eprintln!("Failed to initialize network stack: {}", err);
            std::process::exit(1);
        }
    }
}
