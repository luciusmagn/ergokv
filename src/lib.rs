//! # ergokv
//!
//! `ergokv` is a library for easy integration with TiKV, providing derive macros for automatic CRUD operations.
//!
//! ## Usage
//!
//! Add this to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! ergokv = "0.1"
//! ```
//!
//! Then, in your Rust file:
//!
//! ```rust
//! use ergokv::Store;
//! use serde::{Serialize, Deserialize};
//! use uuid::Uuid;
//!
//! #[derive(Store, Serialize, Deserialize)]
//! struct User {
//!     #[key]
//!     id: Uuid,
//!     #[index]
//!     username: String,
//!     email: String,
//! }
//! ```
//!
//! This will generate `load`, `save`, `delete`, `by_username`, `set_username`, and `set_email` methods for `User`.

pub use ergokv_macro::Store;

pub use ciborium;

use tikv_client::TransactionClient;

use std::env;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::Path;
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::Duration;

/// Helper function to connect to a single or multiple TiKV pd-server
pub async fn connect(
    endpoints: Vec<&str>,
) -> Result<tikv_client::TransactionClient, tikv_client::Error> {
    tikv_client::TransactionClient::new(endpoints).await
}

/// A structure storing a local cluster.
///
/// Use this to set up and spawn the minimal TiKV cluster on your machine.
/// Running [`LocalCluster::start()`] will verify if tikv-server and pd-server
/// are available. If so, it will spawn the minimal cluster in a data-dir as
/// specified
///
/// [`LocalCluster`] will automatically pick free ports, meaning that you can
/// have multiple apps running seamlessly at the same time.
///
/// Still, you should probably deploy a proper production cluster for your app
/// in production.
pub struct LocalCluster {
    pd_process: Child,
    tikv_process: Child,
    pd_port: u16,
}

impl LocalCluster {
    fn find_free_port() -> u16 {
        let socket = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
        TcpListener::bind(socket)
            .and_then(|listener| listener.local_addr())
            .map(|addr| addr.port())
            .expect("You have no free ports, fuck off")
    }

    fn generate_service_ports() -> [u16; 4] {
        let mut result = [0; 4];

        result[0] = Self::find_free_port();

        for i in 1..4 {
            result[i] = loop {
                let attempt = Self::find_free_port();

                if !&result[..i].contains(&attempt) {
                    break attempt;
                }
            }
        }

        result
    }

    fn setup_components() -> std::io::Result<()> {
        // Check if components are in PATH first
        if which::which("pd-server").is_ok()
            && which::which("tikv-server").is_ok()
        {
            return Ok(());
        }

        // Install tiup if not present
        if which::which("tiup").is_err() {
            Command::new("sh")
                .args([
                    "-c",
                    "curl --proto=https --tlsv1.2 -sSf https://tiup-mirrors.pingcap.com/install.sh | sh"
                ])
                .status()?;
        }

        let tiup_bin = env::var("HOME")
            .map(|h| Path::new(&h).join(".tiup/bin"))
            .expect("You have no HOME var");
        env::set_var(
            "PATH",
            format!(
                "{}:{}",
                tiup_bin.display(),
                env::var("PATH").unwrap_or_default()
            ),
        );

        // Install components if needed
        Command::new("tiup").args(["install", "pd"]).status()?;
        Command::new("tiup")
            .args(["install", "tikv"])
            .status()?;

        // Link components
        Command::new("tiup").args(["link", "pd"]).status()?;
        Command::new("tiup").args(["link", "tikv"]).status()?;

        Ok(())
    }

    /// Possibly install `tiup` and use it to install `tikv-server` and `pd-server` to start
    /// a minimal cluster and manage it until death.
    ///
    /// The [`Drop`] implementation will take care of shutting down the cluster, making everything
    /// seamless.
    pub fn start<P: AsRef<Path>>(
        data_dir: P,
    ) -> std::io::Result<Self> {
        Self::setup_components()?;

        let data_dir = data_dir.as_ref().to_path_buf();
        let pd_dir = data_dir.join("pd");
        let tikv_dir = data_dir.join("tikv");

        std::fs::create_dir_all(&pd_dir)?;
        std::fs::create_dir_all(&tikv_dir)?;

        let [pd_port, pd_peer_port, tikv_port, tikv_status_port] =
            Self::generate_service_ports();

        let pd_process = Command::new("pd-server")
            .args([
                "--name=pd1",
                "--data-dir",
                pd_dir.to_str().unwrap(),
                "--client-urls",
                &format!("http://127.0.0.1:{}", pd_port),
                "--peer-urls",
                &format!("http://127.0.0.1:{}", pd_peer_port),
                "--initial-cluster",
                &format!(
                    "pd1=http://127.0.0.1:{}",
                    pd_peer_port
                ),
            ])
            .spawn()?;

        sleep(Duration::from_secs(2));

        let tikv_process = Command::new("tikv-server")
            .args([
                "--pd",
                &format!("127.0.0.1:{}", pd_port),
                "--addr",
                &format!("127.0.0.1:{}", tikv_port),
                "--status-addr",
                &format!("127.0.0.1:{}", tikv_status_port),
                "--data-dir",
                tikv_dir.to_str().unwrap(),
            ])
            .spawn()?;

        sleep(Duration::from_secs(3));

        Ok(Self {
            pd_process,
            tikv_process,
            pd_port,
        })
    }

    /// Get the address of the PD endpoint. Use this if you for some reason
    /// do not want [`LocalCluster::spawn_client()`]
    pub fn pd_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.pd_port)
    }

    /// Spawn a new transactional client
    pub async fn spawn_client(
        &self,
    ) -> tikv_client::Result<TransactionClient> {
        TransactionClient::new(vec![&self.pd_endpoint()]).await
    }
}

impl Drop for LocalCluster {
    fn drop(&mut self) {
        let _ = self.tikv_process.kill();
        let _ = self.pd_process.kill();
    }
}
